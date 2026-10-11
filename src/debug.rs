//! Debug overlays and cheats (F-keys):
//!
//! F1 free-fly camera · F2 debug panel (aero sliders, cheats) · F3 FPS ·
//! F4 velocity/lift/missile vectors · F5 infinite fuel · F6 spawn target
//! drone · F7 fuel leak on your jet · F8 god mode · F9 spawn enemy jet ·
//! F10 unit ranges.
//!
//! Automation (for headless-ish checks): WARGAME_AUTOSTART=1 starts a
//! round, WARGAME_SHOT=<file.png> with WARGAME_SHOT_AT=<secs> saves a
//! screenshot and exits, WARGAME_VIEW=map|cockpit|debug picks the view,
//! WARGAME_LOG=1 prints the player's flight state every second (plus FPS
//! and every ground unit's orders), WARGAME_TIMESCALE=<x> speeds up game
//! time (soak tests), WARGAME_WARHEAD=<lon>,<lat> fires a NATO warhead
//! there and films the impact.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy_egui::{egui, EguiContexts};

use crate::config::*;
use crate::flight::{spawn_jet, AeroTuning, FlightState, Jet, JetAssets, Velocity};
use crate::ground::GroundUnit;
use crate::pilot_ai::AiPilot;
use crate::player::{CameraRig, Player};
use crate::state::*;
use crate::weapons::{spawn_drone, JetDamage, Missile, WeaponAssets};

#[derive(Resource, Default)]
pub struct Cheats {
    pub infinite_fuel: bool,
    pub god_mode: bool,
}

#[derive(Resource)]
struct DebugUi {
    panel: bool,
    fps: bool,
    vectors: bool,
    ranges: bool,
}

impl Default for DebugUi {
    fn default() -> Self {
        Self {
            panel: false,
            fps: true,
            vectors: false,
            ranges: false,
        }
    }
}

#[derive(Resource, Default)]
struct FreeCam {
    yaw: f32,
    pitch: f32,
}

#[derive(Resource, Default)]
struct Automation {
    autostart: bool,
    shot: Option<String>,
    shot_at: f32,
    view: Option<String>,
    log: bool,
    fly: Option<String>,
    timescale: Option<f32>,
    elapsed: f32,
    log_timer: f32,
    taken: bool,
    view_set: bool,
    /// WARGAME_WARHEAD=lon,lat: NATO fires a warhead there at the start and
    /// the free camera watches the impact (photographed if WARGAME_SHOT).
    warhead: Option<(f32, f32)>,
    warhead_sent: bool,
}

pub struct DebugToolsPlugin;

impl Plugin for DebugToolsPlugin {
    fn build(&self, app: &mut App) {
        let env = |k: &str| std::env::var(k).ok();
        app.add_plugins(FrameTimeDiagnosticsPlugin)
            .init_resource::<Cheats>()
            .init_resource::<DebugUi>()
            .init_resource::<FreeCam>()
            .insert_resource(Automation {
                autostart: env("WARGAME_AUTOSTART").is_some(),
                shot: env("WARGAME_SHOT"),
                shot_at: env("WARGAME_SHOT_AT").and_then(|s| s.parse().ok()).unwrap_or(5.0),
                view: env("WARGAME_VIEW"),
                log: env("WARGAME_LOG").is_some(),
                fly: env("WARGAME_FLY"),
                timescale: env("WARGAME_TIMESCALE").and_then(|s| s.parse().ok()),
                warhead: env("WARGAME_WARHEAD").and_then(|s| {
                    let (a, b) = s.split_once(',')?;
                    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
                }),
                ..default()
            })
            .add_systems(Update, (debug_keys, fps_overlay, debug_panel, automation, log_fps, log_units))
            .add_systems(Update, auto_warhead.after(CameraSet::Position).before(CameraSet::Shake))
            .add_systems(Update, free_camera.run_if(|d: Res<DebugCamActive>| d.0).in_set(CameraSet::Position))
            .add_systems(Update, (vectors, ranges).after(CameraSet::Shake))
            .add_systems(FixedUpdate, auto_fly.in_set(crate::flight::FlightSet::Control));
    }
}

#[allow(clippy::too_many_arguments)]
fn debug_keys(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    app: Res<State<AppState>>,
    mut debug_cam: ResMut<DebugCamActive>,
    mut ui: ResMut<DebugUi>,
    mut cheats: ResMut<Cheats>,
    mut free: ResMut<FreeCam>,
    cam: Query<&Transform, With<MainCamera>>,
    mut player: Query<(&Transform, &mut JetDamage), With<Player>>,
    jet_assets: Res<JetAssets>,
    weapon_assets: Res<WeaponAssets>,
) {
    if keys.just_pressed(KeyCode::F1) {
        debug_cam.0 = !debug_cam.0;
        if let Ok(tf) = cam.get_single() {
            let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
            free.yaw = yaw;
            free.pitch = pitch;
        }
    }
    if keys.just_pressed(KeyCode::F2) {
        ui.panel = !ui.panel;
    }
    if keys.just_pressed(KeyCode::F3) {
        ui.fps = !ui.fps;
    }
    if keys.just_pressed(KeyCode::F4) {
        ui.vectors = !ui.vectors;
    }
    if keys.just_pressed(KeyCode::F5) {
        cheats.infinite_fuel = !cheats.infinite_fuel;
    }
    if keys.just_pressed(KeyCode::F8) {
        cheats.god_mode = !cheats.god_mode;
    }
    if keys.just_pressed(KeyCode::F10) {
        ui.ranges = !ui.ranges;
    }
    if *app.get() != AppState::Playing {
        return;
    }
    let Ok((ptf, mut dmg)) = player.get_single_mut() else {
        return;
    };
    let ahead = ptf.translation + ptf.forward().as_vec3() * 1_500.0;
    if keys.just_pressed(KeyCode::F6) {
        let pos = Vec3::new(ahead.x, ahead.y.max(ptf.translation.y + 50.0), ahead.z);
        spawn_drone(&mut commands, &weapon_assets, pos);
    }
    if keys.just_pressed(KeyCode::F7) {
        dmg.fuel_leak += DEBUG_LEAK_RATE;
    }
    if keys.just_pressed(KeyCode::F9) {
        let pos = ptf.translation + ptf.forward().as_vec3() * 3_000.0 + Vec3::Y * 200.0;
        let rot = Transform::from_translation(pos).looking_at(ptf.translation, Vec3::Y).rotation;
        let e = spawn_jet(&mut commands, &jet_assets, Side::Enemy, pos, rot, AI_SPAWN_SPEED, false);
        commands.entity(e).insert(AiPilot::new(false));
    }
}

/// WARGAME_FLY=takeoff|pull|shoot: scripted stick for testing in the real
/// app (takeoff roll, rotate, gear up, climb; `pull` then holds a max-G
/// pull to check blackout; `shoot` levels off, spawns a drone ahead and
/// fires a missile once locked).
#[allow(clippy::too_many_arguments)]
fn auto_fly(
    mut commands: Commands,
    auto: Res<Automation>,
    hm: Res<crate::world::Heightmap>,
    weapon_assets: Res<WeaponAssets>,
    mut q: Query<(&Transform, &Velocity, &FlightState, &crate::weapons::LockState, &mut crate::flight::ControlInput), With<Player>>,
    mut drone_spawned: Local<bool>,
    mut fired: Local<bool>,
) {
    let Some(mode) = auto.fly.as_deref() else {
        return;
    };
    let Ok((tf, v, st, lock, mut input)) = q.get_single_mut() else {
        return;
    };
    let agl = tf.translation.y - hm.surface(tf.translation.x, tf.translation.z);
    let speed = v.0.length();
    input.throttle = 1.0;
    input.roll = 0.0;
    input.yaw = 0.0;
    // Ignore stray keyboard/mouse input during scripted runs.
    input.fire_gun = false;
    input.fire_missile = false;
    input.flare = false;
    let (_, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
    if st.on_ground {
        input.pitch = if speed > 72.0 { 0.6 } else { 0.0 };
    } else if mode == "pull" && agl > 1_200.0 && speed > 200.0 {
        input.pitch = 1.0;
    } else if mode == "shoot" && agl > 500.0 {
        input.throttle = 0.8;
        input.pitch = ((0.0 - pitch) * 3.0).clamp(-1.0, 1.0);
        if !*drone_spawned && pitch.abs() < 0.05 {
            *drone_spawned = true;
            let f = tf.forward().as_vec3();
            spawn_drone(&mut commands, &weapon_assets, tf.translation + Vec3::new(f.x, 0.0, f.z).normalize() * 2_500.0);
        }
        if lock.locked && !*fired {
            *fired = true;
            input.fire_missile = true;
        }
    } else {
        // Hold ~12 degree climb.
        input.pitch = ((0.2 - pitch) * 3.0).clamp(-1.0, 1.0);
        if st.gear_down && agl > 60.0 {
            input.toggle_gear = true;
        }
    }
}

fn free_camera(
    time: Res<Time<Real>>,
    keys: Res<ButtonInput<KeyCode>>,
    motion: Res<AccumulatedMouseMotion>,
    mut free: ResMut<FreeCam>,
    mut cam: Query<&mut Transform, With<MainCamera>>,
) {
    let Ok(mut tf) = cam.get_single_mut() else {
        return;
    };
    let dt = time.delta_secs();
    free.yaw -= motion.delta.x * 0.003;
    free.pitch = (free.pitch - motion.delta.y * 0.003).clamp(-1.55, 1.55);
    tf.rotation = Quat::from_euler(EulerRot::YXZ, free.yaw, free.pitch, 0.0);
    let mut v = Vec3::ZERO;
    let f = tf.forward().as_vec3();
    let r = tf.right().as_vec3();
    for (k, d) in [
        (KeyCode::KeyW, f),
        (KeyCode::KeyS, -f),
        (KeyCode::KeyD, r),
        (KeyCode::KeyA, -r),
        (KeyCode::KeyE, Vec3::Y),
        (KeyCode::KeyQ, -Vec3::Y),
    ] {
        if keys.pressed(k) {
            v += d;
        }
    }
    let speed = if keys.pressed(KeyCode::ShiftLeft) { 3_000.0 } else { 300.0 };
    tf.translation += v.normalize_or_zero() * speed * dt;
}

/// WARGAME_WARHEAD (see `Automation`).
fn auto_warhead(
    mut auto: ResMut<Automation>,
    app: Res<State<AppState>>,
    hm: Res<crate::world::Heightmap>,
    mut debug_cam: ResMut<DebugCamActive>,
    mut cam: Query<&mut Transform, With<MainCamera>>,
    mut launches: EventWriter<crate::warheads::LaunchWarhead>,
) {
    let Some((lon, lat)) = auto.warhead else {
        return;
    };
    if *app.get() != AppState::Playing || auto.elapsed < 2.0 {
        return;
    }
    let [x, z] = crate::geo::geo(lon, lat);
    let target = Vec3::new(x, hm.surface(x, z), z);
    if !auto.warhead_sent {
        auto.warhead_sent = true;
        launches.send(crate::warheads::LaunchWarhead { side: Side::Nato, target });
        debug_cam.0 = true;
    }
    // Pinned every frame so stray mouse input can't turn it away.
    if let Ok(mut tf) = cam.get_single_mut() {
        *tf = Transform::from_translation(target + Vec3::new(0.0, 900.0, 4_500.0)).looking_at(target + Vec3::Y * 500.0, Vec3::Y);
    }
}

/// WARGAME_LOG also prints every ground unit's orders every 20 s.
fn log_units(auto: Res<Automation>, time: Res<Time>, units: Query<(&crate::ground::GroundUnit, &Transform)>, mut timer: Local<f32>) {
    if !auto.log {
        return;
    }
    *timer -= time.delta_secs();
    if *timer > 0.0 {
        return;
    }
    *timer = 20.0;
    for (u, tf) in &units {
        let (lon, lat) = crate::geo::lonlat(tf.translation.x, tf.translation.z);
        let goal = u.auto_goal.as_ref().map(|g| g.1.as_str()).unwrap_or("-");
        info!(
            "UNIT {:<24} str {:>3.0} at {lon:5.1}E {lat:4.1}N {:?} engaged {} path {} goal {goal}",
            u.name, u.strength, u.order, u.engaged, u.path.len()
        );
    }
}

/// WARGAME_LOG also prints the frame rate every 2 s.
fn log_fps(auto: Res<Automation>, time: Res<Time<Real>>, diagnostics: Res<DiagnosticsStore>, mut timer: Local<f32>) {
    if !auto.log {
        return;
    }
    *timer -= time.delta_secs();
    if *timer > 0.0 {
        return;
    }
    *timer = 2.0;
    let fps = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed()).unwrap_or(0.0);
    info!("FPS {fps:.0}");
}

fn fps_overlay(
    mut contexts: EguiContexts,
    ui: Res<DebugUi>,
    diagnostics: Res<DiagnosticsStore>,
    cheats: Res<Cheats>,
    debug_cam: Res<DebugCamActive>,
) {
    if !ui.fps {
        return;
    }
    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);
    let mut flags = String::new();
    if debug_cam.0 {
        flags += "  [FREE CAM]";
    }
    if cheats.infinite_fuel {
        flags += "  [INF FUEL]";
    }
    if cheats.god_mode {
        flags += "  [GOD]";
    }
    egui::Area::new(egui::Id::new("fps"))
        .anchor(egui::Align2::LEFT_TOP, [8.0, 6.0])
        .interactable(false)
        .show(contexts.ctx_mut(), |ui| {
            let text = egui::RichText::new(format!("{fps:.0} FPS{flags}")).color(egui::Color32::from_rgb(200, 255, 200)).monospace();
            ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Extend));
        });
}

fn debug_panel(
    mut contexts: EguiContexts,
    mut ui_state: ResMut<DebugUi>,
    mut tune: ResMut<AeroTuning>,
    mut cheats: ResMut<Cheats>,
) {
    if !ui_state.panel {
        return;
    }
    let mut open = true;
    egui::Window::new("Debug").open(&mut open).default_pos([10.0, 40.0]).show(contexts.ctx_mut(), |ui| {
        ui.strong("Aero multipliers");
        ui.add(egui::Slider::new(&mut tune.lift, 0.3..=2.0).text("lift"));
        ui.add(egui::Slider::new(&mut tune.drag, 0.3..=3.0).text("drag"));
        ui.add(egui::Slider::new(&mut tune.thrust, 0.3..=3.0).text("thrust"));
        ui.add(egui::Slider::new(&mut tune.rates, 0.3..=2.5).text("rotation rates"));
        ui.add(egui::Slider::new(&mut tune.g_limit, 0.3..=1.5).text("G limit"));
        if ui.button("Reset aero").clicked() {
            *tune = AeroTuning::default();
        }
        ui.separator();
        ui.checkbox(&mut cheats.infinite_fuel, "Infinite fuel (F5)");
        ui.checkbox(&mut cheats.god_mode, "God mode (F8)");
        ui.checkbox(&mut ui_state.vectors, "Velocity/lift/missile vectors (F4)");
        ui.checkbox(&mut ui_state.ranges, "Unit ranges (F10)");
        ui.small("F6 drone · F7 fuel leak · F9 enemy jet · F1 free cam");
    });
    if !open {
        ui_state.panel = false;
    }
}

fn vectors(
    ui: Res<DebugUi>,
    mut gizmos: Gizmos,
    cam: Query<&Transform, With<MainCamera>>,
    jets: Query<(&Transform, &Velocity, &FlightState), With<Jet>>,
    missiles: Query<(&Missile, &Transform)>,
    targets: Query<&GlobalTransform>,
) {
    if !ui.vectors {
        return;
    }
    let cam = cam.get_single().map(|t| t.translation).unwrap_or_default();
    for (tf, v, st) in &jets {
        if tf.translation.distance(cam) > 3_000.0 {
            continue;
        }
        let p = tf.translation;
        const N: f32 = 1.25e-4; // 160 kN (≈1 G) ≈ 20 m
        gizmos.arrow(p, p + v.0 * 0.15, Color::srgb(0.2, 0.6, 1.0));
        gizmos.arrow(p, p + st.debug_lift * N, Color::srgb(0.2, 1.0, 0.2));
        gizmos.arrow(p, p + st.debug_drag * N * 4.0, Color::srgb(1.0, 0.3, 0.2));
        gizmos.arrow(p, p + st.debug_thrust * N, Color::srgb(1.0, 0.8, 0.2));
    }
    for (m, tf) in &missiles {
        if let Some(t) = m.target.and_then(|t| targets.get(t).ok()) {
            gizmos.line(tf.translation, t.translation(), Color::srgb(1.0, 0.2, 1.0));
        }
    }
}

fn ranges(ui: Res<DebugUi>, mut gizmos: Gizmos, units: Query<(&GroundUnit, &Transform)>) {
    if !ui.ranges {
        return;
    }
    for (u, tf) in &units {
        let col = match u.side {
            Side::Nato => Color::srgb(0.3, 0.5, 1.0),
            Side::Enemy => Color::srgb(1.0, 0.3, 0.3),
        };
        let mut circle = |r: f32, c: Color| {
            let pts: Vec<Vec3> = (0..=48)
                .map(|i| {
                    let a = i as f32 / 48.0 * std::f32::consts::TAU;
                    tf.translation + Vec3::new(a.cos() * r, 30.0, a.sin() * r)
                })
                .collect();
            gizmos.linestrip(pts, c);
        };
        circle(u.stats().range, col);
        if u.kind == UnitKind::AntiAir {
            circle(AA_RANGE, Color::srgb(1.0, 0.8, 0.2));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn automation(
    mut commands: Commands,
    time: Res<Time<Real>>,
    mut auto: ResMut<Automation>,
    app: Res<State<AppState>>,
    mut next_app: ResMut<NextState<AppState>>,
    mut next_view: ResMut<NextState<ViewMode>>,
    mut rig: ResMut<CameraRig>,
    mut debug_cam: ResMut<DebugCamActive>,
    player: Query<(&Transform, &Velocity, &FlightState), With<Player>>,
    mut exit: EventWriter<AppExit>,
    mut virt: ResMut<Time<Virtual>>,
    log: Res<crate::game_rules::EventLog>,
    mut printed: Local<usize>,
    mut booms: EventReader<crate::weapons::Detonation>,
) {
    // In shoot mode, photograph the first real explosion.
    for b in booms.read() {
        if auto.fly.as_deref() == Some("shoot") && b.size != crate::weapons::Boom::Spark && !auto.taken && auto.shot_at > auto.elapsed + 0.3 {
            auto.shot_at = auto.elapsed + 0.25;
        }
        // In warhead mode, photograph the blast.
        if auto.warhead.is_some() && b.size == crate::weapons::Boom::Huge && !auto.taken {
            auto.shot_at = auto.elapsed + 4.0;
        }
    }
    if auto.log {
        for line in log.lines.iter().skip(*printed) {
            info!("LOG {line}");
        }
        *printed = log.lines.len();
    }
    auto.elapsed += time.delta_secs();
    if let Some(s) = auto.timescale {
        virt.set_relative_speed(s);
    }
    if auto.autostart && *app.get() == AppState::Menu && auto.elapsed > 0.5 {
        next_app.set(AppState::Playing);
        auto.autostart = false;
    }
    if auto.fly.is_some() {
        rig.look_yaw = 0.0;
        rig.look_pitch = 0.0;
    }
    if !auto.view_set && *app.get() == AppState::Playing && auto.elapsed > 1.5 {
        auto.view_set = true;
        match auto.view.as_deref() {
            Some("map") => next_view.set(ViewMode::Map),
            Some("cockpit") => rig.cockpit = true,
            Some("debug") => debug_cam.0 = true,
            _ => {}
        }
    }
    if auto.log && *app.get() == AppState::Playing {
        auto.log_timer -= time.delta_secs();
        if auto.log_timer <= 0.0 {
            auto.log_timer = 1.0;
            if let Ok((tf, v, st)) = player.get_single() {
                info!(
                    "player pos {:.0?} spd {:.0} thr {:.2} g {:.1} aoa {:.1} ground {} gear {} fuel {:.0}",
                    tf.translation,
                    v.0.length(),
                    st.throttle,
                    st.g_load,
                    st.aoa.to_degrees(),
                    st.on_ground,
                    st.gear_down,
                    st.fuel
                );
            }
        }
    }
    if let Some(path) = auto.shot.clone() {
        if !auto.taken && auto.elapsed > auto.shot_at {
            auto.taken = true;
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        } else if auto.taken && auto.elapsed > auto.shot_at + 1.5 {
            exit.send(AppExit::Success);
        }
    }
}
