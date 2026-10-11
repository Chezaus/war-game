//! Flight model (§5.1): atmosphere, thrust, lift with stall, drag, gravity,
//! rate-commanded rotation with a G-limiter, G tolerance, fuel and leaks,
//! ground contact, and airbase restock (§5.4).
//!
//! Player and AI jets share all of this; they only differ in who writes
//! the `ControlInput`.

use bevy::prelude::*;

use crate::config::*;
use crate::debug::Cheats;
use crate::heightgen::smoothstep;
use crate::player::Player;
use crate::state::{AppState, GameplaySet};
use crate::weapons::{DamageEvent, DamageKind, Health, HitSphere, JetDamage, LockState, Targetable, Weapons};
use crate::world::{on_runway, Heightmap};

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

#[derive(Component, Default, Clone, Copy, Debug)]
pub struct Velocity(pub Vec3);

#[derive(Component)]
pub struct Jet {
    pub side: Side,
    pub stats: JetStats,
}

/// Stick and switches. Written by player input or the AI pilot.
/// `fire_missile`, `flare`, `toggle_gear` and `toggle_airbrake` are
/// one-shot latches: whoever acts on them clears them.
#[derive(Component, Default, Debug, Clone)]
pub struct ControlInput {
    /// +1 = nose up.
    pub pitch: f32,
    /// +1 = roll right.
    pub roll: f32,
    /// +1 = nose right.
    pub yaw: f32,
    /// Commanded throttle 0..1 (above 0.9 = afterburner).
    pub throttle: f32,
    pub fire_gun: bool,
    pub fire_missile: bool,
    pub flare: bool,
    pub toggle_gear: bool,
    pub toggle_airbrake: bool,
}

#[derive(Component, Debug, Clone)]
pub struct FlightState {
    pub throttle: f32,
    pub fuel: f32,
    pub gear_down: bool,
    pub airbrake: bool,
    pub on_ground: bool,
    pub engine_out: bool,
    pub stalled: bool,
    pub g_load: f32,
    pub aoa: f32,
    /// Actual (pitch, yaw, roll) rates, rad/s.
    pub rates: Vec3,
    /// 0..1 control authority left after blackout/red-out.
    pub control_auth: f32,
    /// Last computed forces, for the debug overlay.
    pub debug_lift: Vec3,
    pub debug_drag: Vec3,
    pub debug_thrust: Vec3,
}

impl FlightState {
    pub fn new(stats: &JetStats, on_ground: bool) -> Self {
        Self {
            throttle: if on_ground { 0.0 } else { 0.75 },
            fuel: stats.max_fuel,
            gear_down: on_ground,
            airbrake: false,
            on_ground,
            engine_out: false,
            stalled: false,
            g_load: 1.0,
            aoa: 0.0,
            rates: Vec3::ZERO,
            control_auth: 1.0,
            debug_lift: Vec3::ZERO,
            debug_drag: Vec3::ZERO,
            debug_thrust: Vec3::ZERO,
        }
    }
}

/// Blackout (positive G) and red-out (negative G) meters, 0..1.
#[derive(Component, Default, Debug)]
pub struct GTolerance {
    pub blackout: f32,
    pub redout: f32,
}

/// Restock progress while parked on the own runway (§5.4).
#[derive(Component, Default, Debug)]
pub struct Restock {
    pub stopped: f32,
    pub progress: f32,
    /// Nothing to restock (and no restock in progress).
    pub full: bool,
}

impl Restock {
    pub fn active(&self) -> bool {
        self.stopped >= RESTOCK_WAIT
    }
}

#[derive(Component)]
pub struct AfterburnerGlow;

/// Multipliers on the aero constants, tweakable from the debug panel.
#[derive(Resource, Debug, Clone)]
pub struct AeroTuning {
    pub lift: f32,
    pub drag: f32,
    pub thrust: f32,
    pub rates: f32,
    pub g_limit: f32,
}

impl Default for AeroTuning {
    fn default() -> Self {
        Self {
            lift: 1.0,
            drag: 1.0,
            thrust: 1.0,
            rates: 1.0,
            g_limit: 1.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure aero math (unit-tested)
// ---------------------------------------------------------------------------

pub fn air_density(alt: f32) -> f32 {
    RHO0 * (-alt.max(0.0) / ATMOS_SCALE_HEIGHT).exp()
}

/// Lift coefficient with a linear range and a post-stall drop-off.
pub fn lift_coefficient(aoa: f32, stats: &JetStats) -> f32 {
    let a = aoa.abs();
    let cl = if a <= stats.stall_aoa {
        stats.cl_alpha * a
    } else {
        let peak = stats.cl_alpha * stats.stall_aoa;
        let t = ((a - stats.stall_aoa) / STALL_DROP).min(1.0);
        let post = peak * (1.0 - (1.0 - POST_STALL_LIFT) * t);
        // Fades to zero towards 90° (flat plate falling).
        post * (1.0 - smoothstep(0.9, std::f32::consts::FRAC_PI_2, a))
    };
    cl * aoa.signum()
}

pub fn drag_coefficient(cl: f32, aoa: f32, mach: f32, stats: &JetStats, gear: bool, airbrake: bool) -> f32 {
    let mut cd = stats.cd0 + stats.induced_k * cl * cl + CD_WAVE * smoothstep(0.9, 1.1, mach);
    if gear {
        cd += CD_GEAR;
    }
    if airbrake {
        cd += CD_AIRBRAKE;
    }
    // Beyond the stall the wing is a barn door.
    let s = aoa.sin();
    cd + 1.2 * s * s * smoothstep(stats.stall_aoa * 0.8, stats.stall_aoa * 1.3, aoa.abs())
}

pub fn thrust_newtons(throttle: f32, alt: f32, stats: &JetStats) -> f32 {
    let t = throttle.clamp(0.0, 1.0);
    let base = if t <= AFTERBURNER_THRESHOLD {
        stats.thrust_dry * t / AFTERBURNER_THRESHOLD
    } else {
        stats.thrust_dry
            + (stats.thrust_ab - stats.thrust_dry) * (t - AFTERBURNER_THRESHOLD) / (1.0 - AFTERBURNER_THRESHOLD)
    };
    base * (air_density(alt) / RHO0).powf(THRUST_LAPSE)
}

pub fn fuel_flow(throttle: f32, stats: &JetStats) -> f32 {
    let t = throttle.clamp(0.0, 1.0);
    if t <= AFTERBURNER_THRESHOLD {
        stats.fuel_flow_idle + (stats.fuel_flow_dry - stats.fuel_flow_idle) * t / AFTERBURNER_THRESHOLD
    } else {
        stats.fuel_flow_dry
            + (stats.fuel_flow_ab - stats.fuel_flow_dry) * (t - AFTERBURNER_THRESHOLD) / (1.0 - AFTERBURNER_THRESHOLD)
    }
}

pub struct AeroForces {
    pub lift: Vec3,
    pub drag: Vec3,
    pub side: Vec3,
    pub aoa: f32,
    pub sideslip: f32,
}

pub fn aero_forces(rot: Quat, vel: Vec3, alt: f32, stats: &JetStats, gear: bool, airbrake: bool, tune: &AeroTuning) -> AeroForces {
    let speed = vel.length();
    if speed < 0.5 {
        return AeroForces {
            lift: Vec3::ZERO,
            drag: Vec3::ZERO,
            side: Vec3::ZERO,
            aoa: 0.0,
            sideslip: 0.0,
        };
    }
    let local = rot.inverse() * vel;
    let aoa = (-local.y).atan2(-local.z);
    let sideslip = local.x.atan2(-local.z);
    let vdir = vel / speed;
    let right = rot * Vec3::X;
    let q = 0.5 * air_density(alt) * speed * speed;
    let qs = q * stats.wing_area;
    let cl = lift_coefficient(aoa, stats) * tune.lift;
    let cd = drag_coefficient(cl, aoa, speed / SPEED_OF_SOUND, stats, gear, airbrake) * tune.drag;
    let lift_dir = right.cross(vdir).normalize_or_zero();
    AeroForces {
        lift: lift_dir * qs * cl,
        drag: -vdir * qs * cd,
        side: -right * qs * SIDE_FORCE_COEF * 0.3 * sideslip.sin(),
        aoa,
        sideslip,
    }
}

/// Control authority from airspeed and air density: sluggish when slow.
pub fn control_authority(speed: f32, alt: f32, stats: &JetStats) -> f32 {
    let s = (speed / stats.full_authority_speed).clamp(0.04, 1.0);
    s * (air_density(alt) / RHO0).sqrt().max(0.4)
}

/// One airborne physics step (no ground contact). Mutates transform,
/// velocity and state; returns nothing, as everything lands in `st`.
#[allow(clippy::too_many_arguments)]
pub fn step_air(
    tf: &mut Transform,
    vel: &mut Vec3,
    st: &mut FlightState,
    input: &ControlInput,
    stats: &JetStats,
    thrust_scale: f32,
    tune: &AeroTuning,
    dt: f32,
) {
    let alt = tf.translation.y;
    let mass = stats.empty_mass + st.fuel;
    let speed = vel.length();
    let up = tf.rotation * Vec3::Y;
    let fwd = tf.rotation * Vec3::NEG_Z;

    // Forces.
    let aero = aero_forces(tf.rotation, *vel, alt, stats, st.gear_down, st.airbrake, tune);
    let thrust = if st.engine_out {
        0.0
    } else {
        thrust_newtons(st.throttle, alt, stats) * thrust_scale * tune.thrust
    };
    let thrust_v = fwd * thrust;
    let aero_total = aero.lift + aero.drag + aero.side + thrust_v;
    let accel = aero_total / mass + Vec3::new(0.0, -GRAVITY, 0.0);
    st.g_load = (aero_total / mass).dot(up) / GRAVITY;
    st.aoa = aero.aoa;
    st.stalled = aero.aoa.abs() > stats.stall_aoa && speed > 5.0;
    st.debug_lift = aero.lift;
    st.debug_drag = aero.drag;
    st.debug_thrust = thrust_v;

    // Rotation: commanded rates scaled by airspeed, capped by the G-limiter.
    let auth = control_authority(speed, alt, stats) * st.control_auth * tune.rates;
    let v = speed.max(40.0);
    let level_g = up.y;
    let max_g = stats.max_g * tune.g_limit;
    let w_max = ((max_g - level_g) * GRAVITY / v).max(0.05);
    let w_min = ((stats.min_g - level_g) * GRAVITY / v).min(-0.05);
    let mut pitch = (input.pitch.clamp(-1.0, 1.0) * stats.max_pitch_rate * auth).clamp(w_min, w_max);
    let roll = input.roll.clamp(-1.0, 1.0) * stats.max_roll_rate * auth;
    let mut yaw = input.yaw.clamp(-1.0, 1.0) * stats.max_yaw_rate * auth;

    // Weathervane: past the stall (or when slow) the nose falls towards
    // the flight path; sideslip is always damped.
    let speed_factor = (speed / 80.0).clamp(0.3, 1.5);
    let stall_factor = smoothstep(stats.stall_aoa * 0.9, stats.stall_aoa * 1.4, aero.aoa.abs());
    pitch -= aero.aoa * WEATHERVANE_PITCH * stall_factor * speed_factor;
    yaw += aero.sideslip * WEATHERVANE_YAW * speed_factor;

    let target = Vec3::new(pitch, yaw, roll);
    st.rates = st.rates.lerp(target, (RATE_RESPONSE * dt).min(1.0));
    let r = st.rates;
    tf.rotation = (tf.rotation * Quat::from_scaled_axis(Vec3::new(r.x, -r.y, -r.z) * dt)).normalize();

    *vel += accel * dt;
    tf.translation += *vel * dt;
}

// ---------------------------------------------------------------------------
// Plugin and systems
// ---------------------------------------------------------------------------

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum FlightSet {
    /// AI and autopilot write ControlInput.
    Control,
    /// Physics.
    Integrate,
}

pub struct FlightPlugin;

impl Plugin for FlightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AeroTuning>()
            .add_systems(Startup, setup_jet_assets)
            .configure_sets(
                FixedUpdate,
                (FlightSet::Control, FlightSet::Integrate).chain().in_set(GameplaySet),
            )
            .add_systems(
                FixedUpdate,
                (fly_jets, g_tolerance, restock).chain().in_set(FlightSet::Integrate),
            )
            .add_systems(Update, afterburner_glow.in_set(GameplaySet));
    }
}

#[allow(clippy::type_complexity)]
fn fly_jets(
    time: Res<Time>,
    hm: Res<Heightmap>,
    tune: Res<AeroTuning>,
    cheats: Res<Cheats>,
    mut q: Query<(
        Entity,
        &Jet,
        &mut Transform,
        &mut Velocity,
        &mut FlightState,
        &mut ControlInput,
        &JetDamage,
        Has<Player>,
    )>,
    mut damage: EventWriter<DamageEvent>,
) {
    let dt = time.delta_secs();
    for (entity, jet, mut tf, mut vel, mut st, mut input, dmg, is_player) in &mut q {
        let stats = jet.stats;

        // Switches.
        if std::mem::take(&mut input.toggle_gear) && !st.on_ground {
            st.gear_down = !st.gear_down;
        }
        if std::mem::take(&mut input.toggle_airbrake) {
            st.airbrake = !st.airbrake;
        }

        // Engine spool and fuel.
        let target = input.throttle.clamp(0.0, 1.0);
        st.throttle += (target - st.throttle).clamp(-dt * 0.8, dt * 0.8);
        let infinite = is_player && cheats.infinite_fuel;
        if !st.engine_out && !infinite {
            st.fuel -= fuel_flow(st.throttle, &stats) * dt;
        }
        if !infinite {
            st.fuel -= dmg.fuel_leak * dt;
        }
        if st.fuel <= 0.0 {
            st.fuel = 0.0;
            st.engine_out = true;
        } else if st.engine_out && infinite {
            st.engine_out = false;
        }
        let thrust_scale = if dmg.engine_damaged { ENGINE_DAMAGE_THRUST } else { 1.0 };

        let mut v = vel.0;
        step_air(&mut tf, &mut v, &mut st, &input, &stats, thrust_scale, &tune, dt);

        // Ground contact (§5.1): wheels on terrain/runway, brakes, crashes.
        let p = tf.translation;
        let terrain = hm.height(p.x, p.z);
        let surface = terrain.max(SEA_LEVEL);
        if p.y - GEAR_HEIGHT <= surface {
            let (yaw, pitch, roll) = tf.rotation.to_euler(EulerRot::YXZ);
            let sink = -v.y;
            let speed = v.length();
            let crash = terrain < SEA_LEVEL
                || !st.gear_down
                || sink > CRASH_SINK_RATE
                || roll.abs() > CRASH_BANK
                || pitch < -0.12
                || speed > CRASH_MAX_GROUND_SPEED;
            if crash {
                let god = is_player && cheats.god_mode;
                if !god {
                    damage.send(DamageEvent {
                        target: entity,
                        amount: 10_000.0,
                        kind: DamageKind::Crash,
                        by_player: false,
                    });
                }
                // Keep it above ground for the frame it takes to explode.
                tf.translation.y = surface + GEAR_HEIGHT;
                v = Vec3::new(v.x, v.y.max(0.0), v.z) * 0.3;
                st.on_ground = true;
            } else {
                st.on_ground = true;
                tf.translation.y = surface + GEAR_HEIGHT;
                let fwd_h = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
                let mut along = v.dot(fwd_h);
                let braking = st.throttle < 0.05 || st.airbrake;
                let mu = if braking { BRAKE_FRICTION } else { ROLLING_FRICTION };
                let decel = mu * GRAVITY * dt;
                along = if along.abs() <= decel { 0.0 } else { along - decel * along.signum() };
                v = fwd_h * along;
                // Nosewheel steering at low speed; rudder does the rest.
                let steer = input.yaw * NOSEWHEEL_STEER_RATE * (1.0 - (along.abs() / 70.0).min(1.0));
                let new_yaw = yaw - steer * dt;
                let new_pitch = pitch.clamp(0.0, MAX_GROUND_PITCH);
                tf.rotation = Quat::from_euler(EulerRot::YXZ, new_yaw, new_pitch, 0.0);
                st.rates.z = 0.0;
                st.g_load = 1.0;
            }
        } else {
            st.on_ground = false;
            // Gear rips off if lowered way too fast; just keep it simple:
            if st.gear_down && v.length() > GEAR_MAX_SPEED * 1.6 {
                st.gear_down = false;
            }
        }
        vel.0 = v;
    }
}

fn g_tolerance(time: Res<Time>, cheats: Res<Cheats>, mut q: Query<(&mut FlightState, &mut GTolerance, Has<Player>)>) {
    let dt = time.delta_secs();
    for (mut st, mut tol, is_player) in &mut q {
        let g = st.g_load;
        if g > G_TOL_ONSET {
            tol.blackout += (g - G_TOL_ONSET) * G_TOL_FILL * dt;
        } else {
            tol.blackout -= G_TOL_RECOVER * dt;
        }
        if g < REDOUT_ONSET {
            tol.redout += (REDOUT_ONSET - g) * REDOUT_FILL * dt;
        } else {
            tol.redout -= G_TOL_RECOVER * 1.5 * dt;
        }
        tol.blackout = tol.blackout.clamp(0.0, 1.0);
        tol.redout = tol.redout.clamp(0.0, 1.0);
        if is_player && cheats.god_mode {
            tol.blackout = tol.blackout.min(0.6);
        }
        let worst = tol.blackout.max(tol.redout);
        st.control_auth = 1.0 - smoothstep(BLACKOUT_FADE_START, 1.0, worst) * 0.95;
    }
}

/// Stop on your own runway for RESTOCK_WAIT s → refuel, rearm and repair
/// over RESTOCK_DURATION s.
fn restock(
    time: Res<Time>,
    infra: Res<crate::infrastructure::Infra>,
    mut q: Query<(&Jet, &Transform, &Velocity, &mut FlightState, &mut Restock, &mut Health, &mut JetDamage, &mut Weapons)>,
) {
    let dt = time.delta_secs();
    for (jet, tf, vel, mut st, mut rs, mut hp, mut dmg, mut w) in &mut q {
        let base = jet.side.base();
        let parked = st.on_ground
            && vel.0.length() < RESTOCK_MAX_SPEED
            && on_runway(&base, tf.translation.x, tf.translation.z);
        let stats = jet.stats;
        let needs = st.fuel < stats.max_fuel - 1.0
            || hp.hp < hp.max
            || dmg.fuel_leak > 0.0
            || dmg.engine_damaged
            || dmg.on_fire
            || w.gun_ammo < stats.gun_ammo
            || w.missiles < stats.missiles
            || w.flares < stats.flares;
        if !parked || (!needs && rs.progress == 0.0) {
            rs.stopped = 0.0;
            rs.progress = 0.0;
            rs.full = !needs;
            continue;
        }
        rs.full = false;
        rs.stopped += dt;
        if !rs.active() || rs.progress >= 1.0 {
            continue;
        }
        // A dark fuel depot (§6.4) makes everything slower.
        let slow = if infra.get(jet.side).fuel_up() { 1.0 } else { DEPOT_OUT_RESTOCK_FACTOR };
        let step = dt / (RESTOCK_DURATION * slow);
        rs.progress = (rs.progress + step).min(1.0);
        st.fuel = (st.fuel + stats.max_fuel * step).min(stats.max_fuel);
        hp.hp = (hp.hp + hp.max * step).min(hp.max);
        if rs.progress >= 1.0 {
            st.fuel = stats.max_fuel;
            st.engine_out = false;
            hp.hp = hp.max;
            *dmg = JetDamage::default();
            w.gun_ammo = stats.gun_ammo;
            w.missiles = stats.missiles;
            w.flares = stats.flares;
        }
    }
}

fn afterburner_glow(
    jets: Query<(&FlightState, &Children)>,
    mut glows: Query<(&mut Visibility, &mut Transform), With<AfterburnerGlow>>,
) {
    for (st, children) in &jets {
        for &c in children.iter() {
            if let Ok((mut vis, mut tf)) = glows.get_mut(c) {
                let ab = ((st.throttle - AFTERBURNER_THRESHOLD) / (1.0 - AFTERBURNER_THRESHOLD)).clamp(0.0, 1.0);
                let on = ab > 0.0 && !st.engine_out;
                *vis = if on { Visibility::Inherited } else { Visibility::Hidden };
                tf.scale = Vec3::new(1.0, 0.6 + ab, 1.0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Jet mesh and spawning
// ---------------------------------------------------------------------------

#[derive(Resource)]
pub struct JetAssets {
    fuselage: Handle<Mesh>,
    nose: Handle<Mesh>,
    wing: Handle<Mesh>,
    tailplane: Handle<Mesh>,
    fin: Handle<Mesh>,
    canopy: Handle<Mesh>,
    nozzle: Handle<Mesh>,
    flame: Handle<Mesh>,
    nato_body: Handle<StandardMaterial>,
    enemy_body: Handle<StandardMaterial>,
    glass: Handle<StandardMaterial>,
    dark: Handle<StandardMaterial>,
    flame_mat: Handle<StandardMaterial>,
}

/// A flat wing panel: root chord along Z at x=0, swept tip at x=span.
fn wing_mesh(span: f32, root: f32, tip: f32, sweep: f32, thick: f32) -> Mesh {
    use bevy::render::mesh::PrimitiveTopology;
    use bevy::render::render_asset::RenderAssetUsages;
    let h = thick * 0.5;
    // Top outline (x, z): root front, root back, tip back, tip front.
    let outline = [
        Vec2::new(0.0, -root * 0.5),
        Vec2::new(0.0, root * 0.5),
        Vec2::new(span, -root * 0.5 + sweep + tip),
        Vec2::new(span, -root * 0.5 + sweep),
    ];
    let top: Vec<Vec3> = outline.iter().map(|p| Vec3::new(p.x, h, p.y)).collect();
    let bot: Vec<Vec3> = outline.iter().map(|p| Vec3::new(p.x, -h, p.y)).collect();
    let mut tris: Vec<[Vec3; 3]> = vec![
        [top[0], top[1], top[2]],
        [top[0], top[2], top[3]],
        [bot[0], bot[2], bot[1]],
        [bot[0], bot[3], bot[2]],
    ];
    for i in 0..4 {
        let j = (i + 1) % 4;
        tris.push([top[i], bot[i], bot[j]]);
        tris.push([top[i], bot[j], top[j]]);
    }
    // Winding above is consistently outward-facing.
    let mut pos = Vec::new();
    let mut nor = Vec::new();
    for t in &tris {
        let n = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
        for v in t {
            pos.push(v.to_array());
            nor.push(n.to_array());
        }
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nor)
}

fn setup_jet_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(JetAssets {
        fuselage: meshes.add(Cylinder::new(0.9, 13.0).mesh().resolution(8)),
        nose: meshes.add(Cone::new(0.9, 4.0).mesh().resolution(8)),
        wing: meshes.add(wing_mesh(5.2, 6.5, 1.6, 4.4, 0.25)),
        tailplane: meshes.add(wing_mesh(2.6, 2.6, 1.0, 1.4, 0.15)),
        fin: meshes.add(wing_mesh(2.6, 2.8, 1.0, 1.7, 0.15)),
        canopy: meshes.add(Sphere::new(0.7).mesh().ico(1).unwrap()),
        nozzle: meshes.add(Cylinder::new(0.75, 0.8).mesh().resolution(8)),
        flame: meshes.add(Cone::new(0.6, 4.5).mesh().resolution(8)),
        nato_body: materials.add(StandardMaterial {
            base_color: Color::srgb(0.48, 0.52, 0.56),
            perceptual_roughness: 0.6,
            ..default()
        }),
        enemy_body: materials.add(StandardMaterial {
            base_color: Color::srgb(0.55, 0.50, 0.38),
            perceptual_roughness: 0.6,
            ..default()
        }),
        glass: materials.add(StandardMaterial {
            base_color: Color::srgb(0.15, 0.2, 0.25),
            perceptual_roughness: 0.1,
            metallic: 0.5,
            ..default()
        }),
        dark: materials.add(Color::srgb(0.12, 0.12, 0.13)),
        flame_mat: materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 0.6, 0.2, 0.8),
            emissive: LinearRgba::rgb(30.0, 10.0, 2.0),
            alpha_mode: AlphaMode::Add,
            unlit: true,
            ..default()
        }),
    });
}

/// Spawns a jet (no controller attached). Returns the entity so the caller
/// can add `Player` or `AiPilot`.
pub fn spawn_jet(commands: &mut Commands, a: &JetAssets, side: Side, pos: Vec3, rot: Quat, speed: f32, on_ground: bool) -> Entity {
    let stats = match side {
        Side::Nato => NATO_JET,
        Side::Enemy => ENEMY_JET,
    };
    let body = match side {
        Side::Nato => a.nato_body.clone(),
        Side::Enemy => a.enemy_body.clone(),
    };
    let fwd = rot * Vec3::NEG_Z;
    let mut st = FlightState::new(&stats, on_ground);
    if !on_ground {
        st.throttle = 0.8;
    }
    let input = ControlInput {
        throttle: st.throttle,
        ..default()
    };
    let id = commands
        .spawn((
            (
                Name::new(format!("{side:?} jet")),
                Jet { side, stats },
                Transform::from_translation(pos).with_rotation(rot),
                Visibility::default(),
                Velocity(fwd * speed),
                input,
                st,
                GTolerance::default(),
                Restock::default(),
            ),
            (
                Health::new(stats.hp),
                JetDamage::default(),
                Weapons::new(&stats),
                LockState::default(),
                HitSphere { radius: stats.hit_radius, side: Some(side) },
                Targetable { side },
                StateScoped(AppState::Playing),
            ),
        ))
        .id();
    let along_z = Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
    commands.entity(id).with_children(|p| {
        p.spawn((Mesh3d(a.fuselage.clone()), MeshMaterial3d(body.clone()), Transform::from_rotation(along_z)));
        p.spawn((
            Mesh3d(a.nose.clone()),
            MeshMaterial3d(body.clone()),
            Transform::from_xyz(0.0, 0.0, -8.5).with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
        ));
        for s in [-1.0f32, 1.0] {
            let mirror = if s < 0.0 { Quat::from_rotation_z(std::f32::consts::PI) } else { Quat::IDENTITY };
            p.spawn((
                Mesh3d(a.wing.clone()),
                MeshMaterial3d(body.clone()),
                Transform::from_xyz(s * 0.6, -0.2, 0.2).with_rotation(mirror),
            ));
            p.spawn((
                Mesh3d(a.tailplane.clone()),
                MeshMaterial3d(body.clone()),
                Transform::from_xyz(s * 0.6, 0.0, 5.6).with_rotation(mirror),
            ));
            // Twin fins canted outwards by 20°.
            let cant = if s > 0.0 { 1.22 } else { std::f32::consts::PI - 1.22 };
            p.spawn((
                Mesh3d(a.fin.clone()),
                MeshMaterial3d(body.clone()),
                Transform::from_xyz(s * 1.0, 0.6, 4.6).with_rotation(Quat::from_rotation_z(cant)),
            ));
        }
        p.spawn((Mesh3d(a.canopy.clone()), MeshMaterial3d(a.glass.clone()), Transform::from_xyz(0.0, 0.85, -4.2).with_scale(Vec3::new(0.8, 0.7, 2.4))));
        p.spawn((Mesh3d(a.nozzle.clone()), MeshMaterial3d(a.dark.clone()), Transform::from_xyz(0.0, 0.0, 6.8).with_rotation(along_z)));
        p.spawn((
            AfterburnerGlow,
            Mesh3d(a.flame.clone()),
            MeshMaterial3d(a.flame_mat.clone()),
            // Cone points +Y; turn it to trail behind the nozzle (+Z).
            Transform::from_xyz(0.0, 0.0, 9.4).with_rotation(along_z),
            Visibility::Hidden,
        ));
    });
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level_trim(stats: &JetStats, speed: f32, alt: f32) -> (f32, f32) {
        // Find AoA where lift = weight, then throttle where thrust = drag.
        let weight = (stats.empty_mass + stats.max_fuel) * GRAVITY;
        let q = 0.5 * air_density(alt) * speed * speed * stats.wing_area;
        let (mut lo, mut hi) = (0.0, stats.stall_aoa);
        for _ in 0..50 {
            let mid = (lo + hi) * 0.5;
            if lift_coefficient(mid, stats) * q < weight {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let aoa = lo;
        let cl = lift_coefficient(aoa, stats);
        let drag = q * drag_coefficient(cl, aoa, speed / SPEED_OF_SOUND, stats, false, false);
        let (mut tlo, mut thi) = (0.0, 1.0);
        for _ in 0..50 {
            let mid = (tlo + thi) * 0.5;
            if thrust_newtons(mid, alt, stats) * aoa.cos() < drag {
                tlo = mid;
            } else {
                thi = mid;
            }
        }
        (aoa, tlo)
    }

    #[test]
    fn density_falls_with_altitude() {
        assert!((air_density(0.0) - RHO0).abs() < 1e-6);
        assert!(air_density(5_000.0) < air_density(1_000.0));
        assert!((air_density(ATMOS_SCALE_HEIGHT) - RHO0 / std::f32::consts::E).abs() < 1e-3);
    }

    #[test]
    fn stall_drops_lift() {
        let s = NATO_JET;
        let at_stall = lift_coefficient(s.stall_aoa, &s);
        let past = lift_coefficient(s.stall_aoa + 0.2, &s);
        assert!(past < at_stall * 0.8, "{past} vs {at_stall}");
        assert!(lift_coefficient(-0.1, &s) < 0.0);
    }

    #[test]
    fn drag_grows_with_lift_and_gear() {
        let s = NATO_JET;
        let clean = drag_coefficient(0.2, 0.05, 0.5, &s, false, false);
        assert!(drag_coefficient(0.8, 0.2, 0.5, &s, false, false) > clean);
        assert!(drag_coefficient(0.2, 0.05, 0.5, &s, true, true) > clean);
        assert!(drag_coefficient(0.2, 0.05, 1.2, &s, false, false) > clean);
    }

    #[test]
    fn level_flight_holds() {
        let stats = NATO_JET;
        let (speed, alt) = (220.0, 2_000.0);
        let (aoa, throttle) = level_trim(&stats, speed, alt);
        let mut tf = Transform::from_xyz(0.0, alt, 0.0).with_rotation(Quat::from_rotation_x(aoa));
        let mut vel = Vec3::new(0.0, 0.0, -speed);
        let mut st = FlightState::new(&stats, false);
        st.throttle = throttle;
        let input = ControlInput { throttle, ..default() };
        let tune = AeroTuning::default();
        for _ in 0..(60 * 10) {
            step_air(&mut tf, &mut vel, &mut st, &input, &stats, 1.0, &tune, 1.0 / 60.0);
        }
        assert!((tf.translation.y - alt).abs() < 60.0, "altitude drifted to {}", tf.translation.y);
        assert!((vel.length() - speed).abs() < 15.0, "speed drifted to {}", vel.length());
        assert!((st.g_load - 1.0).abs() < 0.15, "g = {}", st.g_load);
    }

    #[test]
    fn full_pull_respects_g_limit() {
        let stats = NATO_JET;
        let mut tf = Transform::from_xyz(0.0, 3_000.0, 0.0);
        let mut vel = Vec3::new(0.0, 0.0, -260.0);
        let mut st = FlightState::new(&stats, false);
        st.throttle = 1.0;
        let input = ControlInput { pitch: 1.0, throttle: 1.0, ..default() };
        let tune = AeroTuning::default();
        let mut max_g: f32 = 0.0;
        for _ in 0..(60 * 3) {
            step_air(&mut tf, &mut vel, &mut st, &input, &stats, 1.0, &tune, 1.0 / 60.0);
            max_g = max_g.max(st.g_load);
        }
        assert!(max_g > 7.0, "should reach high G, got {max_g}");
        assert!(max_g < stats.max_g + 1.5, "limiter failed, got {max_g}");
    }

    #[test]
    fn slow_jet_stalls_and_drops() {
        let stats = NATO_JET;
        let mut tf = Transform::from_xyz(0.0, 3_000.0, 0.0).with_rotation(Quat::from_rotation_x(0.6));
        let mut vel = Vec3::new(0.0, 0.0, -55.0);
        let mut st = FlightState::new(&stats, false);
        st.throttle = 0.0;
        let input = ControlInput { pitch: 1.0, ..default() };
        let tune = AeroTuning::default();
        let mut stalled = false;
        for _ in 0..(60 * 4) {
            step_air(&mut tf, &mut vel, &mut st, &input, &stats, 1.0, &tune, 1.0 / 60.0);
            stalled |= st.stalled;
        }
        assert!(stalled);
        assert!(vel.y < -10.0, "should be falling, vy = {}", vel.y);
    }
}
