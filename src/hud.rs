//! Flight HUD (§7): speed, altitude, heading, G, throttle, fuel and leak
//! warning, weapons, pitch ladder, flight-path marker, target box + lock,
//! missile warning, restock progress and the G vignette.

use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::config::*;
use crate::flight::{FlightState, GTolerance, Restock, Velocity, Jet};
use crate::heightgen::smoothstep;
use crate::pilot_ai::predicted_agl;
use crate::player::Player;
use crate::state::*;
use crate::weapons::{Health, JetDamage, LockState, Missile, Weapons};
use crate::world::{on_runway, Heightmap};

#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct HudGizmos;

#[derive(Component)]
struct HudRoot;
#[derive(Component)]
struct FlightText;
#[derive(Component)]
struct WeaponText;
#[derive(Component)]
struct WarnText;
#[derive(Component)]
struct LockText;
#[derive(Component)]
struct Vignette;
#[derive(Component)]
struct Blackout;
#[derive(Component)]
struct RestockBox;
#[derive(Component)]
struct RestockFill;
#[derive(Component)]
struct RestockLabel;

const HUD_GREEN: Color = Color::srgb(0.35, 1.0, 0.45);
const HUD_RED: Color = Color::srgb(1.0, 0.25, 0.2);
const HUD_AMBER: Color = Color::srgb(1.0, 0.8, 0.2);

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_gizmo_group::<HudGizmos>()
            .add_systems(Startup, configure_gizmos)
            .add_systems(OnEnter(AppState::Playing), spawn_hud)
            .add_systems(
                Update,
                (hud_visibility, hud_text, hud_vignette, hud_restock).in_set(GameplaySet),
            )
            .add_systems(
                Update,
                hud_gizmos
                    .after(CameraSet::Shake)
                    .run_if(in_state(ViewMode::Flight))
                    .run_if(debug_cam_off),
            );
    }
}

fn configure_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (cfg, _) = store.config_mut::<HudGizmos>();
    cfg.line_width = 2.0;
    cfg.depth_bias = -1.0;
}

fn vignette_image() -> Image {
    let n = 256u32;
    let mut data = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let dx = (x as f32 + 0.5) / n as f32 * 2.0 - 1.0;
            let dy = (y as f32 + 0.5) / n as f32 * 2.0 - 1.0;
            let r = (dx * dx + dy * dy).sqrt();
            let a = smoothstep(0.35, 1.25, r);
            data.extend_from_slice(&[255, 255, 255, (a * 255.0) as u8]);
        }
    }
    Image::new(
        Extent3d { width: n, height: n, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn spawn_hud(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let vignette = images.add(vignette_image());
    let font = |size: f32| TextFont { font_size: size, ..default() };
    commands
        .spawn((
            Name::new("hud"),
            HudRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            StateScoped(AppState::Playing),
        ))
        .with_children(|p| {
            let full = Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            };
            p.spawn((Vignette, ImageNode::new(vignette).with_color(Color::NONE), full.clone()));
            p.spawn((Blackout, full, BackgroundColor(Color::NONE)));
            p.spawn((
                FlightText,
                Text::new(""),
                font(17.0),
                TextColor(HUD_GREEN),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(24.0),
                    bottom: Val::Px(24.0),
                    padding: UiRect::all(Val::Px(8.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.35)),
            ));
            p.spawn((
                WeaponText,
                Text::new(""),
                font(17.0),
                TextColor(HUD_GREEN),
                TextLayout::new_with_justify(JustifyText::Right),
                Node {
                    position_type: PositionType::Absolute,
                    right: Val::Px(24.0),
                    bottom: Val::Px(24.0),
                    padding: UiRect::all(Val::Px(8.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.35)),
            ));
            p.spawn(Node {
                position_type: PositionType::Absolute,
                top: Val::Px(70.0),
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                ..default()
            })
            .with_children(|c| {
                c.spawn((WarnText, Text::new(""), font(26.0), TextColor(HUD_RED), TextLayout::new_with_justify(JustifyText::Center)));
            });
            p.spawn(Node {
                position_type: PositionType::Absolute,
                top: Val::Percent(60.0),
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                ..default()
            })
            .with_children(|c| {
                c.spawn((LockText, Text::new(""), font(20.0), TextColor(HUD_AMBER)));
            });
            // Restock progress bar.
            p.spawn((
                RestockBox,
                Node {
                    position_type: PositionType::Absolute,
                    bottom: Val::Px(120.0),
                    left: Val::Percent(35.0),
                    width: Val::Percent(30.0),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(6.0),
                    ..default()
                },
                Visibility::Hidden,
            ))
            .with_children(|c| {
                c.spawn((RestockLabel, Text::new(""), font(18.0), TextColor(HUD_GREEN)));
                c.spawn((
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Px(12.0),
                        border: UiRect::all(Val::Px(2.0)),
                        ..default()
                    },
                    BorderColor(HUD_GREEN),
                ))
                .with_children(|b| {
                    b.spawn((
                        RestockFill,
                        Node {
                            width: Val::Percent(0.0),
                            height: Val::Percent(100.0),
                            ..default()
                        },
                        BackgroundColor(HUD_GREEN),
                    ));
                });
            });
        });
}

fn hud_visibility(
    view: Res<State<ViewMode>>,
    debug: Res<DebugCamActive>,
    player: Query<(), With<Player>>,
    mut root: Query<&mut Visibility, With<HudRoot>>,
) {
    let show = *view.get() == ViewMode::Flight && !debug.0 && !player.is_empty();
    for mut v in &mut root {
        *v = if show { Visibility::Inherited } else { Visibility::Hidden };
    }
}

fn heading_deg(fwd: Vec3) -> i32 {
    (fwd.x.atan2(-fwd.z).to_degrees().round() as i32).rem_euclid(360)
}

#[allow(clippy::type_complexity)]
fn hud_text(
    time: Res<Time<Real>>,
    hm: Res<Heightmap>,
    player: Query<
        (Entity, &Transform, &Velocity, &FlightState, &GTolerance, &Health, &JetDamage, &Weapons, &LockState),
        With<Player>,
    >,
    missiles: Query<&Missile>,
    mut texts: ParamSet<(
        Query<&mut Text, With<FlightText>>,
        Query<&mut Text, With<WeaponText>>,
        Query<&mut Text, With<WarnText>>,
        Query<(&mut Text, &mut TextColor), With<LockText>>,
    )>,
) {
    let Ok((me, tf, vel, st, tol, hp, dmg, w, lock)) = player.get_single() else {
        return;
    };
    let p = tf.translation;
    let speed = vel.0.length();
    let agl = p.y - hm.surface(p.x, p.z);
    let ab = st.throttle > AFTERBURNER_THRESHOLD;
    let stats = NATO_JET;
    let fuel_pct = st.fuel / stats.max_fuel * 100.0;
    let mut s = format!(
        "SPD {:>5.0} km/h  M{:.2}\nALT {:>6.0} m   AGL {:.0}\nHDG {:>5}     VS {:+.0} m/s\nG   {:>+5.1}   AoA {:.0}\nTHR {:>5.0}%{}\nFUEL {:>5.0} kg ({:.0}%)",
        speed * 3.6,
        speed / SPEED_OF_SOUND,
        p.y,
        agl,
        heading_deg(*tf.forward()),
        vel.0.y,
        st.g_load,
        st.aoa.to_degrees(),
        st.throttle * 100.0,
        if ab { "  AB" } else { "" },
        st.fuel,
        fuel_pct,
    );
    if dmg.fuel_leak > 0.0 {
        s += &format!("  LEAK -{:.1} kg/s", dmg.fuel_leak);
    }
    s += &format!(
        "\nHP  {:>5.0}%\n{}{}",
        hp.hp / hp.max * 100.0,
        if st.gear_down { "GEAR DOWN  " } else { "" },
        if st.airbrake { "AIRBRAKE" } else { "" }
    );
    if tol.blackout > 0.05 {
        s += &format!("\nG-LOC {:>3.0}%", tol.blackout * 100.0);
    }
    texts.p0().single_mut().0 = s;

    texts.p1().single_mut().0 = format!(
        "GUN {:>4}\nMSL {:>4}\nFLR {:>4}",
        w.gun_ammo, w.missiles, w.flares
    );

    // Warnings, blinking.
    let blink = (time.elapsed_secs() * 3.0).fract() < 0.6;
    let mut warns: Vec<&str> = Vec::new();
    let tracked = missiles.iter().any(|m| m.target == Some(me));
    if tracked {
        warns.push("MISSILE");
    }
    if !st.on_ground && predicted_agl(&hm, p, vel.0) < 120.0 && vel.0.y < -5.0 {
        warns.push("PULL UP");
    }
    if st.stalled && !st.on_ground {
        warns.push("STALL");
    }
    if st.engine_out {
        warns.push("ENGINE OUT");
    } else if dmg.engine_damaged {
        warns.push("ENGINE DAMAGED");
    }
    if dmg.fuel_leak > 0.0 {
        warns.push("FUEL LEAK");
    }
    if dmg.on_fire {
        warns.push("FIRE");
    }
    if fuel_pct < 15.0 && !st.engine_out {
        warns.push("BINGO FUEL");
    }
    texts.p2().single_mut().0 = if blink { warns.join("   ") } else { String::new() };

    let mut q3 = texts.p3();
    let (mut t, mut c) = q3.single_mut();
    if lock.locked {
        t.0 = "LOCK".into();
        c.0 = HUD_RED;
    } else if lock.candidate.is_some() {
        t.0 = "SEEKING".into();
        c.0 = HUD_AMBER;
    } else {
        t.0 = String::new();
    }
}

fn hud_vignette(
    player: Query<&GTolerance, With<Player>>,
    mut vig: Query<&mut ImageNode, With<Vignette>>,
    mut black: Query<&mut BackgroundColor, With<Blackout>>,
) {
    let Ok(tol) = player.get_single() else {
        return;
    };
    let (Ok(mut img), Ok(mut bg)) = (vig.get_single_mut(), black.get_single_mut()) else {
        return;
    };
    if tol.redout > tol.blackout {
        let r = tol.redout;
        img.color = Color::srgba(0.6, 0.0, 0.0, smoothstep(0.1, 0.6, r));
        bg.0 = Color::srgba(0.45, 0.0, 0.0, smoothstep(0.6, 1.0, r) * 0.95);
    } else {
        let b = tol.blackout;
        // Grey-out first, then the tunnel closes to black.
        let grey = 0.25 * (1.0 - smoothstep(0.5, 0.9, b));
        img.color = Color::srgba(grey, grey, grey, smoothstep(0.15, 0.6, b));
        bg.0 = Color::srgba(0.0, 0.0, 0.0, smoothstep(0.7, 1.0, b));
    }
}

#[allow(clippy::type_complexity)]
fn hud_restock(
    player: Query<(&Jet, &Transform, &FlightState, &Restock), With<Player>>,
    mut boxes: Query<&mut Visibility, With<RestockBox>>,
    mut fill: Query<&mut Node, With<RestockFill>>,
    mut label: Query<&mut Text, With<RestockLabel>>,
) {
    let (Ok(mut vis), Ok(mut node), Ok(mut text)) = (boxes.get_single_mut(), fill.get_single_mut(), label.get_single_mut()) else {
        return;
    };
    let Ok((jet, tf, st, rs)) = player.get_single() else {
        *vis = Visibility::Hidden;
        return;
    };
    let base = jet.side.base();
    let on_base = st.on_ground && on_runway(&base, tf.translation.x, tf.translation.z);
    if !on_base || rs.full {
        *vis = Visibility::Hidden;
        return;
    }
    *vis = Visibility::Inherited;
    if rs.progress >= 1.0 {
        text.0 = "RESTOCKED: fuel, ammo, repairs".into();
        node.width = Val::Percent(100.0);
    } else if rs.active() {
        text.0 = format!("RESTOCKING {:.0}%", rs.progress * 100.0);
        node.width = Val::Percent(rs.progress * 100.0);
    } else if rs.stopped > 0.0 {
        text.0 = format!("Hold still to restock... {:.0}", (RESTOCK_WAIT - rs.stopped).ceil());
        node.width = Val::Percent(rs.stopped / RESTOCK_WAIT * 100.0);
    } else {
        text.0 = "Stop on the runway to restock".into();
        node.width = Val::Percent(0.0);
    }
}

#[allow(clippy::type_complexity)]
fn hud_gizmos(
    mut gizmos: Gizmos<HudGizmos>,
    cam: Query<&Transform, With<MainCamera>>,
    player: Query<(Entity, &Transform, &Velocity, &LockState), (With<Player>, Without<MainCamera>)>,
    targets: Query<&GlobalTransform>,
    missiles: Query<(&Missile, &Transform), Without<MainCamera>>,
) {
    let (Ok(cam), Ok((me, jet, vel, lock))) = (cam.get_single(), player.get_single()) else {
        return;
    };
    let c = cam.translation;
    const D: f32 = 100.0;
    let fwd = jet.forward().as_vec3();
    let h = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(Vec3::NEG_Z);
    let right_h = h.cross(Vec3::Y).normalize();

    // Pitch ladder every 10°, horizon longer.
    for deg in (-80..=80).step_by(10) {
        let a = (deg as f32).to_radians();
        let center = c + (h * a.cos() + Vec3::Y * a.sin()) * D;
        let (half, gap) = if deg == 0 { (D * 0.5, D * 0.04) } else { (D * 0.12, D * 0.05) };
        let col = if deg < 0 { HUD_GREEN.with_alpha(0.55) } else { HUD_GREEN.with_alpha(0.8) };
        gizmos.line(center - right_h * half, center - right_h * gap, col);
        gizmos.line(center + right_h * gap, center + right_h * half, col);
        if deg != 0 {
            // Tick pointing at the horizon.
            let tick = -Vec3::Y * a.signum() * D * 0.015;
            gizmos.line(center - right_h * half, center - right_h * half + tick, col);
            gizmos.line(center + right_h * half, center + right_h * half + tick, col);
        }
    }

    let cam_right = cam.right().as_vec3();
    let cam_up = cam.up().as_vec3();
    let ring = |g: &mut Gizmos<HudGizmos>, center: Vec3, r: f32, color: Color| {
        let pts: Vec<Vec3> = (0..=16)
            .map(|i| {
                let t = i as f32 / 16.0 * std::f32::consts::TAU;
                center + (cam_right * t.cos() + cam_up * t.sin()) * r
            })
            .collect();
        g.linestrip(pts, color);
    };

    // Boresight cross and flight-path marker.
    let bore = c + fwd * D;
    gizmos.line(bore - cam_right * 1.2, bore + cam_right * 1.2, HUD_GREEN);
    gizmos.line(bore - cam_up * 1.2, bore + cam_up * 1.2, HUD_GREEN);
    if vel.0.length() > 20.0 {
        let fpm = c + vel.0.normalize() * D;
        ring(&mut gizmos, fpm, 1.0, HUD_GREEN);
        gizmos.line(fpm + cam_right * 1.0, fpm + cam_right * 2.2, HUD_GREEN);
        gizmos.line(fpm - cam_right * 1.0, fpm - cam_right * 2.2, HUD_GREEN);
        gizmos.line(fpm + cam_up * 1.0, fpm + cam_up * 1.8, HUD_GREEN);
    }
    // Seeker circle.
    ring(&mut gizmos, c + fwd * D, D * MISSILE_LOCK_CONE.tan(), HUD_GREEN.with_alpha(0.25));

    // Target box.
    if let Some(t) = lock.candidate.and_then(|t| targets.get(t).ok()) {
        let dir = (t.translation() - c).normalize();
        if dir.dot(cam.forward().as_vec3()) > 0.0 {
            let center = c + dir * D;
            let s = 2.5;
            let col = if lock.locked { HUD_RED } else { HUD_AMBER };
            let (r, u) = (cam_right * s, cam_up * s);
            gizmos.linestrip(vec![center - r - u, center + r - u, center + r + u, center - r + u, center - r - u], col);
            if lock.locked {
                gizmos.line(center - r - u, center + r + u, col);
                gizmos.line(center - r + u, center + r - u, col);
            }
        }
    }

    // Missile warning: arrows from the centre towards each tracking missile.
    for (m, mtf) in &missiles {
        if m.target != Some(me) {
            continue;
        }
        let to = mtf.translation - c;
        let flat = cam_right * to.dot(cam_right) + cam_up * to.dot(cam_up);
        let dir = flat.normalize_or(cam_up);
        let center = c + cam.forward().as_vec3() * D;
        gizmos.arrow(center + dir * D * 0.08, center + dir * D * 0.2, HUD_RED);
    }
}
