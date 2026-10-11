//! Map mode (§6.1): top-down camera, pan/zoom, icons with strength bars,
//! battle icons, roads, bridges and cities, selection + right-click
//! orders, path lines, warhead targeting (§6.7), and the egui side panel
//! (unit info and orders, jet reserve, Launch Jet, warheads, event log).

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{egui, EguiContexts};

use crate::config::*;
use crate::flight::Jet;
use crate::game_rules::{CaptureState, EventLog};
use crate::geo;
use crate::ground::{Battles, GroundUnit, Order, Terrain};
use crate::infantry::JoinInfantry;
use crate::infrastructure::{Infra, Site};
use crate::nav::RoadNet;
use crate::pilot_ai::{AiPilot, FriendlyFlight};
use crate::player::{JetReserve, LaunchJet, Player, PlayerStatus};
use crate::state::*;
use crate::warheads::{LaunchWarhead, Silos, Warhead, WarheadTargeting};
use crate::weapons::Missile;
use crate::world::Bridges;

#[derive(Resource)]
pub struct MapCam {
    pub center: Vec2,
    pub height: f32,
}

impl Default for MapCam {
    fn default() -> Self {
        let [x, z] = geo::geo(20.0, 52.5);
        Self {
            center: Vec2::new(x, z),
            height: 40_000.0,
        }
    }
}

#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Selection {
    #[default]
    None,
    Unit(Entity),
    Flight,
}

pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MapCam>()
            .init_resource::<Selection>()
            .add_systems(OnEnter(AppState::Playing), |mut s: ResMut<Selection>, mut c: ResMut<MapCam>| {
                *s = Selection::None;
                *c = MapCam::default();
            })
            .add_systems(OnEnter(ViewMode::Map), enter_map)
            .add_systems(OnExit(ViewMode::Map), exit_map)
            .add_systems(
                Update,
                toggle_map.in_set(GameplaySet).run_if(in_state(PlayState::Running)),
            )
            .add_systems(
                Update,
                map_camera
                    .run_if(in_state(ViewMode::Map))
                    .run_if(debug_cam_off)
                    .in_set(CameraSet::Position),
            )
            .add_systems(
                Update,
                (map_overlay, map_panel, map_input)
                    .chain()
                    .after(CameraSet::Shake)
                    .run_if(in_state(ViewMode::Map)),
            );
    }
}

fn toggle_map(
    keys: Res<ButtonInput<KeyCode>>,
    view: Res<State<ViewMode>>,
    status: Res<PlayerStatus>,
    mut next: ResMut<NextState<ViewMode>>,
) {
    if !keys.just_pressed(KeyCode::KeyM) {
        return;
    }
    match view.get() {
        ViewMode::Flight | ViewMode::Infantry => next.set(ViewMode::Map),
        ViewMode::Map if status.flying => next.set(ViewMode::Flight),
        _ => {}
    }
}

fn enter_map(mut fog: Query<&mut DistanceFog, With<MainCamera>>, player: Query<&Transform, With<Player>>, mut cam: ResMut<MapCam>) {
    for mut f in &mut fog {
        f.falloff = FogFalloff::from_visibility_squared(FOG_VISIBILITY_MAP);
    }
    if let Ok(tf) = player.get_single() {
        cam.center = tf.translation.xz();
    }
}

fn exit_map(mut fog: Query<&mut DistanceFog, With<MainCamera>>) {
    for mut f in &mut fog {
        f.falloff = FogFalloff::from_visibility_squared(FOG_VISIBILITY);
    }
}

fn map_camera(
    time: Res<Time<Real>>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut contexts: EguiContexts,
    mut mc: ResMut<MapCam>,
    mut cam: Query<&mut Transform, With<MainCamera>>,
) {
    let dt = time.delta_secs();
    let over_ui = contexts.ctx_mut().is_pointer_over_area();
    let mut pan = Vec2::ZERO;
    if keys.any_pressed([KeyCode::KeyW, KeyCode::ArrowUp]) {
        pan.y -= 1.0;
    }
    if keys.any_pressed([KeyCode::KeyS, KeyCode::ArrowDown]) {
        pan.y += 1.0;
    }
    if keys.any_pressed([KeyCode::KeyA, KeyCode::ArrowLeft]) {
        pan.x -= 1.0;
    }
    if keys.any_pressed([KeyCode::KeyD, KeyCode::ArrowRight]) {
        pan.x += 1.0;
    }
    let h = mc.height;
    mc.center += pan * h * 0.8 * dt;
    // Middle-drag to pan.
    if mouse.pressed(MouseButton::Middle) {
        mc.center -= motion.delta * h * 0.0013;
    }
    if !over_ui && scroll.delta.y != 0.0 {
        mc.height = (mc.height * 0.88f32.powf(scroll.delta.y.signum() * scroll.delta.y.abs().min(3.0))).clamp(2_500.0, 150_000.0);
    }
    mc.center = mc.center.clamp(Vec2::splat(-WORLD_HALF), Vec2::splat(WORLD_HALF));
    let Ok(mut tf) = cam.get_single_mut() else {
        return;
    };
    *tf = Transform::from_xyz(mc.center.x, mc.height, mc.center.y)
        .looking_at(Vec3::new(mc.center.x, 0.0, mc.center.y), Vec3::NEG_Z);
}

fn side_color(side: Side) -> egui::Color32 {
    match side {
        Side::Nato => egui::Color32::from_rgb(60, 120, 230),
        Side::Enemy => egui::Color32::from_rgb(220, 60, 50),
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn map_overlay(
    mut contexts: EguiContexts,
    time: Res<Time<Real>>,
    cam: Query<(&Camera, &Transform), With<MainCamera>>,
    units: Query<(Entity, &GroundUnit, &Transform)>,
    jets: Query<(Entity, &Jet, &Transform, Option<&AiPilot>, Has<Player>)>,
    missiles: Query<(&Missile, &Transform)>,
    sites: Query<&Site>,
    warheads: Query<&Warhead>,
    infra: Res<Infra>,
    battles: Res<Battles>,
    (bridges, roads, mc): (Res<Bridges>, Res<RoadNet>, Res<MapCam>),
    capture: Res<CaptureState>,
    selection: Res<Selection>,
    friendly: Res<FriendlyFlight>,
    (targeting, windows): (Res<WarheadTargeting>, Query<&Window, With<PrimaryWindow>>),
) {
    let Ok((camera, ctf)) = cam.get_single() else {
        return;
    };
    let gt = GlobalTransform::from(*ctf);
    let to_screen = |p: Vec3| -> Option<egui::Pos2> {
        camera.world_to_viewport(&gt, p).ok().map(|v| egui::pos2(v.x, v.y))
    };
    let ctx = contexts.ctx_mut();
    let painter = ctx.layer_painter(egui::LayerId::background());
    let white = egui::Color32::WHITE;
    let stroke = |w: f32, c: egui::Color32| egui::Stroke::new(w, c);
    let font = egui::FontId::proportional(13.0);
    let px_per_m = {
        let a = to_screen(Vec3::ZERO);
        let b = to_screen(Vec3::X * 1_000.0);
        match (a, b) {
            (Some(a), Some(b)) => (b.x - a.x).abs() / 1_000.0,
            _ => 0.01,
        }
    };

    // Roads, thin and faint; cities when zoomed in.
    let road_col = egui::Color32::from_rgba_unmultiplied(235, 215, 150, 150);
    let step = if mc.height > 60_000.0 { 25 } else { 8 };
    for line in &roads.lines {
        let pts: Vec<egui::Pos2> = line.iter().step_by(step).filter_map(|p| to_screen(Vec3::new(p.x, 0.0, p.y))).collect();
        painter.add(egui::Shape::line(pts, stroke(1.2, road_col)));
    }
    if mc.height < 90_000.0 {
        let small = egui::FontId::proportional(11.0);
        for (name, ll) in geo::CITIES {
            let [x, z] = geo::geo_ll(*ll);
            if let Some(c) = to_screen(Vec3::new(x, 0.0, z)) {
                painter.circle_filled(c, 2.5, egui::Color32::from_gray(230));
                painter.text(c + egui::vec2(4.0, -2.0), egui::Align2::LEFT_BOTTOM, *name, small.clone(), egui::Color32::from_gray(225));
            }
        }
    }

    // Bridges: grey bar, red cross when down.
    for b in &bridges.list {
        let (Some(a), Some(c)) = (to_screen(b.point(0.0)), to_screen(b.point(1.0))) else {
            continue;
        };
        let mid = a + (c - a) * 0.5;
        if b.intact {
            if mc.height < 60_000.0 {
                painter.line_segment([a, c], stroke(4.0, egui::Color32::from_gray(215)));
                if mc.height < 12_000.0 {
                    painter.text(mid + egui::vec2(0.0, 8.0), egui::Align2::CENTER_TOP, &b.name, egui::FontId::proportional(11.0), white);
                }
            }
        } else {
            let r = 6.0;
            painter.line_segment([mid + egui::vec2(-r, -r), mid + egui::vec2(r, r)], stroke(2.5, egui::Color32::RED));
            painter.line_segment([mid + egui::vec2(-r, r), mid + egui::vec2(r, -r)], stroke(2.5, egui::Color32::RED));
        }
    }

    // Airbases and capture zones.
    for (side, base, progress) in [
        (Side::Nato, NATO_BASE, capture.nato_base),
        (Side::Enemy, ENEMY_BASE, capture.enemy_base),
    ] {
        let Some(c) = to_screen(Vec3::new(base.center[0], base.elevation, base.center[1])) else {
            continue;
        };
        let col = side_color(side);
        painter.circle_stroke(c, base.zone_radius * px_per_m, stroke(2.0, col.gamma_multiply(0.8)));
        let half = egui::vec2(base.runway_width.max(400.0) * px_per_m * 0.5, base.runway_length * px_per_m * 0.5);
        painter.rect_filled(egui::Rect::from_center_size(c, half * 2.0), 1.0, egui::Color32::from_gray(40));
        let name = match side {
            Side::Nato => "NATO AIRBASE",
            Side::Enemy => "ENEMY AIRBASE",
        };
        painter.text(c + egui::vec2(0.0, -base.zone_radius * px_per_m - 10.0), egui::Align2::CENTER_BOTTOM, name, font.clone(), col);
        if progress > 0.0 {
            painter.text(
                c + egui::vec2(0.0, base.zone_radius * px_per_m + 6.0),
                egui::Align2::CENTER_TOP,
                format!("CAPTURE {:.0}%", progress / CAPTURE_TIME * 100.0),
                font.clone(),
                egui::Color32::YELLOW,
            );
        }
    }

    // Infrastructure: a square with a letter and an HP bar, crossed out
    // when destroyed.
    for s in &sites {
        let Some(c) = to_screen(s.aim) else {
            continue;
        };
        let col = side_color(s.side);
        let rect = egui::Rect::from_center_size(c, egui::vec2(18.0, 18.0));
        painter.rect_filled(rect, 3.0, if s.alive { egui::Color32::from_gray(35) } else { egui::Color32::from_gray(20) });
        painter.rect_stroke(rect, 3.0, stroke(2.0, col));
        let letter = match s.kind {
            SiteKind::PowerPlant => "P",
            SiteKind::FuelDepot => "F",
            SiteKind::Radar => "R",
        };
        painter.text(c, egui::Align2::CENTER_CENTER, letter, font.clone(), if s.alive { white } else { egui::Color32::from_gray(90) });
        if s.alive {
            let bar = egui::Rect::from_min_size(rect.left_bottom() + egui::vec2(0.0, 3.0), egui::vec2(18.0, 3.0));
            painter.rect_filled(bar, 0.0, egui::Color32::from_gray(30));
            let f = (s.hp / s.stats().hp).clamp(0.0, 1.0);
            painter.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(18.0 * f, 3.0)), 0.0, egui::Color32::from_rgb(230, 200, 60));
        } else {
            let r = 9.0;
            painter.line_segment([c + egui::vec2(-r, -r), c + egui::vec2(r, r)], stroke(2.5, egui::Color32::RED));
            painter.line_segment([c + egui::vec2(-r, r), c + egui::vec2(r, -r)], stroke(2.5, egui::Color32::RED));
        }
    }

    // Without NATO radar (§6.4) enemy jets only show near your own forces.
    let nato_eyes: Vec<Vec3> = units
        .iter()
        .filter(|(_, u, _)| u.side == Side::Nato)
        .map(|(_, _, tf)| tf.translation)
        .chain(jets.iter().filter(|(_, j, ..)| j.side == Side::Nato).map(|(_, _, tf, ..)| tf.translation))
        .collect();
    let radar = infra.nato.radar_up();
    let visible = |p: Vec3| radar || nato_eyes.iter().any(|e| e.xz().distance(p.xz()) < RADAR_OUT_MAP_RANGE);

    // Selected unit's path.
    if let Selection::Unit(sel) = *selection {
        if let Ok((_, u, tf)) = units.get(sel) {
            let mut pts = vec![tf.translation];
            pts.extend(u.path.iter().map(|p| Vec3::new(p.x, tf.translation.y, p.z)));
            let screen: Vec<egui::Pos2> = pts.iter().filter_map(|p| to_screen(*p)).collect();
            painter.add(egui::Shape::line(screen.clone(), stroke(2.0, egui::Color32::YELLOW)));
            if let Some(last) = screen.last() {
                painter.circle_stroke(*last, 6.0, stroke(2.0, egui::Color32::YELLOW));
            }
        }
    }

    // Ground units: NATO-style rectangle, symbol, strength bar.
    for (e, u, tf) in &units {
        let Some(c) = to_screen(tf.translation) else {
            continue;
        };
        let rect = egui::Rect::from_center_size(c, egui::vec2(28.0, 18.0));
        let mut fill = side_color(u.side);
        if u.retreating {
            fill = fill.gamma_multiply(0.45);
        }
        painter.rect_filled(rect, 1.0, fill);
        let selected = *selection == Selection::Unit(e);
        painter.rect_stroke(rect, 1.0, stroke(if selected { 3.0 } else { 1.5 }, if selected { egui::Color32::YELLOW } else { white }));
        let inner = rect.shrink(3.0);
        match u.kind {
            UnitKind::Infantry => {
                painter.line_segment([inner.left_top(), inner.right_bottom()], stroke(1.5, white));
                painter.line_segment([inner.left_bottom(), inner.right_top()], stroke(1.5, white));
            }
            UnitKind::Armor => {
                painter.rect_stroke(inner.shrink2(egui::vec2(2.0, 2.0)), 5.0, stroke(1.5, white));
            }
            UnitKind::AntiAir => {
                let pts: Vec<egui::Pos2> = (0..=10)
                    .map(|i| {
                        let a = std::f32::consts::PI * i as f32 / 10.0;
                        egui::pos2(c.x - a.cos() * 8.0, inner.bottom() - a.sin() * 9.0)
                    })
                    .collect();
                painter.add(egui::Shape::line(pts, stroke(1.5, white)));
            }
        }
        let bar = egui::Rect::from_min_size(rect.left_bottom() + egui::vec2(0.0, 3.0), egui::vec2(28.0, 4.0));
        painter.rect_filled(bar, 0.0, egui::Color32::from_gray(30));
        let s = (u.strength / 100.0).clamp(0.0, 1.0);
        let bar_col = egui::Color32::from_rgb(((1.0 - s) * 255.0) as u8, (s * 220.0) as u8, 40);
        painter.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(28.0 * s, 4.0)), 0.0, bar_col);
        if u.engaged && !u.retreating {
            painter.text(rect.right_top() + egui::vec2(3.0, 0.0), egui::Align2::LEFT_TOP, "!", font.clone(), egui::Color32::YELLOW);
        }
    }

    // Battles: flashing crossed swords.
    let flash = (time.elapsed_secs() * 2.5).fract() < 0.6;
    if flash {
        for b in &battles.0 {
            if let Some(c) = to_screen(*b) {
                let col = egui::Color32::from_rgb(255, 160, 30);
                let r = 9.0;
                painter.line_segment([c + egui::vec2(-r, r), c + egui::vec2(r, -r)], stroke(2.5, col));
                painter.line_segment([c + egui::vec2(-r, -r), c + egui::vec2(r, r)], stroke(2.5, col));
                painter.line_segment([c + egui::vec2(-r * 0.9, r * 0.2), c + egui::vec2(-r * 0.2, r * 0.9)], stroke(2.0, col));
                painter.line_segment([c + egui::vec2(r * 0.9, r * 0.2), c + egui::vec2(r * 0.2, r * 0.9)], stroke(2.0, col));
            }
        }
    }

    // Friendly flight patrol point.
    if let Some(p) = to_screen(friendly.patrol_point) {
        let col = if *selection == Selection::Flight { egui::Color32::YELLOW } else { egui::Color32::from_rgb(120, 180, 255) };
        painter.circle_stroke(p, 7.0, stroke(1.5, col));
        painter.text(p + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, "CAP", font.clone(), col);
    }

    // Jets: small plane icons with heading.
    for (_, jet, tf, ai, is_player) in &jets {
        if jet.side == Side::Enemy && !visible(tf.translation) {
            continue;
        }
        let Some(c) = to_screen(tf.translation) else {
            continue;
        };
        // Strike raids: a dashed red line to the target site.
        if let Some((_, target)) = ai.and_then(|a| a.strike) {
            if let Some(t) = to_screen(target) {
                let shapes = egui::Shape::dashed_line(&[c, t], stroke(1.5, egui::Color32::from_rgb(255, 90, 80)), 6.0, 5.0);
                painter.extend(shapes);
            }
        }
        let f = tf.forward();
        let dir = egui::vec2(f.x, f.z).normalized();
        let perp = egui::vec2(-dir.y, dir.x);
        let col = if is_player {
            egui::Color32::from_rgb(255, 230, 60)
        } else if jet.side == Side::Nato {
            egui::Color32::from_rgb(130, 190, 255)
        } else {
            egui::Color32::from_rgb(255, 90, 80)
        };
        let s = if is_player { 11.0 } else { 9.0 };
        let pts = vec![c + dir * s, c - dir * s * 0.7 + perp * s * 0.7, c - dir * s * 0.35, c - dir * s * 0.7 - perp * s * 0.7];
        painter.add(egui::Shape::convex_polygon(pts, col, stroke(1.0, egui::Color32::BLACK)));
        let selected_flight = *selection == Selection::Flight && ai.is_some_and(|a| a.friendly_flight);
        if selected_flight {
            painter.circle_stroke(c, s + 4.0, stroke(2.0, egui::Color32::YELLOW));
        }
        if is_player {
            painter.text(c + egui::vec2(12.0, -12.0), egui::Align2::LEFT_BOTTOM, "YOU", font.clone(), col);
        }
    }

    for (m, tf) in &missiles {
        if m.side == Side::Enemy && !visible(tf.translation) {
            continue;
        }
        if let Some(c) = to_screen(tf.translation) {
            painter.circle_filled(c, 2.0, side_color(m.side).linear_multiply(1.5));
        }
    }

    // Warheads in flight: track, impact zone and countdown.
    for w in &warheads {
        let col = match w.side {
            Side::Nato => egui::Color32::from_rgb(120, 200, 255),
            Side::Enemy if flash => egui::Color32::from_rgb(255, 60, 40),
            Side::Enemy => egui::Color32::from_rgb(255, 180, 60),
        };
        let s = w.age / w.flight;
        let track: Vec<egui::Pos2> = (0..=24)
            .map(|k| s + (1.0 - s) * k as f32 / 24.0)
            .filter_map(|t| to_screen(w.position(t).with_y(0.0)))
            .collect();
        painter.extend(egui::Shape::dashed_line(&track, stroke(1.5, col), 8.0, 6.0));
        if let Some(c) = to_screen(w.position(s).with_y(0.0)) {
            painter.circle_filled(c, 4.5, col);
        }
        if let Some(t) = to_screen(w.target) {
            let r = (WARHEAD_RADIUS * px_per_m).max(6.0);
            painter.circle_stroke(t, r, stroke(2.0, col));
            painter.text(t + egui::vec2(0.0, -r - 4.0), egui::Align2::CENTER_BOTTOM, format!("IMPACT {:.0}s", w.remaining()), font.clone(), col);
        }
    }

    // Targeting cursor.
    if targeting.0 {
        if let Some(cursor) = windows.get_single().ok().and_then(|w| w.cursor_position()) {
            let c = egui::pos2(cursor.x, cursor.y);
            let col = egui::Color32::from_rgb(255, 150, 40);
            let r = (WARHEAD_RADIUS * px_per_m).max(6.0);
            painter.circle_stroke(c, r, stroke(2.0, col));
            painter.line_segment([c - egui::vec2(r + 6.0, 0.0), c + egui::vec2(r + 6.0, 0.0)], stroke(1.0, col));
            painter.line_segment([c - egui::vec2(0.0, r + 6.0), c + egui::vec2(0.0, r + 6.0)], stroke(1.0, col));
            painter.text(c + egui::vec2(r + 8.0, -r), egui::Align2::LEFT_BOTTOM, "LMB fire warhead · RMB cancel", font.clone(), col);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn map_panel(
    mut contexts: EguiContexts,
    time: Res<Time<Virtual>>,
    reserve: Res<JetReserve>,
    status: Res<PlayerStatus>,
    selection: Res<Selection>,
    mut units: Query<&mut GroundUnit>,
    friendly: Query<&AiPilot>,
    sites: Query<&Site>,
    infra: Res<Infra>,
    bridges: Res<Bridges>,
    capture: Res<CaptureState>,
    log: Res<EventLog>,
    silos: Res<Silos>,
    mut targeting: ResMut<WarheadTargeting>,
    mut launch: EventWriter<LaunchJet>,
    mut join: EventWriter<JoinInfantry>,
) {
    let ctx = contexts.ctx_mut();
    egui::SidePanel::right("map_panel").exact_width(300.0).show(ctx, |ui| {
        ui.heading("Command Map");
        let t = time.elapsed_secs();
        ui.label(format!("Battle time {:02}:{:02}", (t / 60.0) as u32, (t % 60.0) as u32));
        ui.separator();

        ui.label(format!("Jets in reserve: {}", reserve.remaining));
        if status.flying {
            ui.label("You are airborne (autopilot holding). M: back to the jet.");
        } else {
            let can = reserve.remaining > 0;
            if ui.add_enabled(can, egui::Button::new(egui::RichText::new("  Launch Jet  ").size(18.0))).clicked() {
                launch.send(LaunchJet);
            }
            if !can {
                ui.colored_label(egui::Color32::RED, "No jets left.");
            }
        }
        ui.separator();

        match *selection {
            Selection::Unit(e) => {
                if let Ok(mut u) = units.get_mut(e) {
                    ui.strong(&u.name);
                    ui.add(egui::ProgressBar::new(u.strength / 100.0).text(format!("Strength {:.0}", u.strength)));
                    let s = u.stats();
                    ui.label(format!("Attack {}  Defense {}  Speed {:.0} km/h", s.attack, s.defense, s.speed * 3.6));
                    let state = if u.retreating {
                        "Retreating".to_string()
                    } else if u.engaged {
                        "Engaged".to_string()
                    } else {
                        match u.order {
                            Order::Auto => match &u.auto_goal {
                                Some((_, label)) if !u.path.is_empty() => format!("Auto: {label}"),
                                _ => "Auto: looking for a target".to_string(),
                            },
                            Order::Manual { resume } if u.path.is_empty() => format!("Orders done, auto in {resume:.0} s"),
                            Order::Manual { .. } => "Following your orders".to_string(),
                            Order::Hold => "Holding position".to_string(),
                        }
                    };
                    ui.label(format!("Status: {state}"));
                    ui.horizontal(|ui| {
                        if ui.selectable_label(u.order == Order::Auto, "Auto").clicked() {
                            u.order = Order::Auto;
                            u.replan = 0.0;
                        }
                        if ui.selectable_label(u.order == Order::Hold, "Hold here").clicked() {
                            u.order = Order::Hold;
                            u.path.clear();
                        }
                    });
                    ui.small("Right-click the map to send it somewhere (auto resumes 30 s after it arrives).");
                    if u.kind == UnitKind::Infantry
                        && ui.add(egui::Button::new(egui::RichText::new("  Fight with this squad (I)  ").size(16.0))).clicked()
                    {
                        join.send(JoinInfantry(e));
                    }
                } else {
                    ui.label("Selected unit is gone.");
                }
            }
            Selection::Flight => {
                let n = friendly.iter().filter(|p| p.friendly_flight).count();
                ui.strong("Friendly flight (CAP)");
                ui.label(format!("{n} jet(s) airborne"));
                ui.small("Right-click the map to set the patrol point.");
            }
            Selection::None => {
                ui.label("Left-click a blue unit or friendly jet to select it.");
            }
        }
        ui.separator();

        ui.strong("Ballistic warheads");
        ui.label(format!("Ready in your silos: {}   (enemy: {})", silos.nato.stock, silos.enemy.stock));
        if targeting.0 {
            ui.colored_label(egui::Color32::from_rgb(255, 150, 40), "Click the impact point on the map.");
            if ui.button("Cancel").clicked() {
                targeting.0 = false;
            }
        } else {
            let label = if silos.nato.stock > 0 && silos.nato.cooldown > 0.0 {
                format!("  Reloading {:.0} s  ", silos.nato.cooldown)
            } else {
                "  Launch warhead  ".to_string()
            };
            if ui.add_enabled(silos.nato.ready(), egui::Button::new(egui::RichText::new(label).size(16.0))).clicked() {
                targeting.0 = true;
            }
        }
        ui.separator();
        ui.label(format!("Bridges standing: {} of {}", bridges.intact_count(), bridges.list.len()));
        for b in bridges.list.iter().filter(|b| !b.intact).take(4) {
            ui.colored_label(egui::Color32::from_rgb(230, 120, 100), format!("  down: {}", b.name));
        }
        ui.separator();
        ui.strong("Infrastructure");
        egui::Grid::new("infra").num_columns(2).spacing([10.0, 2.0]).show(ui, |ui| {
            for side in [Side::Nato, Side::Enemy] {
                for s in sites.iter().filter(|s| s.side == side) {
                    ui.colored_label(side_color(side), &s.name);
                    if s.alive {
                        ui.label(format!("{:.0}%", s.hp / s.stats().hp * 100.0));
                    } else {
                        ui.colored_label(egui::Color32::RED, "DESTROYED");
                    }
                    ui.end_row();
                }
            }
        });
        let nato = infra.nato;
        if !nato.radar_up() {
            ui.colored_label(egui::Color32::RED, "NATO radar dark: weak AA, enemy jets only seen nearby");
        }
        if !nato.fuel_up() {
            ui.colored_label(egui::Color32::RED, "NATO fuel offline: restocking is slow");
        }
        if !infra.enemy.radar_up() {
            ui.colored_label(egui::Color32::GREEN, "Enemy radar dark: weak AA, blind pilots");
        }
        if !infra.enemy.fuel_up() {
            ui.colored_label(egui::Color32::GREEN, "Enemy fuel offline: fewer jet launches");
        }
        if capture.nato_base > 0.0 {
            ui.colored_label(egui::Color32::RED, format!("NATO base under capture: {:.0}%", capture.nato_base / CAPTURE_TIME * 100.0));
        }
        if capture.enemy_base > 0.0 {
            ui.colored_label(egui::Color32::GREEN, format!("Capturing enemy base: {:.0}%", capture.enemy_base / CAPTURE_TIME * 100.0));
        }
        ui.separator();
        ui.small("LMB select · RMB order · WASD/middle-drag pan · wheel zoom (out to the whole world)");
        ui.separator();
        ui.strong("Event log");
        egui::ScrollArea::vertical().show(ui, |ui| {
            for line in log.lines.iter().rev() {
                ui.label(line);
            }
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn map_input(
    mut contexts: EguiContexts,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cam: Query<(&Camera, &Transform), With<MainCamera>>,
    terrain: Terrain,
    mut targeting: ResMut<WarheadTargeting>,
    mut warheads: EventWriter<LaunchWarhead>,
    mut selection: ResMut<Selection>,
    mut units: Query<(Entity, &mut GroundUnit, &Transform)>,
    mut pilots: Query<(&mut AiPilot, &Transform)>,
    mut friendly: ResMut<FriendlyFlight>,
    mut log: ResMut<EventLog>,
    keys: Res<ButtonInput<KeyCode>>,
    mut join: EventWriter<JoinInfantry>,
) {
    if keys.just_pressed(KeyCode::KeyI) {
        if let Selection::Unit(e) = *selection {
            if units.get(e).is_ok_and(|(_, u, _)| u.kind == UnitKind::Infantry) {
                join.send(JoinInfantry(e));
            }
        }
    }
    if contexts.ctx_mut().is_pointer_over_area() {
        return;
    }
    let (Ok(window), Ok((camera, ctf))) = (windows.get_single(), cam.get_single()) else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let gt = GlobalTransform::from(*ctf);
    let hm = &terrain.hm;

    // Warhead targeting swallows clicks until it fires or is cancelled.
    if targeting.0 {
        if mouse.just_pressed(MouseButton::Right) {
            targeting.0 = false;
        } else if mouse.just_pressed(MouseButton::Left) {
            let point = camera
                .viewport_to_world(&gt, cursor)
                .ok()
                .and_then(|ray| hm.raycast(ray.origin, *ray.direction, 400_000.0));
            if let Some(p) = point {
                warheads.send(LaunchWarhead { side: Side::Nato, target: p });
                targeting.0 = false;
            }
        }
        return;
    }

    if mouse.just_pressed(MouseButton::Left) {
        let mut best: Option<(Selection, f32)> = None;
        for (e, u, tf) in &units {
            if u.side != Side::Nato {
                continue;
            }
            if let Ok(p) = camera.world_to_viewport(&gt, tf.translation) {
                let d = p.distance(cursor);
                if d < 20.0 && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((Selection::Unit(e), d));
                }
            }
        }
        for (ai, tf) in &pilots {
            if !ai.friendly_flight {
                continue;
            }
            if let Ok(p) = camera.world_to_viewport(&gt, tf.translation) {
                let d = p.distance(cursor);
                if d < 20.0 && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((Selection::Flight, d));
                }
            }
        }
        // The CAP marker also selects the flight.
        if let Ok(p) = camera.world_to_viewport(&gt, friendly.patrol_point) {
            let d = p.distance(cursor);
            if d < 14.0 && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((Selection::Flight, d));
            }
        }
        *selection = best.map(|(s, _)| s).unwrap_or(Selection::None);
    }

    if mouse.just_pressed(MouseButton::Right) {
        let Ok(ray) = camera.viewport_to_world(&gt, cursor) else {
            return;
        };
        let Some(point) = hm.raycast(ray.origin, *ray.direction, 400_000.0) else {
            return;
        };
        match *selection {
            Selection::Unit(e) => {
                let Ok((_, mut u, tf)) = units.get_mut(e) else {
                    return;
                };
                if hm.is_water(point.x, point.z) {
                    log.push("Ground units can't go into water.");
                    return;
                }
                match terrain.plan(tf.translation, point, u.kind) {
                    Some(path) => {
                        u.path = path;
                        u.holding = false;
                        u.order = Order::Manual { resume: AUTO_RESUME };
                        u.auto_goal = None;
                    }
                    None => log.push("No route there: water in the way (bridges down?)."),
                }
            }
            Selection::Flight => {
                friendly.patrol_point = Vec3::new(point.x, AI_CRUISE_ALT, point.z);
                for (mut ai, _) in &mut pilots {
                    if ai.friendly_flight {
                        ai.waypoint = Vec3::ZERO;
                    }
                }
                log.push("Friendly flight: new patrol point");
            }
            Selection::None => {}
        }
    }
}
