//! The player: jet reserve, launch, death, camera rigs (chase/cockpit,
//! death cam, menu orbit), keyboard/mouse flight input and the map-mode
//! autopilot.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};

use crate::config::*;
use crate::flight::{spawn_jet, ControlInput, FlightState, JetAssets, Velocity};
use crate::game_rules::EventLog;
use crate::pilot_ai::{predicted_agl, steer};
use crate::state::*;
use crate::weapons::{JetDestroyed, DamageKind};
use crate::world::{base_center, runway_start, Heightmap};

#[derive(Component)]
pub struct Player;

#[derive(Resource)]
pub struct JetReserve {
    pub remaining: u32,
}

#[derive(Resource, Default)]
pub struct PlayerStatus {
    pub flying: bool,
    pub death_pos: Vec3,
    pub death_timer: f32,
}

/// "Launch Jet" (map panel): spawns the next reserve jet on the runway.
#[derive(Event)]
pub struct LaunchJet;

#[derive(Resource)]
pub struct CameraRig {
    pub cockpit: bool,
    pub look_yaw: f32,
    pub look_pitch: f32,
    look_idle: f32,
    chase_offset: Vec3,
    chase_up: Vec3,
    orbit: f32,
    pub autopilot_alt: f32,
}

impl Default for CameraRig {
    fn default() -> Self {
        Self {
            cockpit: false,
            look_yaw: 0.0,
            look_pitch: 0.0,
            look_idle: 0.0,
            chase_offset: Vec3::new(0.0, CHASE_HEIGHT, CHASE_DISTANCE),
            chase_up: Vec3::Y,
            orbit: 0.0,
            autopilot_alt: AI_CRUISE_ALT,
        }
    }
}

#[derive(Component)]
struct CockpitFrame;

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<LaunchJet>()
            .insert_resource(JetReserve { remaining: JET_RESERVE })
            .init_resource::<PlayerStatus>()
            .init_resource::<CameraRig>()
            .add_systems(Startup, (spawn_camera, spawn_cockpit))
            .add_systems(OnEnter(AppState::Playing), start_round)
            .add_systems(OnEnter(ViewMode::Map), remember_autopilot_alt)
            .add_systems(
                Update,
                (
                    launch_jet,
                    player_death,
                    death_timer.run_if(in_state(ViewMode::DeathCam)),
                    player_input.run_if(in_state(PlayState::Running)),
                )
                    .chain()
                    .in_set(GameplaySet),
            )
            .add_systems(
                Update,
                (
                    flight_camera.run_if(in_state(ViewMode::Flight)),
                    death_camera.run_if(in_state(ViewMode::DeathCam)),
                    menu_camera.run_if(not(in_state(AppState::Playing))),
                )
                    .run_if(debug_cam_off)
                    .in_set(CameraSet::Position),
            )
            .add_systems(Update, (cursor_grab, cockpit_visibility));
    }
}

fn spawn_camera(mut commands: Commands) {
    let sky = Color::srgb(SKY_COLOR[0], SKY_COLOR[1], SKY_COLOR[2]);
    commands.spawn((
        Name::new("main camera"),
        MainCamera,
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: 70f32.to_radians(),
            near: 0.1,
            far: 500_000.0,
            ..default()
        }),
        DistanceFog {
            color: sky,
            directional_light_color: Color::srgba(1.0, 0.95, 0.85, 0.5),
            directional_light_exponent: 30.0,
            falloff: FogFalloff::from_visibility_squared(FOG_VISIBILITY),
        },
        Transform::from_xyz(NATO_BASE.center[0], 600.0, NATO_BASE.center[1] + 2_000.0)
            .looking_at(Vec3::new(NATO_BASE.center[0], NATO_BASE.elevation, NATO_BASE.center[1]), Vec3::Y),
    ));
}

fn spawn_cockpit(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let frame = materials.add(StandardMaterial {
        base_color: Color::srgb(0.12, 0.13, 0.14),
        perceptual_roughness: 0.8,
        ..default()
    });
    let panel = materials.add(Color::srgb(0.08, 0.09, 0.1));
    let screen = materials.add(StandardMaterial {
        base_color: Color::srgb(0.1, 0.3, 0.15),
        emissive: LinearRgba::rgb(0.2, 0.8, 0.3),
        ..default()
    });
    commands
        .spawn((Name::new("cockpit"), CockpitFrame, Transform::default(), Visibility::Hidden))
        .with_children(|p| {
            // Dashboard and its two screens.
            p.spawn((Mesh3d(meshes.add(Cuboid::new(1.5, 0.45, 0.6))), MeshMaterial3d(panel.clone()), Transform::from_xyz(0.0, -0.5, -0.85)));
            for x in [-0.35, 0.35] {
                p.spawn((
                    Mesh3d(meshes.add(Cuboid::new(0.5, 0.3, 0.02))),
                    MeshMaterial3d(screen.clone()),
                    Transform::from_xyz(x, -0.36, -0.54).with_rotation(Quat::from_rotation_x(-0.3)),
                ));
            }
            // Canopy bow and side rails.
            p.spawn((Mesh3d(meshes.add(Cuboid::new(0.06, 0.06, 1.6))), MeshMaterial3d(frame.clone()), Transform::from_xyz(0.0, 0.62, -0.6)));
            for s in [-1.0f32, 1.0] {
                p.spawn((
                    Mesh3d(meshes.add(Cuboid::new(0.05, 0.9, 0.05))),
                    MeshMaterial3d(frame.clone()),
                    Transform::from_xyz(s * 0.42, 0.1, -1.05).with_rotation(Quat::from_rotation_z(s * 0.45) * Quat::from_rotation_x(-0.5)),
                ));
                p.spawn((
                    Mesh3d(meshes.add(Cuboid::new(0.08, 0.08, 2.0))),
                    MeshMaterial3d(frame.clone()),
                    Transform::from_xyz(s * 0.62, -0.25, -0.2),
                ));
            }
        });
}

fn start_round(
    mut reserve: ResMut<JetReserve>,
    mut status: ResMut<PlayerStatus>,
    mut rig: ResMut<CameraRig>,
    mut launch: EventWriter<LaunchJet>,
) {
    reserve.remaining = JET_RESERVE;
    *status = PlayerStatus::default();
    *rig = CameraRig::default();
    launch.send(LaunchJet);
}

pub fn spawn_player_jet(commands: &mut Commands, assets: &JetAssets) -> Entity {
    let pos = runway_start(&NATO_BASE) + Vec3::Y * GEAR_HEIGHT;
    let e = spawn_jet(commands, assets, Side::Nato, pos, Quat::IDENTITY, 0.0, true);
    commands.entity(e).insert((Player, Name::new("player jet")));
    e
}

fn launch_jet(
    mut commands: Commands,
    mut events: EventReader<LaunchJet>,
    assets: Res<JetAssets>,
    mut reserve: ResMut<JetReserve>,
    mut status: ResMut<PlayerStatus>,
    mut rig: ResMut<CameraRig>,
    mut view: ResMut<NextState<ViewMode>>,
    mut log: ResMut<EventLog>,
) {
    for _ in events.read() {
        if status.flying || reserve.remaining == 0 {
            continue;
        }
        reserve.remaining -= 1;
        status.flying = true;
        spawn_player_jet(&mut commands, &assets);
        rig.look_yaw = 0.0;
        rig.look_pitch = 0.0;
        view.set(ViewMode::Flight);
        log.push(format!("Jet launched ({} left in reserve)", reserve.remaining));
    }
}

fn player_death(
    mut events: EventReader<JetDestroyed>,
    mut status: ResMut<PlayerStatus>,
    mut view: ResMut<NextState<ViewMode>>,
    mut log: ResMut<EventLog>,
    mut damage: EventReader<crate::weapons::DamageEvent>,
    players: Query<Entity, With<Player>>,
) {
    // Remember whether the last thing to hurt the player was the ground.
    let player = players.get_single().ok();
    let mut crashed = false;
    for d in damage.read() {
        if Some(d.target) == player && d.kind == DamageKind::Crash {
            crashed = true;
        }
    }
    for ev in events.read() {
        if !ev.was_player {
            continue;
        }
        status.flying = false;
        status.death_pos = ev.pos;
        status.death_timer = DEATH_CAM_TIME;
        view.set(ViewMode::DeathCam);
        log.push(if crashed { "You crashed. Jet lost." } else { "You were shot down. Jet lost." });
    }
}

fn death_timer(time: Res<Time>, mut status: ResMut<PlayerStatus>, mut view: ResMut<NextState<ViewMode>>) {
    status.death_timer -= time.delta_secs();
    if status.death_timer <= 0.0 {
        view.set(ViewMode::Map);
    }
}

fn remember_autopilot_alt(mut rig: ResMut<CameraRig>, hm: Res<Heightmap>, q: Query<&Transform, With<Player>>) {
    if let Ok(tf) = q.get_single() {
        let p = tf.translation;
        rig.autopilot_alt = p.y.max(hm.surface(p.x, p.z) + 400.0);
    }
}

fn approach(current: f32, target: f32, rate: f32) -> f32 {
    current + (target - current).clamp(-rate, rate)
}

#[allow(clippy::too_many_arguments)]
fn player_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    time: Res<Time>,
    hm: Res<Heightmap>,
    view: Res<State<ViewMode>>,
    debug: Res<DebugCamActive>,
    mut rig: ResMut<CameraRig>,
    mut q: Query<(&mut ControlInput, &Transform, &Velocity, &FlightState), With<Player>>,
) {
    let Ok((mut input, tf, vel, st)) = q.get_single_mut() else {
        return;
    };
    let dt = time.delta_secs();

    // Map open (or debug camera): autopilot holds level flight.
    if *view.get() != ViewMode::Flight || debug.0 {
        input.fire_gun = false;
        if st.on_ground {
            input.pitch = 0.0;
            input.roll = 0.0;
            input.yaw = 0.0;
            return;
        }
        let fwd = tf.forward();
        let h = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or_zero();
        let mut desired = h + Vec3::Y * ((rig.autopilot_alt - tf.translation.y) / 1_000.0).clamp(-0.2, 0.2);
        if predicted_agl(&hm, tf.translation, vel.0) < 150.0 {
            desired = h * 0.5 + Vec3::Y;
        }
        let (p, r, y) = steer(tf.rotation, desired, 0.4);
        input.pitch = p;
        input.roll = r;
        input.yaw = y;
        return;
    }

    let axis = |neg: KeyCode, pos: KeyCode| -> f32 {
        (keys.pressed(pos) as i32 - keys.pressed(neg) as i32) as f32
    };
    // W = stick forward (nose down), S = nose up.
    let pitch = axis(KeyCode::KeyW, KeyCode::KeyS);
    let roll = axis(KeyCode::KeyA, KeyCode::KeyD);
    let yaw = axis(KeyCode::KeyQ, KeyCode::KeyE);
    let ramp = STICK_RAMP * dt;
    input.pitch = approach(input.pitch, pitch, if pitch == 0.0 { ramp * 2.0 } else { ramp });
    input.roll = approach(input.roll, roll, if roll == 0.0 { ramp * 2.0 } else { ramp * 1.5 });
    input.yaw = approach(input.yaw, yaw, ramp);

    if keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]) {
        input.throttle = (input.throttle + THROTTLE_RATE * dt).min(1.0);
    }
    if keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]) {
        input.throttle = (input.throttle - THROTTLE_RATE * dt).max(0.0);
    }
    input.fire_gun = keys.pressed(KeyCode::Space) || mouse.pressed(MouseButton::Left);
    if mouse.just_pressed(MouseButton::Right) || keys.just_pressed(KeyCode::KeyR) {
        input.fire_missile = true;
    }
    if keys.pressed(KeyCode::KeyX) {
        input.flare = true;
    }
    if keys.just_pressed(KeyCode::KeyG) {
        input.toggle_gear = true;
    }
    if keys.just_pressed(KeyCode::KeyB) {
        input.toggle_airbrake = true;
    }
    if keys.just_pressed(KeyCode::KeyV) {
        rig.cockpit = !rig.cockpit;
    }
    if keys.just_pressed(KeyCode::KeyC) {
        rig.look_yaw = 0.0;
        rig.look_pitch = 0.0;
    }
    // Free-look. In chase view it springs back once the mouse rests.
    let d = motion.delta * MOUSE_LOOK_SENS;
    rig.look_yaw = (rig.look_yaw - d.x).clamp(-2.6, 2.6);
    rig.look_pitch = (rig.look_pitch - d.y).clamp(-1.2, 1.3);
    if d != Vec2::ZERO {
        rig.look_idle = 0.0;
    } else {
        rig.look_idle += dt;
    }
    if !rig.cockpit && rig.look_idle > 1.5 {
        let k = 1.0 - (-3.0 * dt).exp();
        rig.look_yaw -= rig.look_yaw * k;
        rig.look_pitch -= rig.look_pitch * k;
    }
}

fn flight_camera(
    time: Res<Time<Real>>,
    mut rig: ResMut<CameraRig>,
    players: Query<&Transform, (With<Player>, Without<MainCamera>)>,
    mut cam: Query<&mut Transform, With<MainCamera>>,
) {
    let (Ok(jet), Ok(mut cam)) = (players.get_single(), cam.get_single_mut()) else {
        return;
    };
    let dt = time.delta_secs();
    let look = Quat::from_euler(EulerRot::YXZ, rig.look_yaw, rig.look_pitch, 0.0);
    if rig.cockpit {
        cam.translation = jet.translation + jet.rotation * Vec3::new(0.0, 1.15, -4.3);
        cam.rotation = jet.rotation * look;
    } else {
        let want = jet.rotation * look * Vec3::new(0.0, CHASE_HEIGHT, CHASE_DISTANCE);
        let k = 1.0 - (-6.0 * dt).exp();
        rig.chase_offset = rig.chase_offset.lerp(want, k);
        let up = rig.chase_up.lerp(*jet.up(), 1.0 - (-4.0 * dt).exp()).normalize_or(Vec3::Y);
        rig.chase_up = up;
        cam.translation = jet.translation + rig.chase_offset;
        let focus = jet.translation + jet.rotation * look * Vec3::new(0.0, 2.0, -40.0);
        cam.look_at(focus, up);
    }
}

fn cockpit_visibility(
    rig: Res<CameraRig>,
    view: Option<Res<State<ViewMode>>>,
    debug: Res<DebugCamActive>,
    players: Query<&Transform, (With<Player>, Without<CockpitFrame>)>,
    mut frame: Query<(&mut Transform, &mut Visibility), With<CockpitFrame>>,
) {
    let Ok((mut tf, mut vis)) = frame.get_single_mut() else {
        return;
    };
    let in_flight = view.is_some_and(|v| *v.get() == ViewMode::Flight);
    match players.get_single() {
        Ok(jet) if rig.cockpit && in_flight && !debug.0 => {
            *tf = *jet * Transform::from_xyz(0.0, 1.15, -4.3);
            *vis = Visibility::Visible;
        }
        _ => *vis = Visibility::Hidden,
    }
}

fn death_camera(
    time: Res<Time<Real>>,
    status: Res<PlayerStatus>,
    mut rig: ResMut<CameraRig>,
    mut cam: Query<&mut Transform, With<MainCamera>>,
) {
    let Ok(mut cam) = cam.get_single_mut() else {
        return;
    };
    rig.orbit += time.delta_secs() * 0.35;
    let c = status.death_pos;
    cam.translation = c + Vec3::new(rig.orbit.cos() * 160.0, 70.0, rig.orbit.sin() * 160.0);
    cam.look_at(c, Vec3::Y);
}

fn menu_camera(time: Res<Time<Real>>, mut rig: ResMut<CameraRig>, mut cam: Query<&mut Transform, With<MainCamera>>) {
    let Ok(mut cam) = cam.get_single_mut() else {
        return;
    };
    rig.orbit += time.delta_secs() * 0.04;
    let c = base_center(&NATO_BASE);
    cam.translation = c + Vec3::new(rig.orbit.cos() * 2_200.0, 450.0, rig.orbit.sin() * 2_200.0);
    cam.look_at(c + Vec3::Y * 50.0, Vec3::Y);
}

fn cursor_grab(
    app: Res<State<AppState>>,
    play: Option<Res<State<PlayState>>>,
    view: Option<Res<State<ViewMode>>>,
    debug: Res<DebugCamActive>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    let Ok(mut window) = windows.get_single_mut() else {
        return;
    };
    let running = play.is_some_and(|p| *p.get() == PlayState::Running);
    let flying = view.is_some_and(|v| matches!(v.get(), ViewMode::Flight | ViewMode::Infantry));
    let lock = (*app.get() == AppState::Playing && running && flying) || debug.0;
    let mode = if lock { CursorGrabMode::Locked } else { CursorGrabMode::None };
    if window.cursor_options.grab_mode != mode {
        window.cursor_options.grab_mode = mode;
        window.cursor_options.visible = !lock;
    }
}
