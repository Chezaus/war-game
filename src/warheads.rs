//! Ballistic warheads (§6.7): both sides keep a few missiles in silos on
//! their mainland (NATO in America, the enemy in Siberia). From the map you
//! pick an impact point; the missile climbs out of its silo, flies a high
//! arc across the mini Earth and lands with a huge blast that wrecks units,
//! bridges, sites and jets. The enemy fires back at NATO concentrations.

use bevy::prelude::*;
use rand::Rng;

use crate::audio::{Sfx, SoundEvent};
use crate::config::*;
use crate::fx::{PKind, Particles};
use crate::game_rules::EventLog;
use crate::geo;
use crate::ground::GroundUnit;
use crate::infrastructure::Site;
use crate::state::{AppState, GameplaySet};
use crate::weapons::{Boom, Detonation};
use crate::world::{base_center, Heightmap};

#[derive(Clone, Copy, Debug)]
pub struct Silo {
    pub stock: u32,
    /// Seconds until the next launch is possible.
    pub cooldown: f32,
}

impl Silo {
    pub fn ready(&self) -> bool {
        self.stock > 0 && self.cooldown <= 0.0
    }
}

#[derive(Resource, Debug)]
pub struct Silos {
    pub nato: Silo,
    pub enemy: Silo,
    enemy_timer: f32,
}

impl Default for Silos {
    fn default() -> Self {
        Self {
            nato: Silo { stock: WARHEAD_STOCK, cooldown: 0.0 },
            enemy: Silo { stock: ENEMY_WARHEAD_STOCK, cooldown: 0.0 },
            enemy_timer: ENEMY_WARHEAD_FIRST,
        }
    }
}

impl Silos {
    pub fn get_mut(&mut self, side: Side) -> &mut Silo {
        match side {
            Side::Nato => &mut self.nato,
            Side::Enemy => &mut self.enemy,
        }
    }
}

/// Map targeting mode: the next left click on the map is the impact point.
#[derive(Resource, Default)]
pub struct WarheadTargeting(pub bool);

#[derive(Event, Debug, Clone, Copy)]
pub struct LaunchWarhead {
    pub side: Side,
    pub target: Vec3,
}

#[derive(Component, Debug)]
pub struct Warhead {
    pub side: Side,
    pub start: Vec3,
    pub target: Vec3,
    pub age: f32,
    pub flight: f32,
    pub apex: f32,
    trail: f32,
}

impl Warhead {
    pub fn new(side: Side, start: Vec3, target: Vec3) -> Self {
        let dist = start.xz().distance(target.xz());
        Self { side, start, target, age: 0.0, flight: flight_time(dist), apex: apex_height(dist), trail: 0.0 }
    }
    /// Position at flight fraction `s` (0 = silo, 1 = impact).
    pub fn position(&self, s: f32) -> Vec3 {
        self.start.lerp(self.target, s) + Vec3::Y * self.apex * 4.0 * s * (1.0 - s)
    }
    pub fn remaining(&self) -> f32 {
        (self.flight - self.age).max(0.0)
    }
}

pub fn flight_time(dist: f32) -> f32 {
    WARHEAD_BASE_TIME + dist / WARHEAD_SPEED
}

pub fn apex_height(dist: f32) -> f32 {
    (dist * WARHEAD_APEX_FRAC).clamp(WARHEAD_APEX_MIN, WARHEAD_APEX_MAX)
}

pub fn silo_pos(side: Side) -> [f32; 2] {
    match side {
        Side::Nato => NATO_SILO,
        Side::Enemy => ENEMY_SILO,
    }
}

/// Name of the city nearest a point, for the event log.
pub fn near_city(p: Vec3) -> &'static str {
    geo::CITIES
        .iter()
        .min_by(|a, b| {
            let (da, db) = (Vec2::from(geo::geo_ll(a.1)), Vec2::from(geo::geo_ll(b.1)));
            da.distance(p.xz()).total_cmp(&db.distance(p.xz()))
        })
        .map(|c| c.0)
        .unwrap_or("?")
}

/// The enemy's aim: the NATO unit with the most friends within the blast
/// (plus a margin); without units, a NATO site; else the NATO airbase.
pub fn pick_enemy_target(nato_units: &[Vec3], nato_sites: &[Vec3]) -> Vec3 {
    let crowd = |p: Vec3| nato_units.iter().filter(|o| o.xz().distance(p.xz()) < WARHEAD_RADIUS * 1.5).count();
    nato_units
        .iter()
        .copied()
        .max_by_key(|p| crowd(*p))
        .or_else(|| nato_sites.first().copied())
        .unwrap_or_else(|| base_center(&NATO_BASE))
}

#[derive(Resource)]
struct WarheadAssets {
    mesh: Handle<Mesh>,
    nato: Handle<StandardMaterial>,
    enemy: Handle<StandardMaterial>,
}

pub struct WarheadsPlugin;

impl Plugin for WarheadsPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<LaunchWarhead>()
            .init_resource::<Silos>()
            .init_resource::<WarheadTargeting>()
            .add_systems(Startup, setup_assets)
            .add_systems(OnEnter(AppState::Playing), |mut c: Commands| {
                c.insert_resource(Silos::default());
                c.insert_resource(WarheadTargeting::default());
            })
            .add_systems(Update, (enemy_launches, launch, fly).chain().in_set(GameplaySet));
    }
}

fn setup_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let body = |c: LinearRgba| StandardMaterial {
        base_color: Color::srgb(0.9, 0.9, 0.88),
        emissive: c,
        ..default()
    };
    commands.insert_resource(WarheadAssets {
        // Oversized so it reads from kilometres away.
        mesh: meshes.add(Capsule3d::new(4.0, 36.0)),
        nato: materials.add(body(LinearRgba::rgb(0.6, 0.8, 2.0))),
        enemy: materials.add(body(LinearRgba::rgb(2.0, 0.5, 0.3))),
    });
}

fn enemy_launches(
    time: Res<Time>,
    mut silos: ResMut<Silos>,
    units: Query<(&GroundUnit, &Transform)>,
    sites: Query<&Site>,
    mut launches: EventWriter<LaunchWarhead>,
) {
    let dt = time.delta_secs();
    silos.nato.cooldown -= dt;
    silos.enemy.cooldown -= dt;
    silos.enemy_timer -= dt;
    if silos.enemy_timer > 0.0 || !silos.enemy.ready() {
        return;
    }
    let mut rng = rand::thread_rng();
    silos.enemy_timer = ENEMY_WARHEAD_INTERVAL * rng.gen_range(0.75..1.25);
    let nato_units: Vec<Vec3> = units
        .iter()
        .filter(|(u, _)| u.side == Side::Nato)
        .map(|(_, tf)| tf.translation)
        .collect();
    let nato_sites: Vec<Vec3> = sites.iter().filter(|s| s.side == Side::Nato && s.alive).map(|s| s.aim).collect();
    let aim = pick_enemy_target(&nato_units, &nato_sites);
    let a = rng.gen_range(0.0..std::f32::consts::TAU);
    let miss = Vec3::new(a.cos(), 0.0, a.sin()) * rng.gen_range(0.0..ENEMY_WARHEAD_ERROR);
    launches.send(LaunchWarhead { side: Side::Enemy, target: aim + miss });
}

fn launch(
    mut commands: Commands,
    mut events: EventReader<LaunchWarhead>,
    hm: Res<Heightmap>,
    assets: Res<WarheadAssets>,
    mut silos: ResMut<Silos>,
    mut log: ResMut<EventLog>,
    mut sounds: EventWriter<SoundEvent>,
) {
    for ev in events.read() {
        let silo = silos.get_mut(ev.side);
        if !silo.ready() {
            continue;
        }
        silo.stock -= 1;
        silo.cooldown = WARHEAD_COOLDOWN;
        let [sx, sz] = silo_pos(ev.side);
        let start = Vec3::new(sx, hm.surface(sx, sz) + 20.0, sz);
        let target = Vec3::new(ev.target.x, hm.surface(ev.target.x, ev.target.z), ev.target.z);
        let w = Warhead::new(ev.side, start, target);
        let city = near_city(target);
        log.push(match ev.side {
            Side::Nato => format!("Warhead away from America: impact near {city} in {:.0} s", w.flight),
            Side::Enemy => format!("ENEMY BALLISTIC MISSILE! Impact near {city} in {:.0} s, clear the area", w.flight),
        });
        sounds.send(SoundEvent::ui(Sfx::MissileLaunch));
        commands.spawn((
            Name::new("warhead"),
            Mesh3d(assets.mesh.clone()),
            MeshMaterial3d(match ev.side {
                Side::Nato => assets.nato.clone(),
                Side::Enemy => assets.enemy.clone(),
            }),
            Transform::from_translation(start),
            w,
            StateScoped(AppState::Playing),
        ));
    }
}

fn fly(
    mut commands: Commands,
    time: Res<Time>,
    mut q: Query<(Entity, &mut Warhead, &mut Transform)>,
    mut particles: ResMut<Particles>,
    mut detonations: EventWriter<Detonation>,
    mut log: ResMut<EventLog>,
) {
    let dt = time.delta_secs();
    for (e, mut w, mut tf) in &mut q {
        w.age += dt;
        let s = (w.age / w.flight).min(1.0);
        let pos = w.position(s);
        let ahead = w.position((s + 0.002).min(1.0)) - pos;
        tf.translation = pos;
        if ahead.length_squared() > 1e-6 {
            tf.rotation = Quat::from_rotation_arc(Vec3::Y, ahead.normalize());
        }
        w.trail -= dt;
        if w.trail <= 0.0 {
            w.trail = 0.05;
            let back = pos - ahead.normalize_or_zero() * 25.0;
            particles.spawn(PKind::Smoke, back, Vec3::ZERO, 10.0, 70.0, 7.0, 0.0, 0.3);
            if s < 0.3 {
                particles.spawn(PKind::Fire, back, Vec3::ZERO, 14.0, 4.0, 0.25, 0.0, 0.0);
            }
        }
        if s >= 1.0 {
            detonations.send(Detonation {
                pos: w.target + Vec3::Y * 5.0,
                size: Boom::Huge,
                damage: WARHEAD_DAMAGE,
                radius: WARHEAD_RADIUS,
                by_player: w.side == Side::Nato,
            });
            log.push(format!("Warhead impact near {}", near_city(w.target)));
            commands.entity(e).despawn_recursive();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_starts_and_ends_on_target() {
        let w = Warhead::new(Side::Nato, Vec3::new(-40_000.0, 10.0, 0.0), Vec3::new(5_000.0, 50.0, -20_000.0));
        assert!(w.position(0.0).distance(w.start) < 1e-3);
        assert!(w.position(1.0).distance(w.target) < 1e-3);
        assert!(w.position(0.5).y > w.apex * 0.9);
        assert!(w.flight > WARHEAD_BASE_TIME);
    }

    #[test]
    fn enemy_aims_at_the_crowd() {
        let lone = Vec3::new(10_000.0, 0.0, 0.0);
        let crowd = [Vec3::ZERO, Vec3::new(300.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 400.0)];
        let mut units = vec![lone];
        units.extend(crowd);
        assert!(pick_enemy_target(&units, &[]).distance(Vec3::ZERO) < 500.0);
        let site = Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(pick_enemy_target(&[], &[site]), site);
    }
}
