//! Infantry mode (§6.5): fight on foot with a NATO infantry squad.
//! First-person soldier (walk, sprint, crouch, rifle), an IR drone that
//! makes enemies glow and drops small bombs, enemy units that shoot at you
//! when they have line of sight, and enemy recon drones that spot you.
//!
//! Automation: WARGAME_VIEW=infantry joins the first NATO infantry squad,
//! WARGAME_VIEW=drone also launches the IR drone.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use rand::Rng;

use crate::audio::{Sfx, SoundEvent};
use crate::config::*;
use crate::debug::Cheats;
use crate::fx::Particles;
use crate::game_rules::EventLog;
use crate::ground::{GroundUnit, IrView};
use crate::player::PlayerStatus;
use crate::state::*;
use crate::weapons::{spawn_bullet, Boom, DamageEvent, DamageKind, Detonation, HitSphere, WeaponAssets};
use crate::nav::NavGrid;
use crate::world::{Bridges, Heightmap};

/// Map panel / I key: become a soldier in this NATO infantry unit.
#[derive(Event)]
pub struct JoinInfantry(pub Entity);

#[derive(Component, Debug)]
pub struct PlayerSoldier {
    pub squad: Entity,
    pub hp: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub eye: f32,
    pub crouched: bool,
    pub moving: bool,
    pub aiming: bool,
    pub mag: u32,
    pub reserve: u32,
    /// Seconds left of a reload in progress.
    pub reload: f32,
    pub fire_cd: f32,
    pub sound_cd: f32,
    pub drone_cd: f32,
    pub spotted: bool,
    pub under_fire: f32,
    pub hurt: f32,
    pub resupplying: bool,
    /// Standing in forest: harder to hit and to spot from the air.
    pub in_forest: bool,
    resupply_timer: f32,
    fire_timer: f32,
}

#[derive(Component, Debug)]
pub struct IrDrone {
    pub battery: f32,
    pub bombs: u32,
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Component)]
struct DroneBomb {
    vel: Vec3,
}

#[derive(Component)]
pub struct EnemyDrone {
    hp: f32,
    phase: f32,
}

#[derive(Resource, Default)]
struct EnemyDroneRespawn(Vec<f32>);

#[derive(Resource)]
struct InfantryAssets {
    drone: Handle<Mesh>,
    arm: Handle<Mesh>,
    bomb: Handle<Mesh>,
    dark: Handle<StandardMaterial>,
    enemy_light: Handle<StandardMaterial>,
}

#[derive(Component)]
struct RifleView;
#[derive(Component)]
struct InfantryHud;
#[derive(Component)]
struct HudOverlay;
#[derive(Component)]
struct HudCrosshair;
#[derive(Component)]
struct HudStatus;
#[derive(Component)]
struct HudAmmo;
#[derive(Component)]
struct HudWarn;

const HUD_GREEN: Color = Color::srgb(0.35, 1.0, 0.45);
const HUD_RED: Color = Color::srgb(1.0, 0.25, 0.2);

pub struct InfantryPlugin;

impl Plugin for InfantryPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<JoinInfantry>()
            .init_resource::<EnemyDroneRespawn>()
            .add_systems(Startup, (setup_assets, spawn_rifle_view))
            .add_systems(OnEnter(AppState::Playing), (spawn_enemy_drones, spawn_hud))
            .add_systems(OnExit(ViewMode::Infantry), leave_infantry)
            .add_systems(
                Update,
                (
                    auto_join,
                    join_infantry,
                    soldier_input.run_if(in_state(ViewMode::Infantry)).run_if(in_state(PlayState::Running)),
                    drone_bombs,
                    enemy_drones,
                    enemy_fire,
                    soldier_damage,
                    soldier_death,
                )
                    .chain()
                    .in_set(GameplaySet),
            )
            .add_systems(
                Update,
                infantry_camera
                    .run_if(in_state(ViewMode::Infantry))
                    .run_if(debug_cam_off)
                    .in_set(CameraSet::Position),
            )
            .add_systems(Update, (rifle_view, hud_update, drone_marker).after(CameraSet::Shake));
    }
}

// ---------------------------------------------------------------------------
// Pure rules (unit-tested)
// ---------------------------------------------------------------------------

/// Chance per second that an enemy unit with line of sight hits you.
pub fn enemy_hit_chance(attack: f32, strength: f32, dist: f32, range: f32, crouched: bool, spotted: bool, in_forest: bool) -> f64 {
    if dist >= range {
        return 0.0;
    }
    let falloff = (1.0 - dist / range).sqrt() as f64;
    let mut c = SOLDIER_HIT_CHANCE * (attack / 7.0) as f64 * (strength / 100.0).clamp(0.0, 1.0) as f64 * falloff;
    if crouched {
        c *= CROUCH_HIT_FACTOR;
    }
    if spotted {
        c *= SPOTTED_HIT_FACTOR;
    }
    if in_forest {
        c *= FOREST_HIT_FACTOR;
    }
    c.min(0.9)
}

/// A recon drone overhead sees you unless you are crouched and still.
/// Under the forest canopy it has to be almost on top of you.
pub fn drone_spots(horizontal_dist: f32, crouched: bool, moving: bool, in_forest: bool) -> bool {
    let range = if in_forest { ENEMY_DRONE_SPOT_RANGE * FOREST_SPOT_FACTOR } else { ENEMY_DRONE_SPOT_RANGE };
    horizontal_dist < range && (!crouched || moving)
}

/// Ground (or bridge deck) height for someone on foot.
fn foot_height(hm: &Heightmap, bridges: &Bridges, x: f32, z: f32) -> f32 {
    bridges.ground(hm, x, z)
}

fn flat_dirs(yaw: f32) -> (Vec3, Vec3) {
    let fwd = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
    let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
    (fwd, right)
}

fn look_rotation(yaw: f32, pitch: f32) -> Quat {
    Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0)
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

fn setup_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(InfantryAssets {
        drone: meshes.add(Cuboid::new(0.9, 0.3, 0.9)),
        arm: meshes.add(Cuboid::new(2.6, 0.08, 0.14)),
        bomb: meshes.add(Cuboid::new(0.15, 0.15, 0.45)),
        dark: materials.add(Color::srgb(0.12, 0.12, 0.13)),
        enemy_light: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.1, 0.1),
            emissive: LinearRgba::rgb(8.0, 0.4, 0.3),
            ..default()
        }),
    });
}

fn spawn_rifle_view(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let metal = materials.add(StandardMaterial {
        base_color: Color::srgb(0.10, 0.10, 0.11),
        perceptual_roughness: 0.6,
        ..default()
    });
    let furniture = materials.add(Color::srgb(0.28, 0.30, 0.22));
    commands
        .spawn((Name::new("rifle view"), RifleView, Transform::default(), Visibility::Hidden))
        .with_children(|p| {
            p.spawn((Mesh3d(meshes.add(Cuboid::new(0.05, 0.07, 0.55))), MeshMaterial3d(furniture.clone()), Transform::from_xyz(0.0, 0.0, 0.0)));
            p.spawn((Mesh3d(meshes.add(Cuboid::new(0.025, 0.025, 0.35))), MeshMaterial3d(metal.clone()), Transform::from_xyz(0.0, 0.015, -0.42)));
            p.spawn((Mesh3d(meshes.add(Cuboid::new(0.04, 0.12, 0.06))), MeshMaterial3d(metal.clone()), Transform::from_xyz(0.0, -0.08, -0.06)));
            p.spawn((Mesh3d(meshes.add(Cuboid::new(0.035, 0.05, 0.12))), MeshMaterial3d(metal.clone()), Transform::from_xyz(0.0, 0.06, -0.05)));
        });
}

fn spawn_enemy_drones(mut commands: Commands, hm: Res<Heightmap>, assets: Res<InfantryAssets>, mut respawn: ResMut<EnemyDroneRespawn>) {
    respawn.0.clear();
    for i in 0..ENEMY_DRONES {
        let (x, z) = (6_000.0 + i as f32 * 1_500.0, -2_000.0 + i as f32 * 2_500.0);
        spawn_enemy_drone(&mut commands, &assets, Vec3::new(x, hm.surface(x, z) + ENEMY_DRONE_AGL, z), i as f32 * std::f32::consts::PI);
    }
}

fn spawn_enemy_drone(commands: &mut Commands, a: &InfantryAssets, pos: Vec3, phase: f32) {
    commands
        .spawn((
            Name::new("enemy recon drone"),
            EnemyDrone { hp: ENEMY_DRONE_HP, phase },
            HitSphere { radius: 2.5, side: Some(Side::Enemy) },
            Mesh3d(a.drone.clone()),
            MeshMaterial3d(a.dark.clone()),
            Transform::from_translation(pos),
            StateScoped(AppState::Playing),
        ))
        .with_children(|p| {
            for r in [0.785f32, -0.785] {
                p.spawn((Mesh3d(a.arm.clone()), MeshMaterial3d(a.dark.clone()), Transform::from_rotation(Quat::from_rotation_y(r))));
            }
            p.spawn((Mesh3d(a.bomb.clone()), MeshMaterial3d(a.enemy_light.clone()), Transform::from_xyz(0.0, -0.2, 0.0)));
        });
}

fn spawn_hud(mut commands: Commands) {
    let font = |size: f32| TextFont { font_size: size, ..default() };
    let panel = BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.35));
    commands
        .spawn((
            Name::new("infantry hud"),
            InfantryHud,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            Visibility::Hidden,
            StateScoped(AppState::Playing),
        ))
        .with_children(|p| {
            p.spawn((
                HudOverlay,
                Node {
                    position_type: PositionType::Absolute,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(Color::NONE),
            ));
            p.spawn(Node {
                position_type: PositionType::Absolute,
                top: Val::Percent(50.0),
                margin: UiRect::top(Val::Px(-17.0)),
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                ..default()
            })
            .with_children(|c| {
                c.spawn((HudCrosshair, Text::new("+"), font(28.0), TextColor(HUD_GREEN)));
            });
            p.spawn((
                HudStatus,
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
                panel,
            ));
            p.spawn((
                HudAmmo,
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
                panel,
            ));
            p.spawn(Node {
                position_type: PositionType::Absolute,
                top: Val::Px(70.0),
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                ..default()
            })
            .with_children(|c| {
                c.spawn((HudWarn, Text::new(""), font(24.0), TextColor(HUD_RED), TextLayout::new_with_justify(JustifyText::Center)));
            });
        });
}

// ---------------------------------------------------------------------------
// Joining and leaving
// ---------------------------------------------------------------------------

/// WARGAME_VIEW=infantry|drone (see module docs).
fn auto_join(
    mut commands: Commands,
    time: Res<Time<Real>>,
    mut elapsed: Local<f32>,
    mut done: Local<u8>,
    units: Query<(Entity, &GroundUnit)>,
    mut soldier: Query<(&mut PlayerSoldier, &Transform)>,
    assets: Res<InfantryAssets>,
    mut ir: ResMut<IrView>,
    mut join: EventWriter<JoinInfantry>,
) {
    let Ok(view) = std::env::var("WARGAME_VIEW") else {
        return;
    };
    if view != "infantry" && view != "drone" {
        return;
    }
    *elapsed += time.delta_secs();
    if *done == 0 && *elapsed > 2.0 {
        if let Some((e, _)) = units.iter().find(|(_, u)| u.side == Side::Nato && u.kind == UnitKind::Infantry) {
            join.send(JoinInfantry(e));
            *done = 1;
        }
    } else if *done == 1 && view == "drone" {
        if let Ok((mut s, tf)) = soldier.get_single_mut() {
            launch_drone(&mut commands, &assets, &mut s, tf.translation, &mut ir);
            *done = 2;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn join_infantry(
    mut commands: Commands,
    mut events: EventReader<JoinInfantry>,
    hm: Res<Heightmap>,
    bridges: Res<Bridges>,
    view: Res<State<ViewMode>>,
    units: Query<(&GroundUnit, &Transform)>,
    existing: Query<(), With<PlayerSoldier>>,
    mut next: ResMut<NextState<ViewMode>>,
    mut log: ResMut<EventLog>,
) {
    for JoinInfantry(squad) in events.read() {
        if matches!(view.get(), ViewMode::Infantry | ViewMode::DeathCam) || !existing.is_empty() {
            continue;
        }
        let Ok((u, tf)) = units.get(*squad) else {
            continue;
        };
        if u.side != Side::Nato || u.kind != UnitKind::Infantry {
            continue;
        }
        // Behind the squad, facing the enemy (east).
        let (x, z) = (tf.translation.x - 25.0, tf.translation.z);
        let pos = Vec3::new(x, foot_height(&hm, &bridges, x, z) + 1.0, z);
        commands.spawn((
            Name::new("player soldier"),
            PlayerSoldier {
                squad: *squad,
                hp: SOLDIER_HP,
                yaw: -std::f32::consts::FRAC_PI_2,
                pitch: 0.0,
                eye: SOLDIER_EYE,
                crouched: false,
                moving: false,
                aiming: false,
                mag: RIFLE_MAG,
                reserve: RIFLE_RESERVE,
                reload: 0.0,
                fire_cd: 0.0,
                sound_cd: 0.0,
                drone_cd: 0.0,
                spotted: false,
                under_fire: 0.0,
                hurt: 0.0,
                resupplying: false,
                in_forest: false,
                resupply_timer: 0.0,
                fire_timer: 1.0,
            },
            HitSphere { radius: SOLDIER_HIT_RADIUS, side: Some(Side::Nato) },
            Transform::from_translation(pos),
            StateScoped(AppState::Playing),
        ));
        next.set(ViewMode::Infantry);
        log.push(format!("You join the {} on foot", u.name));
    }
}

/// Leaving infantry mode (map, death) takes the soldier and drone out.
fn leave_infantry(
    mut commands: Commands,
    soldiers: Query<Entity, With<PlayerSoldier>>,
    drones: Query<Entity, With<IrDrone>>,
    mut ir: ResMut<IrView>,
    mut cam: Query<&mut Projection, With<MainCamera>>,
) {
    for e in soldiers.iter().chain(drones.iter()) {
        commands.entity(e).despawn_recursive();
    }
    ir.0 = false;
    if let Ok(mut proj) = cam.get_single_mut() {
        if let Projection::Perspective(p) = proj.as_mut() {
            p.fov = DEFAULT_FOV_DEG.to_radians();
        }
    }
}

fn launch_drone(commands: &mut Commands, a: &InfantryAssets, s: &mut PlayerSoldier, pos: Vec3, ir: &mut IrView) {
    s.drone_cd = DRONE_COOLDOWN;
    s.aiming = false;
    commands
        .spawn((
            Name::new("ir drone"),
            IrDrone { battery: DRONE_BATTERY, bombs: DRONE_BOMBS, yaw: s.yaw, pitch: -1.0 },
            Mesh3d(a.drone.clone()),
            MeshMaterial3d(a.dark.clone()),
            Transform::from_translation(pos + Vec3::Y * DRONE_START_AGL),
            StateScoped(AppState::Playing),
        ))
        .with_children(|p| {
            for r in [0.785f32, -0.785] {
                p.spawn((Mesh3d(a.arm.clone()), MeshMaterial3d(a.dark.clone()), Transform::from_rotation(Quat::from_rotation_y(r))));
            }
        });
    ir.0 = true;
}

// ---------------------------------------------------------------------------
// Soldier and drone control
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn soldier_input(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    time: Res<Time>,
    hm: Res<Heightmap>,
    bridges: Res<Bridges>,
    assets: Res<InfantryAssets>,
    weapons: Res<WeaponAssets>,
    mut ir: ResMut<IrView>,
    mut soldiers: Query<(&mut PlayerSoldier, &mut Transform), Without<IrDrone>>,
    mut drones: Query<(Entity, &mut IrDrone, &mut Transform), Without<PlayerSoldier>>,
    units: Query<(&GroundUnit, &Transform), (Without<PlayerSoldier>, Without<IrDrone>)>,
    mut sounds: EventWriter<SoundEvent>,
) {
    let Ok((mut s, mut tf)) = soldiers.get_single_mut() else {
        return;
    };
    let dt = time.delta_secs();
    s.fire_cd -= dt;
    s.sound_cd -= dt;
    s.drone_cd -= dt;
    let axis = |neg: KeyCode, pos: KeyCode| (keys.pressed(pos) as i32 - keys.pressed(neg) as i32) as f32;
    let look = motion.delta * MOUSE_LOOK_SENS;

    // --- IR drone ---
    if let Ok((drone_e, mut d, mut dtf)) = drones.get_single_mut() {
        d.battery -= dt;
        d.yaw -= look.x;
        d.pitch = (d.pitch - look.y).clamp(-1.5, -0.1);
        let (fwd, right) = flat_dirs(d.yaw);
        let mv = (fwd * axis(KeyCode::KeyS, KeyCode::KeyW) + right * axis(KeyCode::KeyA, KeyCode::KeyD)).normalize_or_zero();
        let mut p = dtf.translation + mv * DRONE_SPEED * dt;
        p.y += axis(KeyCode::KeyQ, KeyCode::KeyE) * DRONE_SPEED * 0.6 * dt;
        // Stay within radio range of the soldier.
        let off = (p - tf.translation).xz();
        if off.length() > DRONE_MAX_RANGE {
            let c = off.normalize() * DRONE_MAX_RANGE;
            p.x = tf.translation.x + c.x;
            p.z = tf.translation.z + c.y;
        }
        let ground = hm.surface(p.x, p.z);
        p.y = p.y.clamp(ground + DRONE_MIN_AGL, ground + DRONE_MAX_AGL);
        dtf.translation = p;
        dtf.rotation = Quat::from_rotation_y(d.yaw);
        if mouse.just_pressed(MouseButton::Left) && d.bombs > 0 {
            d.bombs -= 1;
            commands.spawn((
                DroneBomb { vel: Vec3::ZERO },
                Mesh3d(assets.bomb.clone()),
                MeshMaterial3d(assets.dark.clone()),
                Transform::from_translation(p - Vec3::Y * 1.0).with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
                StateScoped(AppState::Playing),
            ));
        }
        if keys.just_pressed(KeyCode::KeyT) || d.battery <= 0.0 {
            commands.entity(drone_e).despawn_recursive();
            ir.0 = false;
            s.drone_cd = DRONE_COOLDOWN;
        }
        s.moving = false;
        return;
    }

    // --- Look, stance, movement ---
    s.aiming = mouse.pressed(MouseButton::Right);
    let sens = if s.aiming { 0.5 } else { 1.0 };
    s.yaw -= look.x * sens;
    s.pitch = (s.pitch - look.y * sens).clamp(-1.45, 1.45);
    if keys.just_pressed(KeyCode::KeyC) {
        s.crouched = !s.crouched;
    }
    let sprint = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]) && !s.aiming;
    if sprint {
        s.crouched = false;
    }
    let speed = if s.crouched {
        SOLDIER_CROUCH_SPEED
    } else if sprint {
        SOLDIER_SPRINT
    } else {
        SOLDIER_WALK
    };
    let (fwd, right) = flat_dirs(s.yaw);
    let mv = (fwd * axis(KeyCode::KeyS, KeyCode::KeyW) + right * axis(KeyCode::KeyA, KeyCode::KeyD)).normalize_or_zero();
    s.moving = mv != Vec3::ZERO;
    let next = tf.translation + mv * speed * dt;
    let wet = hm.is_water(next.x, next.z) && bridges.deck_at(next.x, next.z).is_none();
    let (x, z) = if wet { (tf.translation.x, tf.translation.z) } else { (next.x, next.z) };
    tf.translation = Vec3::new(x, foot_height(&hm, &bridges, x, z) + 1.0, z);
    let eye = if s.crouched { SOLDIER_EYE_CROUCHED } else { SOLDIER_EYE };
    s.eye += (eye - s.eye) * (1.0 - (-10.0 * dt).exp());

    // Near any friendly unit: top up rifle ammo.
    let pos = tf.translation;
    s.resupplying = s.reserve < RIFLE_RESERVE
        && units.iter().any(|(u, utf)| u.side == Side::Nato && utf.translation.xz().distance(pos.xz()) < 80.0);
    if s.resupplying {
        s.resupply_timer += dt * 15.0; // rounds per second
        let n = s.resupply_timer as u32;
        s.resupply_timer -= n as f32;
        s.reserve = (s.reserve + n).min(RIFLE_RESERVE);
    }

    // --- Rifle ---
    if s.reload > 0.0 {
        s.reload -= dt;
        if s.reload <= 0.0 {
            let take = (RIFLE_MAG - s.mag).min(s.reserve);
            s.mag += take;
            s.reserve -= take;
        }
    } else if keys.just_pressed(KeyCode::KeyR) && s.mag < RIFLE_MAG && s.reserve > 0 {
        s.reload = RIFLE_RELOAD;
    } else if mouse.pressed(MouseButton::Left) {
        if s.mag == 0 {
            if s.reserve > 0 {
                s.reload = RIFLE_RELOAD;
            }
        } else if s.fire_cd <= 0.0 {
            s.fire_cd = 1.0 / RIFLE_RATE;
            s.mag -= 1;
            let mut rng = rand::thread_rng();
            let spread = if s.crouched || s.aiming { RIFLE_SPREAD_CROUCHED } else { RIFLE_SPREAD };
            let jitter = Vec3::new(rng.gen_range(-1.0..1.0), rng.gen_range(-1.0..1.0), 0.0) * spread;
            let rot = look_rotation(s.yaw, s.pitch);
            let dir = (rot * (Vec3::NEG_Z + jitter)).normalize();
            let eye_pos = pos + Vec3::Y * (s.eye - 1.0);
            // Start the 14 m tracer ahead of the camera so it never fills the view.
            spawn_bullet(&mut commands, &weapons, eye_pos + dir * 7.5, dir * RIFLE_MUZZLE_SPEED, Side::Nato, true, RIFLE_DAMAGE);
            s.pitch = (s.pitch + 0.004).min(1.45);
            if s.sound_cd <= 0.0 {
                s.sound_cd = 0.18;
                sounds.send(SoundEvent::ui(Sfx::GunBurst));
            }
        }
    }

    if keys.just_pressed(KeyCode::KeyT) && s.drone_cd <= 0.0 {
        launch_drone(&mut commands, &assets, &mut s, pos, &mut ir);
    }
}

fn drone_bombs(
    mut commands: Commands,
    time: Res<Time>,
    hm: Res<Heightmap>,
    mut bombs: Query<(Entity, &mut DroneBomb, &mut Transform)>,
    mut detonations: EventWriter<Detonation>,
) {
    let dt = time.delta_secs();
    for (e, mut b, mut tf) in &mut bombs {
        b.vel.y -= GRAVITY * dt;
        tf.translation += b.vel * dt;
        let p = tf.translation;
        if p.y <= hm.surface(p.x, p.z) {
            detonations.send(Detonation {
                pos: Vec3::new(p.x, hm.surface(p.x, p.z) + 0.5, p.z),
                size: Boom::Medium,
                damage: DRONE_BOMB_DAMAGE,
                radius: DRONE_BOMB_RADIUS,
                by_player: true,
            });
            commands.entity(e).despawn_recursive();
        }
    }
}

// ---------------------------------------------------------------------------
// Threats: recon drones and enemy fire
// ---------------------------------------------------------------------------

/// Recon drones circle over the enemy front and hunt the soldier when he
/// is near it. Any drone close overhead can spot him.
#[allow(clippy::too_many_arguments)]
fn enemy_drones(
    mut commands: Commands,
    time: Res<Time>,
    hm: Res<Heightmap>,
    nav: Res<NavGrid>,
    assets: Res<InfantryAssets>,
    mut respawn: ResMut<EnemyDroneRespawn>,
    mut drones: Query<(&mut EnemyDrone, &mut Transform), Without<PlayerSoldier>>,
    mut soldier: Query<(&mut PlayerSoldier, &Transform)>,
    units: Query<(&GroundUnit, &Transform), (Without<PlayerSoldier>, Without<EnemyDrone>)>,
) {
    let dt = time.delta_secs();
    let enemies: Vec<Vec3> = units.iter().filter(|(u, _)| u.side == Side::Enemy).map(|(_, tf)| tf.translation).collect();
    let front = if enemies.is_empty() {
        base_front()
    } else {
        enemies.iter().copied().sum::<Vec3>() / enemies.len() as f32
    };
    let mut soldier = soldier.get_single_mut().ok();
    let hunt = soldier
        .as_ref()
        .map(|(_, tf)| tf.translation)
        .filter(|p| enemies.iter().any(|e| e.xz().distance(p.xz()) < 4_000.0));
    let mut spotted = false;
    for (mut d, mut tf) in &mut drones {
        d.phase += dt * 0.08;
        let center = hunt.unwrap_or(front);
        let target = center + Vec3::new(d.phase.cos(), 0.0, d.phase.sin()) * 300.0;
        let to = (target - tf.translation).xz();
        let step = to.clamp_length_max(ENEMY_DRONE_SPEED * dt);
        let p = tf.translation + Vec3::new(step.x, 0.0, step.y);
        let y = hm.surface(p.x, p.z) + ENEMY_DRONE_AGL;
        tf.translation = Vec3::new(p.x, tf.translation.y + (y - tf.translation.y) * (1.0 - (-dt).exp()), p.z);
        tf.rotate_y(dt * 0.3);
        if let Some((s, stf)) = &soldier {
            if drone_spots(tf.translation.xz().distance(stf.translation.xz()), s.crouched, s.moving, nav.forest_at(stf.translation.xz())) {
                spotted = true;
            }
        }
    }
    if let Some((s, _)) = soldier.as_mut() {
        s.spotted = spotted;
    }
    // Shot-down drones come back after a while.
    let mut i = 0;
    while i < respawn.0.len() {
        respawn.0[i] -= dt;
        if respawn.0[i] <= 0.0 {
            respawn.0.swap_remove(i);
            let p = front + Vec3::new(1_500.0, 0.0, 0.0);
            let pos = Vec3::new(p.x, hm.surface(p.x, p.z) + ENEMY_DRONE_AGL, p.z);
            spawn_enemy_drone(&mut commands, &assets, pos, rand::thread_rng().gen_range(0.0..std::f32::consts::TAU));
        } else {
            i += 1;
        }
    }
}

fn base_front() -> Vec3 {
    Vec3::new(FRONT_CENTER[0] + 2_000.0, 0.0, FRONT_CENTER[1])
}

/// Once a second every enemy unit in range with a clear line of sight may
/// hit you. Crouching and staying unspotted make you much harder to hit;
/// hills block the view entirely.
#[allow(clippy::too_many_arguments)]
fn enemy_fire(
    time: Res<Time>,
    hm: Res<Heightmap>,
    nav: Res<NavGrid>,
    cheats: Res<Cheats>,
    mut soldier: Query<(&mut PlayerSoldier, &Transform)>,
    units: Query<(&GroundUnit, &Transform)>,
    mut detonations: EventWriter<Detonation>,
    mut particles: ResMut<Particles>,
    mut sounds: EventWriter<SoundEvent>,
) {
    let Ok((mut s, tf)) = soldier.get_single_mut() else {
        return;
    };
    let dt = time.delta_secs();
    s.in_forest = nav.forest_at(tf.translation.xz());
    s.under_fire -= dt;
    s.hurt = (s.hurt - dt * 1.5).max(0.0);
    s.fire_timer -= dt;
    if s.fire_timer > 0.0 {
        return;
    }
    s.fire_timer = 1.0;
    let eye = tf.translation + Vec3::Y * (s.eye - 1.0);
    let mut rng = rand::thread_rng();
    for (u, utf) in &units {
        if u.side != Side::Enemy || u.retreating {
            continue;
        }
        let stats = u.stats();
        let muzzle = utf.translation + Vec3::Y * 2.5;
        let dist = muzzle.distance(eye);
        if dist >= stats.range {
            continue;
        }
        let blocked = hm.raycast(muzzle, eye - muzzle, dist).is_some_and(|h| h.distance(muzzle) < dist - 4.0);
        if blocked {
            continue;
        }
        let chance = enemy_hit_chance(stats.attack, u.strength, dist, stats.range, s.crouched, s.spotted, s.in_forest);
        s.under_fire = 2.0;
        let armor = u.kind == UnitKind::Armor;
        if rng.gen_bool(chance) {
            if !cheats.god_mode {
                s.hp -= if armor { ENEMY_SHELL_DAMAGE } else { ENEMY_RIFLE_DAMAGE };
            }
            s.hurt = 1.0;
            sounds.send(SoundEvent::ui(Sfx::Hit));
            let near = eye + Vec3::new(rng.gen_range(-3.0..3.0), -1.0, rng.gen_range(-3.0..3.0));
            detonations.send(Detonation::fx(near, if armor { Boom::Medium } else { Boom::Spark }));
        } else if rng.gen_bool(0.6) {
            // Near miss: dirt kicked up around you.
            let miss = Vec3::new(rng.gen_range(-15.0..15.0), 0.0, rng.gen_range(-15.0..15.0));
            let p = tf.translation + miss;
            let ground = Vec3::new(p.x, hm.surface(p.x, p.z), p.z);
            if armor {
                detonations.send(Detonation::fx(ground + Vec3::Y * 2.0, Boom::Medium));
            } else {
                particles.dust(ground);
            }
        }
        if rng.gen_bool(0.5) {
            sounds.send(SoundEvent::at(Sfx::GunBurst, muzzle));
        }
    }
}

/// Weapons damage (jet guns, blasts, your own drone bombs) on the soldier
/// and on recon drones.
#[allow(clippy::too_many_arguments)]
fn soldier_damage(
    mut commands: Commands,
    mut events: EventReader<DamageEvent>,
    cheats: Res<Cheats>,
    mut soldier: Query<&mut PlayerSoldier>,
    mut drones: Query<(&mut EnemyDrone, &Transform)>,
    mut respawn: ResMut<EnemyDroneRespawn>,
    mut detonations: EventWriter<Detonation>,
    mut log: ResMut<EventLog>,
    mut sounds: EventWriter<SoundEvent>,
) {
    for ev in events.read() {
        if let Ok(mut s) = soldier.get_mut(ev.target) {
            if cheats.god_mode || ev.kind == DamageKind::Crash {
                continue;
            }
            let factor = if ev.kind == DamageKind::Bullet { SOLDIER_BULLET_FACTOR } else { SOLDIER_BLAST_FACTOR };
            s.hp -= ev.amount * factor;
            s.hurt = 1.0;
            sounds.send(SoundEvent::ui(Sfx::Hit));
        } else if let Ok((mut d, tf)) = drones.get_mut(ev.target) {
            if d.hp <= 0.0 {
                continue;
            }
            d.hp -= ev.amount;
            if d.hp <= 0.0 {
                detonations.send(Detonation::fx(tf.translation, Boom::Medium));
                commands.entity(ev.target).despawn_recursive();
                respawn.0.push(ENEMY_DRONE_RESPAWN);
                log.push(if ev.by_player { "You shot down an enemy recon drone" } else { "Enemy recon drone down" });
            }
        }
    }
}

fn soldier_death(
    soldier: Query<(&PlayerSoldier, &Transform)>,
    mut units: Query<&mut GroundUnit>,
    mut status: ResMut<PlayerStatus>,
    mut view: ResMut<NextState<ViewMode>>,
    mut log: ResMut<EventLog>,
    mut done: Local<bool>,
) {
    let Ok((s, tf)) = soldier.get_single() else {
        *done = false;
        return;
    };
    if s.hp > 0.0 || *done {
        return;
    }
    *done = true;
    if let Ok(mut u) = units.get_mut(s.squad) {
        u.strength -= SOLDIER_LOSS_STRENGTH;
    }
    status.death_pos = tf.translation;
    status.death_timer = DEATH_CAM_TIME;
    view.set(ViewMode::DeathCam);
    log.push("You were killed in action");
}

// ---------------------------------------------------------------------------
// Camera, view model, HUD
// ---------------------------------------------------------------------------

fn infantry_camera(
    time: Res<Time<Real>>,
    soldier: Query<(&PlayerSoldier, &Transform), Without<MainCamera>>,
    drone: Query<(&IrDrone, &Transform), Without<MainCamera>>,
    mut cam: Query<(&mut Transform, &mut Projection), With<MainCamera>>,
) {
    let Ok((mut ctf, mut proj)) = cam.get_single_mut() else {
        return;
    };
    let mut want_fov = DEFAULT_FOV_DEG;
    if let Ok((d, dtf)) = drone.get_single() {
        ctf.translation = dtf.translation - Vec3::Y * 0.4;
        ctf.rotation = look_rotation(d.yaw, d.pitch);
    } else if let Ok((s, stf)) = soldier.get_single() {
        ctf.translation = stf.translation + Vec3::Y * (s.eye - 1.0);
        ctf.rotation = look_rotation(s.yaw, s.pitch);
        if s.aiming {
            want_fov = AIM_FOV_DEG;
        }
    }
    if let Projection::Perspective(p) = proj.as_mut() {
        let k = 1.0 - (-12.0 * time.delta_secs()).exp();
        p.fov += (want_fov.to_radians() - p.fov) * k;
    }
}

#[allow(clippy::type_complexity)]
fn rifle_view(
    view: Option<Res<State<ViewMode>>>,
    debug: Res<DebugCamActive>,
    soldier: Query<&PlayerSoldier>,
    drone: Query<(), With<IrDrone>>,
    cam: Query<&Transform, (With<MainCamera>, Without<RifleView>)>,
    mut rifle: Query<(&mut Transform, &mut Visibility), With<RifleView>>,
) {
    let Ok((mut tf, mut vis)) = rifle.get_single_mut() else {
        return;
    };
    let on_foot = view.is_some_and(|v| *v.get() == ViewMode::Infantry) && !debug.0 && drone.is_empty();
    match (soldier.get_single(), cam.get_single()) {
        (Ok(s), Ok(cam)) if on_foot => {
            let offset = if s.aiming {
                Vec3::new(0.0, -0.075, -0.32)
            } else if s.reload > 0.0 {
                Vec3::new(0.16, -0.26, -0.4)
            } else {
                Vec3::new(0.15, -0.16, -0.4)
            };
            *tf = *cam * Transform::from_translation(offset).with_scale(Vec3::splat(0.6));
            *vis = Visibility::Visible;
        }
        _ => *vis = Visibility::Hidden,
    }
}

/// Bomb-drop marker straight below the IR drone.
fn drone_marker(mut gizmos: Gizmos, hm: Res<Heightmap>, drone: Query<(&IrDrone, &Transform)>) {
    let Ok((d, tf)) = drone.get_single() else {
        return;
    };
    let p = tf.translation;
    let g = Vec3::new(p.x, hm.surface(p.x, p.z) + 0.5, p.z);
    let col = if d.bombs > 0 { Color::srgb(1.0, 0.3, 0.2) } else { Color::srgb(0.5, 0.5, 0.5) };
    let pts: Vec<Vec3> = (0..=24)
        .map(|i| {
            let a = i as f32 / 24.0 * std::f32::consts::TAU;
            g + Vec3::new(a.cos(), 0.0, a.sin()) * DRONE_BOMB_RADIUS
        })
        .collect();
    gizmos.linestrip(pts, col);
    gizmos.line(g - Vec3::X * 6.0, g + Vec3::X * 6.0, col);
    gizmos.line(g - Vec3::Z * 6.0, g + Vec3::Z * 6.0, col);
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn hud_update(
    time: Res<Time<Real>>,
    view: Option<Res<State<ViewMode>>>,
    debug: Res<DebugCamActive>,
    soldier: Query<&PlayerSoldier>,
    drone: Query<&IrDrone>,
    units: Query<&GroundUnit>,
    mut root: Query<&mut Visibility, With<InfantryHud>>,
    mut overlay: Query<&mut BackgroundColor, (With<HudOverlay>, Without<HudStatus>, Without<HudAmmo>)>,
    mut texts: ParamSet<(
        Query<&mut Text, With<HudStatus>>,
        Query<(&mut Text, &mut TextColor), With<HudAmmo>>,
        Query<&mut Text, With<HudWarn>>,
        Query<&mut Visibility, (With<HudCrosshair>, Without<InfantryHud>)>,
    )>,
) {
    let Ok(mut vis) = root.get_single_mut() else {
        return;
    };
    let show = view.is_some_and(|v| *v.get() == ViewMode::Infantry) && !debug.0;
    let Ok(s) = soldier.get_single() else {
        *vis = Visibility::Hidden;
        return;
    };
    *vis = if show { Visibility::Inherited } else { Visibility::Hidden };
    if !show {
        return;
    }
    let drone = drone.get_single().ok();
    if let Ok(mut bg) = overlay.get_single_mut() {
        bg.0 = if drone.is_some() {
            Color::srgba(0.05, 0.2, 0.08, 0.35)
        } else {
            Color::srgba(0.6, 0.0, 0.0, s.hurt * 0.35)
        };
    }

    let squad = match units.get(s.squad) {
        Ok(u) => format!("{} ({:.0}%)", u.name, u.strength.max(0.0)),
        Err(_) => "squad wiped out".into(),
    };
    let status = match drone {
        Some(d) => format!(
            "IR DRONE  battery {:.0}s  bombs {}\nWASD move | Q/E down/up | mouse look\nLMB drop bomb | T return to soldier\nSquad: {squad}",
            d.battery.max(0.0),
            d.bombs
        ),
        None => {
            let drone_line = if s.drone_cd > 0.0 {
                format!("IR drone: recharging {:.0}s", s.drone_cd)
            } else {
                "IR drone: ready (T)".into()
            };
            format!(
                "HP {:.0}\n{}{}\nSquad: {squad}\n{drone_line}",
                s.hp.max(0.0),
                if s.crouched { "CROUCHED" } else { "STANDING" },
                if s.in_forest { "  IN FOREST (cover)" } else { "" },
            )
        }
    };
    texts.p0().single_mut().0 = status;

    {
        let mut q = texts.p1();
        let (mut t, mut c) = q.single_mut();
        t.0 = format!(
            "RIFLE {:>2} / {}{}{}",
            s.mag,
            s.reserve,
            if s.reload > 0.0 { "\nRELOADING" } else { "" },
            if s.resupplying { "\nRESUPPLYING" } else { "" }
        );
        c.0 = if s.mag == 0 { HUD_RED } else { HUD_GREEN };
    }

    let blink = (time.elapsed_secs() * 3.0).fract() < 0.6;
    let mut warns: Vec<&str> = Vec::new();
    if s.spotted {
        warns.push("SPOTTED BY DRONE");
    }
    if s.under_fire > 0.0 {
        warns.push("UNDER FIRE");
    }
    if s.hp < 35.0 {
        warns.push("WOUNDED");
    }
    texts.p2().single_mut().0 = if blink { warns.join("   ") } else { String::new() };
    if let Ok(mut cv) = texts.p3().get_single_mut() {
        *cv = if drone.is_some() || s.aiming { Visibility::Hidden } else { Visibility::Inherited };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_and_crouching_reduce_hits() {
        let open = enemy_hit_chance(7.0, 100.0, 500.0, 2_000.0, false, false, false);
        let low = enemy_hit_chance(7.0, 100.0, 500.0, 2_000.0, true, false, false);
        let spotted = enemy_hit_chance(7.0, 100.0, 500.0, 2_000.0, true, true, false);
        assert!(low < open && spotted > low);
        assert_eq!(enemy_hit_chance(7.0, 100.0, 2_000.0, 2_000.0, false, true, false), 0.0);
        // Weaker units and longer ranges hit less often.
        assert!(enemy_hit_chance(7.0, 50.0, 500.0, 2_000.0, false, false, false) < open);
        assert!(enemy_hit_chance(7.0, 100.0, 1_500.0, 2_000.0, false, false, false) < open);
    }

    #[test]
    fn forest_gives_cover() {
        let open = enemy_hit_chance(7.0, 100.0, 500.0, 2_000.0, false, false, false);
        let trees = enemy_hit_chance(7.0, 100.0, 500.0, 2_000.0, false, false, true);
        assert!(trees < open);
        assert!(drone_spots(400.0, false, false, false));
        assert!(!drone_spots(400.0, false, false, true));
    }

    #[test]
    fn still_crouched_soldiers_hide_from_drones() {
        assert!(drone_spots(100.0, false, false, false));
        assert!(drone_spots(100.0, true, true, false));
        assert!(!drone_spots(100.0, true, false, false));
        assert!(!drone_spots(ENEMY_DRONE_SPOT_RANGE + 1.0, false, true, false));
    }

    #[test]
    fn flat_dirs_match_camera() {
        for yaw in [0.0f32, 0.7, -2.0] {
            let (fwd, right) = flat_dirs(yaw);
            let rot = look_rotation(yaw, 0.0);
            assert!(fwd.distance(rot * Vec3::NEG_Z) < 1e-5);
            assert!(right.distance(rot * Vec3::X) < 1e-5);
        }
    }
}
