//! AI pilots (§5.5): Patrol → Engage → Evade → Return to base, plus
//! terrain avoidance. They fly the same model through the same
//! `ControlInput` as the player. Also the enemy jet spawner and the
//! friendly flight.

use bevy::prelude::*;
use rand::Rng;

use crate::config::*;
use crate::flight::{spawn_jet, ControlInput, FlightSet, FlightState, GTolerance, Jet, JetAssets, Velocity};
use crate::game_rules::EventLog;
use crate::infrastructure::Infra;
use crate::state::{AppState, GameplaySet};
use crate::audio::{Sfx, SoundEvent};
use crate::weapons::{spawn_missile, Health, LockState, Missile, Targetable, WeaponAssets, Weapons};
use crate::world::{base_center, Heightmap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiState {
    Patrol,
    Engage,
    Evade,
    ReturnToBase,
    /// Bombing run on an infrastructure site (§6.4).
    Strike,
}

#[derive(Component, Debug)]
pub struct AiPilot {
    pub state: AiState,
    pub waypoint: Vec3,
    pub target: Option<Entity>,
    pub think_timer: f32,
    pub missile_timer: f32,
    pub flare_timer: f32,
    /// Member of the friendly flight (obeys map patrol orders).
    pub friendly_flight: bool,
    /// Site to bomb (entity, aim point), set by the strike planner.
    pub strike: Option<(Entity, Vec3)>,
    pub strike_shots: u32,
}

impl AiPilot {
    pub fn new(friendly_flight: bool) -> Self {
        Self {
            state: AiState::Patrol,
            waypoint: Vec3::ZERO,
            target: None,
            think_timer: 0.0,
            missile_timer: AI_MISSILE_INTERVAL * 0.5,
            flare_timer: 0.0,
            friendly_flight,
            strike: None,
            strike_shots: 0,
        }
    }
}

/// Enemy air pool (§5.5): keeps 2–4 jets airborne from the enemy base.
#[derive(Resource)]
pub struct EnemyAir {
    pub pool: u32,
    pub cooldown: f32,
    pub desired: u32,
    pub desired_timer: f32,
}

impl Default for EnemyAir {
    fn default() -> Self {
        Self {
            pool: ENEMY_JET_POOL,
            cooldown: ENEMY_FIRST_SPAWN_DELAY,
            desired: ENEMY_AIRBORNE_MIN,
            desired_timer: 120.0,
        }
    }
}

/// The friendly AI flight: its patrol point is set from the map.
#[derive(Resource)]
pub struct FriendlyFlight {
    pub pool: u32,
    pub respawn_timer: f32,
    pub patrol_point: Vec3,
}

impl Default for FriendlyFlight {
    fn default() -> Self {
        Self {
            pool: FRIENDLY_POOL,
            respawn_timer: 3.0,
            patrol_point: Vec3::new(NATO_BASE.center[0] + 6_000.0, AI_CRUISE_ALT, NATO_BASE.center[1]),
        }
    }
}

pub struct PilotAiPlugin;

impl Plugin for PilotAiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EnemyAir>()
            .init_resource::<FriendlyFlight>()
            .add_systems(OnEnter(AppState::Playing), reset_air)
            .add_systems(FixedUpdate, ai_pilots.in_set(FlightSet::Control))
            .add_systems(Update, (enemy_spawner, friendly_spawner, ai_return_home).in_set(GameplaySet));
    }
}

fn reset_air(mut commands: Commands) {
    commands.insert_resource(EnemyAir::default());
    commands.insert_resource(FriendlyFlight::default());
}

// ---------------------------------------------------------------------------
// Steering (pure)
// ---------------------------------------------------------------------------

/// Stick inputs (pitch, roll, yaw) that point the nose at `desired`
/// (world direction): roll the target into the lift plane, then pull.
pub fn steer(rot: Quat, desired: Vec3, max_pull: f32) -> (f32, f32, f32) {
    let d = (rot.inverse() * desired.normalize_or_zero()).normalize_or_zero();
    let fwd = -d.z;
    let angle = fwd.clamp(-1.0, 1.0).acos();
    if angle < 0.15 {
        // Fine tracking with wings drifting back to level.
        let (_, _, bank) = rot.to_euler(EulerRot::YXZ);
        let pitch = (d.y * 12.0).clamp(-1.0, 1.0);
        let yaw = (d.x * 10.0).clamp(-1.0, 1.0);
        let roll = (bank * 1.5 + d.x * 6.0).clamp(-1.0, 1.0);
        return (pitch.min(max_pull), roll, yaw);
    }
    let roll_err = if fwd < -0.95 && d.x.abs() < 0.05 && d.y.abs() < 0.05 {
        0.0 // dead astern: just pull
    } else {
        d.x.atan2(d.y)
    };
    let roll = (roll_err * 3.0).clamp(-1.0, 1.0);
    let pitch = if roll_err.abs() < 0.5 {
        (angle * 3.0).clamp(0.0, max_pull)
    } else if roll_err.abs() < 1.2 {
        0.3_f32.min(max_pull)
    } else {
        0.05
    };
    let yaw = (d.x * 2.0).clamp(-1.0, 1.0) * 0.3;
    (pitch, roll, yaw)
}

/// Lowest predicted height above ground over the next few seconds.
pub fn predicted_agl(hm: &Heightmap, pos: Vec3, vel: Vec3) -> f32 {
    let mut min = pos.y - hm.surface(pos.x, pos.z);
    for t in [1.0, 2.0, 3.0, 4.5, 6.0] {
        let p = pos + vel * t;
        min = min.min(p.y - hm.surface(p.x, p.z));
    }
    min
}

/// Desired direction to fly towards `point` at altitude `alt`.
fn cruise_dir(pos: Vec3, point: Vec3, alt: f32, hm: &Heightmap) -> Vec3 {
    let mut h = Vec3::new(point.x - pos.x, 0.0, point.z - pos.z);
    if h.length_squared() < 1.0 {
        h = Vec3::NEG_Z;
    }
    let safe_alt = alt.max(hm.surface(pos.x, pos.z) + AI_MIN_AGL * 2.0);
    let climb = ((safe_alt - pos.y) / 1_500.0).clamp(-0.35, 0.45);
    h.normalize() + Vec3::Y * climb
}

fn random_patrol_point(rng: &mut impl Rng, center: Vec3, radius: f32) -> Vec3 {
    let a = rng.gen_range(0.0..std::f32::consts::TAU);
    let r = rng.gen_range(0.3..1.0) * radius;
    let p = center + Vec3::new(a.cos() * r, 0.0, a.sin() * r);
    Vec3::new(
        p.x.clamp(-WORLD_HALF + 4_000.0, WORLD_HALF - 4_000.0),
        AI_CRUISE_ALT + rng.gen_range(-300.0..500.0),
        p.z.clamp(-WORLD_HALF + 4_000.0, WORLD_HALF - 8_000.0),
    )
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

#[allow(clippy::type_complexity)]
fn ai_pilots(
    mut commands: Commands,
    weapon_assets: Res<WeaponAssets>,
    mut sounds: EventWriter<SoundEvent>,
    time: Res<Time>,
    hm: Res<Heightmap>,
    friendly: Res<FriendlyFlight>,
    infra: Res<Infra>,
    mut pilots: Query<(
        Entity,
        &Jet,
        &Transform,
        &Velocity,
        &FlightState,
        &GTolerance,
        &Health,
        &mut Weapons,
        &LockState,
        &mut ControlInput,
        &mut AiPilot,
    )>,
    targets: Query<(Entity, &Targetable, &Transform, &Velocity)>,
    missiles: Query<(&Missile, &Transform)>,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();
    for (me, jet, tf, vel, st, tol, hp, mut w, lock, mut input, mut ai) in &mut pilots {
        let pos = tf.translation;
        let fwd = tf.forward().as_vec3();
        ai.think_timer -= dt;
        ai.missile_timer -= dt;
        ai.flare_timer -= dt;

        // --- Decide state ---
        let threat = missiles
            .iter()
            .filter(|(m, mtf)| m.target == Some(me) && mtf.translation.distance(pos) < AI_EVADE_RANGE)
            .map(|(_, mtf)| mtf.translation)
            .min_by(|a, b| a.distance(pos).total_cmp(&b.distance(pos)));
        let low_fuel = st.fuel < jet.stats.max_fuel * AI_RTB_FUEL_FRAC;
        let hurt = hp.hp < hp.max * AI_RTB_HP_FRAC;
        let winchester = w.missiles == 0 && w.gun_ammo < 40;

        if ai.think_timer <= 0.0 {
            ai.think_timer = AI_THINK_INTERVAL;
            // Nearest visible enemy air target; half-blind without radar.
            let detect = if infra.get(jet.side).radar_up() { AI_DETECT_RANGE } else { AI_DETECT_RANGE * RADAR_OUT_DETECT_FACTOR };
            ai.target = targets
                .iter()
                .filter(|(e, t, ..)| *e != me && t.side != jet.side)
                .map(|(e, _, ttf, _)| (e, ttf.translation.distance(pos)))
                .filter(|(_, d)| *d < detect)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(e, _)| e);
        }
        let target = ai.target.and_then(|t| targets.get(t).ok()).map(|(_, _, ttf, tv)| (ttf.translation, tv.0));

        ai.state = if threat.is_some() {
            AiState::Evade
        } else if low_fuel || hurt || winchester || ai.state == AiState::ReturnToBase {
            AiState::ReturnToBase
        } else if ai.strike.is_some() {
            AiState::Strike
        } else if target.is_some() {
            AiState::Engage
        } else {
            AiState::Patrol
        };

        // --- Fly ---
        let mut fire_gun = false;
        let (mut desired, mut throttle, mut max_pull) = match ai.state {
            AiState::Patrol => {
                let center = if ai.friendly_flight {
                    friendly.patrol_point
                } else if jet.side == Side::Enemy {
                    // Enemy CAP drifts between its base and the front.
                    let [bx, bz] = ENEMY_BASE.center;
                    Vec3::new((FRONT_CENTER[0] + bx) * 0.5, AI_CRUISE_ALT, (FRONT_CENTER[1] + bz) * 0.5)
                } else {
                    base_center(&NATO_BASE)
                };
                let flat = Vec2::new(ai.waypoint.x - pos.x, ai.waypoint.z - pos.z).length();
                if ai.waypoint == Vec3::ZERO || flat < 1_500.0 || ai.waypoint.xz().distance(center.xz()) > 9_000.0 {
                    ai.waypoint = random_patrol_point(&mut rng, center, 7_000.0);
                }
                (cruise_dir(pos, ai.waypoint, ai.waypoint.y, &hm), 0.72, 0.55)
            }
            AiState::Engage => {
                let (tp, tv) = target.unwrap();
                let dist = tp.distance(pos);
                // Lead pursuit for the gun.
                let tof = dist / (GUN_MUZZLE_SPEED + vel.0.length() * 0.5);
                let aim = tp + tv * tof + Vec3::Y * 0.5 * GRAVITY * tof * tof;
                let to_aim = (aim - pos).normalize_or_zero();
                if dist < AI_GUN_RANGE && fwd.angle_between(to_aim) < AI_GUN_CONE && w.gun_ammo > 0 {
                    fire_gun = true;
                }
                if lock.locked && lock.candidate == ai.target && ai.missile_timer <= 0.0 && w.missiles > 0 && dist < MISSILE_LOCK_RANGE * 0.85 {
                    input.fire_missile = true;
                    ai.missile_timer = AI_MISSILE_INTERVAL * rng.gen_range(0.8..1.3);
                }
                let throttle = if dist > 3_000.0 { 0.97 } else { 0.85 };
                let jitter = Vec3::new(rng.gen_range(-1.0..1.0), rng.gen_range(-1.0..1.0), rng.gen_range(-1.0..1.0)) * AI_GUN_ERROR;
                (to_aim + jitter, throttle, 0.9)
            }
            AiState::Evade => {
                // Beam the missile: turn perpendicular to it, dive a little.
                let m = threat.unwrap();
                let los = (m - pos).normalize_or_zero();
                let mut beam = los.cross(Vec3::Y).normalize_or_zero();
                if beam.dot(fwd) < 0.0 {
                    beam = -beam;
                }
                if ai.flare_timer <= 0.0 && w.flares > 0 {
                    input.flare = true;
                    ai.flare_timer = AI_FLARE_INTERVAL;
                }
                (beam - Vec3::Y * 0.15, 1.0, 1.0)
            }
            AiState::ReturnToBase => {
                let home = base_center(&jet.side.base());
                let flat = Vec2::new(home.x - pos.x, home.z - pos.z).length();
                let alt = if flat < 4_000.0 { home.y + AI_DESPAWN_ALT * 0.6 } else { AI_CRUISE_ALT * 0.7 };
                (cruise_dir(pos, home, alt, &hm), 0.75, 0.5)
            }
            AiState::Strike => {
                // Run in low, dive at the site, ripple guided air-to-ground
                // missiles at it, then break off and go back to patrolling.
                let (site_e, site) = ai.strike.unwrap();
                let flat = Vec2::new(site.x - pos.x, site.z - pos.z).length();
                if flat > STRIKE_DIVE_DIST {
                    (cruise_dir(pos, site, site.y + STRIKE_RUN_IN_AGL, &hm), 0.9, 0.6)
                } else {
                    let aim = (site - pos).normalize_or_zero();
                    let angle = fwd.angle_between(aim);
                    let dist = site.distance(pos);
                    if flat < STRIKE_FIRE_DIST && angle < STRIKE_FIRE_CONE && ai.missile_timer <= 0.0 && w.missiles > 0 {
                        w.missiles -= 1;
                        ai.missile_timer = 1.2;
                        ai.strike_shots += 1;
                        let from = pos - tf.up() * 2.0 + fwd * 2.0;
                        spawn_missile(&mut commands, &weapon_assets, from, vel.0 + fwd * 25.0, jet.side, false, Some(site_e));
                        sounds.send(SoundEvent::at(Sfx::MissileLaunch, pos));
                    }
                    if dist < AI_GUN_RANGE * 1.6 && angle < AI_GUN_CONE && w.gun_ammo > 0 {
                        fire_gun = true;
                    }
                    if ai.strike_shots >= STRIKE_SHOTS || flat < STRIKE_BREAK_DIST {
                        ai.strike = None;
                        ai.strike_shots = 0;
                        ai.missile_timer = ai.missile_timer.max(AI_MISSILE_INTERVAL * 0.5);
                    }
                    (aim, 0.8, 0.8)
                }
            }
        };

        // Terrain avoidance overrides everything.
        let agl = predicted_agl(&hm, pos, vel.0);
        let min_agl = if matches!(ai.state, AiState::ReturnToBase | AiState::Strike) { AI_MIN_AGL * 0.5 } else { AI_MIN_AGL };
        if agl < min_agl {
            let h = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or_zero();
            desired = h * 0.5 + Vec3::Y;
            throttle = 1.0;
            max_pull = 1.0;
        }
        // Stay inside the map.
        if pos.x.abs() > WORLD_HALF - 2_000.0 || pos.z.abs() > WORLD_HALF - 2_000.0 {
            desired = cruise_dir(pos, Vec3::ZERO, AI_CRUISE_ALT, &hm);
        }
        // Don't black yourself out.
        if tol.blackout > 0.35 {
            max_pull *= 0.55;
        }

        let (p, r, y) = steer(tf.rotation, desired, max_pull);
        input.pitch = p;
        input.roll = r;
        input.yaw = y;
        input.throttle = throttle;
        input.fire_gun = fire_gun;
    }
}

fn enemy_spawner(
    mut commands: Commands,
    time: Res<Time>,
    assets: Res<JetAssets>,
    mut air: ResMut<EnemyAir>,
    infra: Res<Infra>,
    pilots: Query<&Jet, With<AiPilot>>,
    mut log: ResMut<EventLog>,
) {
    let dt = time.delta_secs();
    air.cooldown -= dt;
    air.desired_timer -= dt;
    if air.desired_timer <= 0.0 {
        air.desired_timer = 120.0;
        air.desired = rand::thread_rng().gen_range(ENEMY_AIRBORNE_MIN..=ENEMY_AIRBORNE_MAX);
    }
    let airborne = pilots.iter().filter(|j| j.side == Side::Enemy).count() as u32;
    if airborne < air.desired && air.cooldown <= 0.0 && air.pool > 0 {
        air.pool -= 1;
        air.cooldown = ENEMY_SPAWN_COOLDOWN * if infra.enemy.fuel_up() { 1.0 } else { DEPOT_OUT_SPAWN_FACTOR };
        let base = base_center(&ENEMY_BASE);
        let pos = base + Vec3::new(0.0, AI_SPAWN_ALT, 0.0);
        let rot = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2); // facing west
        let e = spawn_jet(&mut commands, &assets, Side::Enemy, pos, rot, AI_SPAWN_SPEED, false);
        commands.entity(e).insert(AiPilot::new(false));
        log.push("Enemy fighter launched");
    }
}

fn friendly_spawner(
    mut commands: Commands,
    time: Res<Time>,
    assets: Res<JetAssets>,
    mut flight: ResMut<FriendlyFlight>,
    pilots: Query<&AiPilot>,
) {
    let airborne = pilots.iter().filter(|p| p.friendly_flight).count() as u32;
    if airborne >= FRIENDLY_FLIGHT_SIZE {
        return;
    }
    flight.respawn_timer -= time.delta_secs();
    if flight.respawn_timer > 0.0 || flight.pool == 0 {
        return;
    }
    flight.pool -= 1;
    flight.respawn_timer = if airborne + 1 < FRIENDLY_FLIGHT_SIZE { 4.0 } else { FRIENDLY_RESPAWN_TIME };
    let base = base_center(&NATO_BASE);
    let pos = base + Vec3::new(400.0 * airborne as f32, AI_SPAWN_ALT, -1_500.0);
    let rot = Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2); // facing east
    let e = spawn_jet(&mut commands, &assets, Side::Nato, pos, rot, AI_SPAWN_SPEED, false);
    commands.entity(e).insert(AiPilot::new(true));
}

/// RTB simplification: once over its base below 300 m AGL the jet "lands"
/// (despawns) and returns to the pool.
fn ai_return_home(
    mut commands: Commands,
    hm: Res<Heightmap>,
    q: Query<(Entity, &Jet, &Transform, &AiPilot)>,
    mut enemy: ResMut<EnemyAir>,
    mut friendly: ResMut<FriendlyFlight>,
) {
    for (e, jet, tf, ai) in &q {
        if ai.state != AiState::ReturnToBase {
            continue;
        }
        let home = base_center(&jet.side.base());
        let p = tf.translation;
        let flat = Vec2::new(home.x - p.x, home.z - p.z).length();
        if flat < 1_800.0 && p.y - hm.surface(p.x, p.z) < AI_DESPAWN_ALT {
            commands.entity(e).despawn_recursive();
            match jet.side {
                Side::Enemy => enemy.pool += 1,
                Side::Nato => friendly.pool += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steer_pulls_up_for_target_above() {
        let (p, r, _) = steer(Quat::IDENTITY, Vec3::new(0.0, 1.0, -1.0), 1.0);
        assert!(p > 0.5 && r.abs() < 0.1);
    }

    #[test]
    fn steer_rolls_towards_target_on_right() {
        let (_, r, _) = steer(Quat::IDENTITY, Vec3::new(1.0, 0.0, -0.2), 1.0);
        assert!(r > 0.5);
        let (_, r, _) = steer(Quat::IDENTITY, Vec3::new(-1.0, 0.0, -0.2), 1.0);
        assert!(r < -0.5);
    }
}
