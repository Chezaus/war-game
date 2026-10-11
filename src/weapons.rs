//! Cannon, heat-seeking missiles, flares, detonation and damage (§5.2, §5.3).

use std::collections::HashMap;

use bevy::prelude::*;
use rand::Rng;

use crate::audio::{Sfx, SoundEvent};
use crate::config::*;
use crate::debug::Cheats;
use crate::flight::{FlightSet, Jet, Velocity, ControlInput};
use crate::fx::Particles;
use crate::player::Player;
use crate::state::{AppState, GameplaySet};
use crate::world::Heightmap;

// ---------------------------------------------------------------------------
// Components, events
// ---------------------------------------------------------------------------

#[derive(Component, Debug)]
pub struct Health {
    pub hp: f32,
    pub max: f32,
    pub last_hit_by_player: bool,
}

impl Health {
    pub fn new(hp: f32) -> Self {
        Self {
            hp,
            max: hp,
            last_hit_by_player: false,
        }
    }
}

/// Jet-only damage states (§5.2).
#[derive(Component, Default, Debug, Clone)]
pub struct JetDamage {
    pub engine_damaged: bool,
    /// kg/s of fuel lost.
    pub fuel_leak: f32,
    pub on_fire: bool,
}

/// Something bullets and blasts can hit. `side: None` = neutral.
#[derive(Component, Debug)]
pub struct HitSphere {
    pub radius: f32,
    pub side: Option<Side>,
}

/// Damage to this entity is forwarded to another (unit model → unit,
/// bridge segment → bridge).
#[derive(Component)]
pub struct DamageRelay(pub Entity);

/// Lockable air target.
#[derive(Component)]
pub struct Targetable {
    pub side: Side,
}

/// Stationary debug target (F6).
#[derive(Component)]
pub struct TargetDrone;

#[derive(Component, Debug)]
pub struct Weapons {
    pub gun_ammo: u32,
    pub gun_cooldown: f32,
    pub missiles: u32,
    pub missile_cooldown: f32,
    pub flares: u32,
    pub flare_cooldown: f32,
    pub gun_sound_cooldown: f32,
}

impl Weapons {
    pub fn new(stats: &JetStats) -> Self {
        Self {
            gun_ammo: stats.gun_ammo,
            gun_cooldown: 0.0,
            missiles: stats.missiles,
            missile_cooldown: 0.0,
            flares: stats.flares,
            flare_cooldown: 0.0,
            gun_sound_cooldown: 0.0,
        }
    }
}

/// Missile seeker on the launching jet: the candidate in the cone and how
/// long it has been held.
#[derive(Component, Default, Debug)]
pub struct LockState {
    pub candidate: Option<Entity>,
    pub time: f32,
    pub locked: bool,
}

#[derive(Component)]
pub struct Bullet {
    pub side: Side,
    pub by_player: bool,
    pub life: f32,
    pub damage: f32,
}

#[derive(Component)]
pub struct Missile {
    pub side: Side,
    pub by_player: bool,
    pub target: Option<Entity>,
    pub age: f32,
    pub trail_timer: f32,
}

#[derive(Component)]
pub struct Flare {
    pub life: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageKind {
    Bullet,
    Blast,
    Crash,
}

#[derive(Event, Debug, Clone)]
pub struct DamageEvent {
    pub target: Entity,
    pub amount: f32,
    pub kind: DamageKind,
    pub by_player: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boom {
    /// Bullet hit spark.
    Spark,
    /// Missile, vehicle.
    Medium,
    /// Jet crash, bridge.
    Large,
    /// Ballistic warhead (§6.7).
    Huge,
}

/// The one shared "explode here" message. Deals radius damage and
/// triggers FX; every explosion goes through it.
#[derive(Event, Debug, Clone)]
pub struct Detonation {
    pub pos: Vec3,
    pub size: Boom,
    pub damage: f32,
    pub radius: f32,
    pub by_player: bool,
}

impl Detonation {
    /// Visual only.
    pub fn fx(pos: Vec3, size: Boom) -> Self {
        Self {
            pos,
            size,
            damage: 0.0,
            radius: 0.0,
            by_player: false,
        }
    }
}

#[derive(Event, Debug, Clone)]
pub struct JetDestroyed {
    pub side: Side,
    pub pos: Vec3,
    pub by_player: bool,
    pub was_player: bool,
}

// ---------------------------------------------------------------------------
// Pure math (unit-tested)
// ---------------------------------------------------------------------------

/// Does the segment a→b pass within `r` of `c`? Returns the fraction
/// along the segment of the closest point if so.
pub fn segment_hits_sphere(a: Vec3, b: Vec3, c: Vec3, r: f32) -> Option<f32> {
    let ab = b - a;
    let len2 = ab.length_squared();
    let t = if len2 < 1e-9 { 0.0 } else { ((c - a).dot(ab) / len2).clamp(0.0, 1.0) };
    let closest = a + ab * t;
    (closest.distance_squared(c) <= r * r).then_some(t)
}

/// Proportional navigation: commanded lateral acceleration.
pub fn pn_accel(missile_pos: Vec3, missile_vel: Vec3, target_pos: Vec3, target_vel: Vec3, gain: f32) -> Vec3 {
    let r = target_pos - missile_pos;
    let r2 = r.length_squared().max(1.0);
    let v_rel = target_vel - missile_vel;
    let omega = r.cross(v_rel) / r2;
    gain * omega.cross(missile_vel)
}

/// Integrates one missile step. Returns the new (pos, vel).
pub fn missile_step(pos: Vec3, vel: Vec3, age: f32, target: Option<(Vec3, Vec3)>, alt: f32, dt: f32) -> (Vec3, Vec3) {
    let speed = vel.length().max(1.0);
    let dir = vel / speed;
    let mut accel = Vec3::new(0.0, -GRAVITY, 0.0);
    // Lift to hold against gravity (perpendicular part).
    let up_perp = (Vec3::Y - dir * dir.y).normalize_or_zero();
    let mut cmd = up_perp * GRAVITY * (1.0 - dir.y * dir.y).sqrt();
    if let Some((tp, tv)) = target {
        cmd += pn_accel(pos, vel, tp, tv, MISSILE_NAV_GAIN);
    }
    // Lateral only, capped by max G.
    cmd -= dir * cmd.dot(dir);
    let max = MISSILE_MAX_G * GRAVITY;
    if cmd.length() > max {
        cmd = cmd.normalize() * max;
    }
    accel += cmd;
    if age < MISSILE_BURN_TIME {
        accel += dir * MISSILE_BOOST_ACCEL;
    }
    let density = (-alt.max(0.0) / ATMOS_SCALE_HEIGHT).exp();
    accel -= dir * (MISSILE_DRAG_K * density * speed * speed + cmd.length() * 0.08);
    let new_vel = vel + accel * dt;
    (pos + new_vel * dt, new_vel)
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

#[derive(Resource)]
pub struct WeaponAssets {
    tracer: Handle<Mesh>,
    tracer_nato: Handle<StandardMaterial>,
    tracer_enemy: Handle<StandardMaterial>,
    missile: Handle<Mesh>,
    missile_mat: Handle<StandardMaterial>,
    flare: Handle<Mesh>,
    flare_mat: Handle<StandardMaterial>,
    drone: Handle<Mesh>,
    drone_mat: Handle<StandardMaterial>,
}

pub struct WeaponsPlugin;

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<DamageEvent>()
            .add_event::<Detonation>()
            .add_event::<JetDestroyed>()
            .add_systems(Startup, setup_weapon_assets)
            .add_systems(
                FixedUpdate,
                (
                    update_locks,
                    fire_weapons,
                    move_bullets,
                    move_missiles,
                    move_flares,
                    blast_damage,
                    apply_damage,
                    burning,
                    kill_dead,
                )
                    .chain()
                    .after(FlightSet::Integrate)
                    .in_set(GameplaySet),
            );
    }
}

fn setup_weapon_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let glow = |c: LinearRgba| StandardMaterial {
        base_color: Color::from(c),
        emissive: c * 12.0,
        unlit: true,
        ..default()
    };
    commands.insert_resource(WeaponAssets {
        tracer: meshes.add(Cuboid::new(0.25, 0.25, 14.0)),
        tracer_nato: materials.add(glow(LinearRgba::rgb(1.0, 0.85, 0.3))),
        tracer_enemy: materials.add(glow(LinearRgba::rgb(1.0, 0.35, 0.2))),
        missile: meshes.add(Cylinder::new(0.14, 3.0).mesh().resolution(6)),
        missile_mat: materials.add(Color::srgb(0.85, 0.85, 0.82)),
        flare: meshes.add(Sphere::new(0.9).mesh().ico(1).unwrap()),
        flare_mat: materials.add(glow(LinearRgba::rgb(1.0, 0.95, 0.8))),
        drone: meshes.add(Cuboid::new(6.0, 3.0, 10.0)),
        drone_mat: materials.add(StandardMaterial {
            base_color: Color::srgb(0.9, 0.3, 0.1),
            emissive: LinearRgba::rgb(0.6, 0.1, 0.0),
            ..default()
        }),
    });
}

pub fn spawn_missile(
    commands: &mut Commands,
    a: &WeaponAssets,
    pos: Vec3,
    vel: Vec3,
    side: Side,
    by_player: bool,
    target: Option<Entity>,
) {
    let dir = vel.normalize_or_zero();
    commands.spawn((
        Name::new("missile"),
        Missile { side, by_player, target, age: 0.0, trail_timer: 0.0 },
        Velocity(vel),
        Mesh3d(a.missile.clone()),
        MeshMaterial3d(a.missile_mat.clone()),
        Transform::from_translation(pos).with_rotation(Quat::from_rotation_arc(Vec3::Y, dir)),
        StateScoped(AppState::Playing),
    ));
}

pub fn spawn_bullet(commands: &mut Commands, a: &WeaponAssets, pos: Vec3, vel: Vec3, side: Side, by_player: bool, damage: f32) {
    commands.spawn((
        Bullet { side, by_player, life: BULLET_LIFE, damage },
        Velocity(vel),
        Mesh3d(a.tracer.clone()),
        MeshMaterial3d(match side {
            Side::Nato => a.tracer_nato.clone(),
            Side::Enemy => a.tracer_enemy.clone(),
        }),
        Transform::from_translation(pos).looking_to(vel, Vec3::Y),
        StateScoped(AppState::Playing),
    ));
}

pub fn spawn_drone(commands: &mut Commands, a: &WeaponAssets, pos: Vec3) {
    commands.spawn((
        Name::new("target drone"),
        TargetDrone,
        Targetable { side: Side::Enemy },
        HitSphere { radius: 8.0, side: Some(Side::Enemy) },
        Health::new(60.0),
        Velocity(Vec3::ZERO),
        Mesh3d(a.drone.clone()),
        MeshMaterial3d(a.drone_mat.clone()),
        Transform::from_translation(pos),
        StateScoped(AppState::Playing),
    ));
}

fn update_locks(
    time: Res<Time>,
    mut shooters: Query<(Entity, &Jet, &Transform, &Weapons, &mut LockState)>,
    targets: Query<(Entity, &Targetable, &Transform)>,
) {
    let dt = time.delta_secs();
    for (me, jet, tf, w, mut lock) in &mut shooters {
        let fwd = tf.forward();
        let mut best: Option<(Entity, f32)> = None;
        if w.missiles > 0 {
            for (e, t, ttf) in &targets {
                if e == me || t.side == jet.side {
                    continue;
                }
                let to = ttf.translation - tf.translation;
                let dist = to.length();
                if dist > MISSILE_LOCK_RANGE || dist < 150.0 {
                    continue;
                }
                let angle = fwd.angle_between(to);
                if angle < MISSILE_LOCK_CONE && best.is_none_or(|(_, a)| angle < a) {
                    best = Some((e, angle));
                }
            }
        }
        let cand = best.map(|(e, _)| e);
        if cand == lock.candidate && cand.is_some() {
            lock.time += dt;
        } else {
            lock.candidate = cand;
            lock.time = 0.0;
        }
        lock.locked = lock.candidate.is_some() && lock.time >= MISSILE_LOCK_TIME;
    }
}

#[allow(clippy::too_many_arguments)]
fn fire_weapons(
    mut commands: Commands,
    time: Res<Time>,
    assets: Res<WeaponAssets>,
    cheats: Res<Cheats>,
    mut jets: Query<(Entity, &Jet, &Transform, &Velocity, &mut ControlInput, &mut Weapons, &LockState, Has<Player>)>,
    bullets: Query<(), With<Bullet>>,
    mut missiles: Query<&mut Missile>,
    mut sounds: EventWriter<SoundEvent>,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();
    let mut bullet_count = bullets.iter().count();
    let mut missile_count = missiles.iter().count();
    for (entity, jet, tf, vel, mut input, mut w, lock, is_player) in &mut jets {
        w.gun_cooldown -= dt;
        w.missile_cooldown -= dt;
        w.flare_cooldown -= dt;
        w.gun_sound_cooldown -= dt;
        let fwd = tf.forward();
        let infinite = is_player && cheats.god_mode;

        // Cannon.
        while input.fire_gun && w.gun_ammo > 0 && w.gun_cooldown <= 0.0 && bullet_count < MAX_BULLETS {
            w.gun_cooldown += 1.0 / GUN_RATE;
            if !infinite {
                w.gun_ammo -= 1;
            }
            bullet_count += 1;
            let spread = Vec3::new(rng.gen_range(-1.0..1.0), rng.gen_range(-1.0..1.0), 0.0) * GUN_SPREAD;
            let dir = (tf.rotation * (Vec3::NEG_Z + spread)).normalize();
            let v = vel.0 + dir * GUN_MUZZLE_SPEED;
            let pos = tf.translation + fwd * 9.0 + tf.right() * 1.2;
            spawn_bullet(&mut commands, &assets, pos, v, jet.side, is_player, BULLET_DAMAGE);
            if !is_player && w.gun_sound_cooldown <= 0.0 {
                w.gun_sound_cooldown = 0.3;
                sounds.send(SoundEvent::at(Sfx::GunBurst, tf.translation));
            }
        }
        if w.gun_cooldown < 0.0 {
            w.gun_cooldown = 0.0;
        }

        // Missile: guided if locked, otherwise unguided boresight shot.
        if std::mem::take(&mut input.fire_missile)
            && w.missiles > 0
            && w.missile_cooldown <= 0.0
            && missile_count < MAX_MISSILES
        {
            w.missile_cooldown = MISSILE_COOLDOWN;
            if !infinite {
                w.missiles -= 1;
            }
            missile_count += 1;
            let target = if lock.locked { lock.candidate } else { None };
            let pos = tf.translation - tf.up() * 2.0 + fwd * 2.0;
            spawn_missile(&mut commands, &assets, pos, vel.0 + fwd * 25.0, jet.side, is_player, target);
            sounds.send(SoundEvent::at(Sfx::MissileLaunch, tf.translation));
        }

        // Flares: each one may decoy every missile tracking this jet.
        if std::mem::take(&mut input.flare) && w.flares > 0 && w.flare_cooldown <= 0.0 {
            w.flare_cooldown = FLARE_COOLDOWN;
            if !infinite {
                w.flares -= 1;
            }
            for k in 0..2 {
                let side_kick = if k == 0 { tf.right() } else { -tf.right() };
                let fv = vel.0 * 0.85 + side_kick * 25.0 - tf.up() * 15.0;
                let flare = commands
                    .spawn((
                        Flare { life: FLARE_LIFE },
                        Velocity(fv),
                        Mesh3d(assets.flare.clone()),
                        MeshMaterial3d(assets.flare_mat.clone()),
                        Transform::from_translation(tf.translation - fwd * 8.0),
                        StateScoped(AppState::Playing),
                    ))
                    .id();
                for mut m in &mut missiles {
                    if m.target == Some(entity) && rng.gen_bool(FLARE_DECOY_CHANCE) {
                        m.target = Some(flare);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn move_bullets(
    mut commands: Commands,
    time: Res<Time>,
    hm: Res<Heightmap>,
    mut bullets: Query<(Entity, &mut Bullet, &mut Velocity, &mut Transform), Without<HitSphere>>,
    spheres: Query<(Entity, &HitSphere, &GlobalTransform, Option<&DamageRelay>, Option<&Velocity>), Without<Bullet>>,
    mut damage: EventWriter<DamageEvent>,
    mut detonations: EventWriter<Detonation>,
    mut particles: ResMut<Particles>,
) {
    let dt = time.delta_secs();
    let targets: Vec<_> = spheres
        .iter()
        .filter(|(_, s, ..)| s.radius > 0.0)
        .map(|(e, s, gt, relay, v)| (e, s.radius, s.side, gt.translation(), relay.map(|r| r.0), v.map(|v| v.0)))
        .collect();
    for (e, mut b, mut vel, mut tf) in &mut bullets {
        b.life -= dt;
        let a = tf.translation;
        let v = vel.0 + Vec3::new(0.0, -GRAVITY, 0.0) * dt;
        let bpos = a + v * dt;
        let mut hit = None;
        for &(te, r, side, c, relay, tv) in &targets {
            if side == Some(b.side) {
                continue;
            }
            // Test in the target's frame so fast jets don't tunnel:
            // `c` is where the target is now, it was at c - tv*dt.
            let a_rel = a + tv.unwrap_or(Vec3::ZERO) * dt;
            if let Some(t) = segment_hits_sphere(a_rel, bpos, c, r) {
                if hit.is_none_or(|(_, bt, _)| t < bt) {
                    hit = Some((relay.unwrap_or(te), t, a.lerp(bpos, t)));
                }
            }
        }
        if let Some((target, _, p)) = hit {
            damage.send(DamageEvent {
                target,
                amount: b.damage,
                kind: DamageKind::Bullet,
                by_player: b.by_player,
            });
            detonations.send(Detonation::fx(p, Boom::Spark));
            commands.entity(e).despawn_recursive();
            continue;
        }
        let ground = hm.surface(bpos.x, bpos.z);
        if bpos.y <= ground {
            if hm.is_water(bpos.x, bpos.z) {
                particles.splash(Vec3::new(bpos.x, ground, bpos.z));
            } else {
                particles.dust(Vec3::new(bpos.x, ground, bpos.z));
            }
            commands.entity(e).despawn_recursive();
            continue;
        }
        if b.life <= 0.0 {
            commands.entity(e).despawn_recursive();
            continue;
        }
        tf.translation = bpos;
        tf.look_to(v, Vec3::Y);
        vel.0 = v;
    }
}

#[allow(clippy::too_many_arguments)]
fn move_missiles(
    mut commands: Commands,
    time: Res<Time>,
    hm: Res<Heightmap>,
    mut missiles: Query<(Entity, &mut Missile, &mut Velocity, &mut Transform), Without<Targetable>>,
    targets: Query<(&GlobalTransform, &Velocity), Without<Missile>>,
    mut detonations: EventWriter<Detonation>,
    mut particles: ResMut<Particles>,
) {
    let dt = time.delta_secs();
    for (e, mut m, mut vel, mut tf) in &mut missiles {
        m.age += dt;
        let prev = tf.translation;
        // Seeker: lose the target if it leaves the gimbal or dies.
        let mut tgt = None;
        if let Some(t) = m.target {
            match targets.get(t) {
                Ok((gt, tv)) => {
                    let to = gt.translation() - prev;
                    if vel.0.angle_between(to) > MISSILE_SEEKER_GIMBAL {
                        m.target = None;
                    } else {
                        tgt = Some((gt.translation(), tv.0));
                    }
                }
                Err(_) => m.target = None,
            }
        }
        let (pos, v) = missile_step(prev, vel.0, m.age, tgt, prev.y, dt);
        vel.0 = v;
        tf.translation = pos;
        tf.rotation = Quat::from_rotation_arc(Vec3::Y, v.normalize_or_zero());

        m.trail_timer -= dt;
        if m.trail_timer <= 0.0 {
            m.trail_timer = 0.03;
            particles.trail(pos - v.normalize_or_zero() * 2.0, m.age < MISSILE_BURN_TIME);
        }

        let mut boom = false;
        if let Some((tp, tv)) = tgt {
            // Proximity fuse: closest approach during this step, in the
            // target's frame.
            if m.age > MISSILE_ARM_TIME {
                let a = prev - (tp - tv * dt);
                let b = pos - tp;
                if segment_hits_sphere(a, b, Vec3::ZERO, MISSILE_PROX_RADIUS).is_some() {
                    boom = true;
                }
            }
        }
        if pos.y <= hm.surface(pos.x, pos.z) || m.age > MISSILE_LIFE {
            boom = true;
        }
        if boom {
            detonations.send(Detonation {
                pos,
                size: Boom::Medium,
                damage: MISSILE_DAMAGE,
                radius: MISSILE_BLAST_RADIUS,
                by_player: m.by_player,
            });
            commands.entity(e).despawn_recursive();
        }
    }
}

fn move_flares(
    mut commands: Commands,
    time: Res<Time>,
    mut flares: Query<(Entity, &mut Flare, &mut Velocity, &mut Transform)>,
    mut particles: ResMut<Particles>,
) {
    let dt = time.delta_secs();
    for (e, mut f, mut v, mut tf) in &mut flares {
        f.life -= dt;
        let drag = v.0 * 0.6;
        v.0 += (Vec3::new(0.0, -GRAVITY * 0.5, 0.0) - drag) * dt;
        tf.translation += v.0 * dt;
        if (f.life * 30.0) as i32 % 2 == 0 {
            particles.flare_smoke(tf.translation);
        }
        if f.life <= 0.0 {
            commands.entity(e).despawn_recursive();
        }
    }
}

fn blast_damage(
    mut events: EventReader<Detonation>,
    spheres: Query<(Entity, &HitSphere, &GlobalTransform, Option<&DamageRelay>)>,
    mut damage: EventWriter<DamageEvent>,
) {
    for ev in events.read() {
        if ev.damage <= 0.0 {
            continue;
        }
        // One hit per receiver (a unit's models share one relay target).
        let mut hits: HashMap<Entity, f32> = HashMap::new();
        for (e, s, gt, relay) in &spheres {
            if s.radius <= 0.0 {
                continue;
            }
            let d = (gt.translation().distance(ev.pos) - s.radius).max(0.0);
            if d < ev.radius {
                let amount = ev.damage * (1.0 - d / ev.radius).powf(0.7);
                let key = relay.map(|r| r.0).unwrap_or(e);
                let entry = hits.entry(key).or_insert(0.0);
                *entry = entry.max(amount);
            }
        }
        for (target, amount) in hits {
            damage.send(DamageEvent {
                target,
                amount,
                kind: DamageKind::Blast,
                by_player: ev.by_player,
            });
        }
    }
}

fn apply_damage(
    mut events: EventReader<DamageEvent>,
    cheats: Res<Cheats>,
    mut q: Query<(&mut Health, Option<&mut JetDamage>, Has<Player>)>,
    mut sounds: EventWriter<SoundEvent>,
) {
    let mut rng = rand::thread_rng();
    for ev in events.read() {
        let Ok((mut hp, jet_dmg, is_player)) = q.get_mut(ev.target) else {
            continue;
        };
        if is_player && cheats.god_mode && ev.kind != DamageKind::Crash {
            continue;
        }
        hp.hp -= ev.amount;
        hp.last_hit_by_player |= ev.by_player;
        if is_player && matches!(ev.kind, DamageKind::Bullet | DamageKind::Blast) {
            sounds.send(SoundEvent::ui(Sfx::Hit));
        }
        let Some(mut jd) = jet_dmg else {
            continue;
        };
        if matches!(ev.kind, DamageKind::Bullet | DamageKind::Blast) {
            let scale = if ev.kind == DamageKind::Blast { 2.5 } else { 1.0 };
            if rng.gen_bool((CHANCE_ENGINE_HIT * scale).min(1.0)) {
                jd.engine_damaged = true;
            }
            if rng.gen_bool((CHANCE_TANK_HIT * scale).min(1.0)) {
                jd.fuel_leak = (jd.fuel_leak + rng.gen_range(LEAK_RATE_MIN..LEAK_RATE_MAX)).min(LEAK_RATE_MAX * 2.0);
            }
            if rng.gen_bool((CHANCE_FIRE * scale).min(1.0)) {
                jd.on_fire = true;
            }
        }
    }
}

fn burning(
    time: Res<Time>,
    mut q: Query<(&Transform, &mut Health, &mut JetDamage)>,
    mut particles: ResMut<Particles>,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();
    for (tf, mut hp, mut jd) in &mut q {
        let back = tf.translation + tf.back() * 7.0;
        if jd.on_fire {
            hp.hp -= FIRE_DPS * dt;
            if rng.gen_bool(FIRE_OUT_CHANCE_PER_S * dt as f64) {
                jd.on_fire = false;
            }
            if rng.gen_bool(0.5) {
                particles.fire_trail(back);
            }
        } else if jd.engine_damaged && rng.gen_bool(0.3) {
            particles.smoke_trail(back);
        }
        if jd.fuel_leak > 0.0 && rng.gen_bool(0.4) {
            particles.vapor(back + tf.right() * 3.0);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn kill_dead(
    mut commands: Commands,
    q: Query<(Entity, &Health, &Transform, Option<&Jet>, Has<Player>)>,
    mut detonations: EventWriter<Detonation>,
    mut destroyed: EventWriter<JetDestroyed>,
) {
    for (e, hp, tf, jet, is_player) in &q {
        if hp.hp > 0.0 {
            continue;
        }
        detonations.send(Detonation {
            pos: tf.translation,
            size: Boom::Large,
            damage: JET_CRASH_DAMAGE,
            radius: JET_CRASH_RADIUS,
            by_player: false,
        });
        if let Some(jet) = jet {
            destroyed.send(JetDestroyed {
                side: jet.side,
                pos: tf.translation,
                by_player: hp.last_hit_by_player,
                was_player: is_player,
            });
        }
        commands.entity(e).despawn_recursive();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_sphere() {
        let c = Vec3::new(0.0, 0.0, -50.0);
        assert!(segment_hits_sphere(Vec3::ZERO, Vec3::new(0.0, 0.0, -100.0), c, 1.0).is_some());
        assert!(segment_hits_sphere(Vec3::ZERO, Vec3::new(0.0, 0.0, -40.0), c, 1.0).is_none());
        assert!(segment_hits_sphere(Vec3::X * 5.0, Vec3::new(5.0, 0.0, -100.0), c, 4.0).is_none());
        let t = segment_hits_sphere(Vec3::ZERO, Vec3::new(0.0, 0.0, -100.0), c, 1.0).unwrap();
        assert!((t - 0.5).abs() < 1e-4);
    }

    #[test]
    fn pn_turns_towards_crossing_target() {
        // Target ahead, moving left: missile should accelerate left.
        let a = pn_accel(Vec3::ZERO, Vec3::new(0.0, 0.0, -600.0), Vec3::new(0.0, 0.0, -3000.0), Vec3::new(-250.0, 0.0, 0.0), 4.0);
        assert!(a.x < 0.0, "{a:?}");
        // Pure collision course: no LOS rotation, no command.
        let a = pn_accel(Vec3::ZERO, Vec3::new(0.0, 0.0, -600.0), Vec3::new(0.0, 0.0, -3000.0), Vec3::new(0.0, 0.0, 100.0), 4.0);
        assert!(a.length() < 1e-3);
    }

    #[test]
    fn missile_intercepts_crossing_target() {
        let dt = 1.0 / 60.0;
        let (mut mp, mut mv) = (Vec3::new(0.0, 3000.0, 0.0), Vec3::new(0.0, 0.0, -250.0));
        let mut tp = Vec3::new(1500.0, 3200.0, -3500.0);
        let tv = Vec3::new(-230.0, 0.0, 40.0);
        let mut best = f32::MAX;
        let mut age = 0.0;
        while age < MISSILE_LIFE {
            let (p, v) = missile_step(mp, mv, age, Some((tp, tv)), mp.y, dt);
            tp += tv * dt;
            best = best.min(segment_hits_sphere(mp, p, tp, 1e9).map(|_| (p - tp).length()).unwrap());
            mp = p;
            mv = v;
            age += dt;
            if best < MISSILE_PROX_RADIUS {
                break;
            }
        }
        assert!(best < MISSILE_PROX_RADIUS, "miss distance {best}");
    }

    #[test]
    fn unguided_missile_flies_roughly_straight() {
        let dt = 1.0 / 60.0;
        let (mut p, mut v) = (Vec3::new(0.0, 2000.0, 0.0), Vec3::new(0.0, 0.0, -250.0));
        for i in 0..(60 * 4) {
            (p, v) = missile_step(p, v, i as f32 * dt, None, p.y, dt);
        }
        assert!((p.y - 2000.0).abs() < 30.0, "dropped to {}", p.y);
        assert!(v.length() > 600.0, "boost too weak: {}", v.length());
    }
}
