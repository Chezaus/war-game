//! Infrastructure (§6.4): a power plant, fuel depot and radar per side.
//! Sites take bullet/blast damage like everything else; losing them
//! weakens that side (radar → AA and detection, fuel → restock and jet
//! launches, power → both). Also enemy strike raids on NATO sites and
//! ground units shelling enemy sites in range.

use bevy::prelude::*;
use rand::seq::SliceRandom;
use rand::Rng;

use crate::config::*;
use crate::flight::{Jet, Velocity};
use crate::fx::Particles;
use crate::game_rules::{EventLog, RoundStats};
use crate::ground::{combat_damage, GroundUnit};
use crate::pilot_ai::{AiPilot, AiState};
use crate::state::{AppState, GameplaySet};
use crate::weapons::{Boom, DamageEvent, DamageKind, DamageRelay, Detonation, HitSphere, Weapons};
use crate::world::Heightmap;

#[derive(Component, Debug)]
pub struct Site {
    pub name: String,
    pub side: Side,
    pub kind: SiteKind,
    pub hp: f32,
    pub alive: bool,
    /// Centre of the hit sphere: what strikes aim at.
    pub aim: Vec3,
}

impl Site {
    pub fn stats(&self) -> SiteStats {
        site_stats(self.kind)
    }
}

/// Model parts hidden when the site is destroyed.
#[derive(Component)]
struct SiteIntact;
/// Shown when the site is destroyed.
#[derive(Component)]
struct SiteRubble;
#[derive(Component)]
struct RadarDish;
#[derive(Component)]
struct SiteHitbox;

/// What a side's infrastructure still provides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SideInfra {
    pub power: bool,
    pub fuel: bool,
    pub radar: bool,
}

impl SideInfra {
    pub const INTACT: Self = Self { power: true, fuel: true, radar: true };

    /// The radar needs power.
    pub fn radar_up(&self) -> bool {
        self.radar && self.power
    }
    /// So do the fuel pumps.
    pub fn fuel_up(&self) -> bool {
        self.fuel && self.power
    }
}

#[derive(Resource, Debug)]
pub struct Infra {
    pub nato: SideInfra,
    pub enemy: SideInfra,
}

impl Default for Infra {
    fn default() -> Self {
        Self { nato: SideInfra::INTACT, enemy: SideInfra::INTACT }
    }
}

impl Infra {
    pub fn get(&self, side: Side) -> SideInfra {
        match side {
            Side::Nato => self.nato,
            Side::Enemy => self.enemy,
        }
    }
}

#[derive(Resource, Default)]
struct InfraTimers {
    strike: f32,
    shelling: f32,
    smoke: f32,
}

pub struct InfrastructurePlugin;

impl Plugin for InfrastructurePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Infra>()
            .init_resource::<InfraTimers>()
            .add_systems(OnEnter(AppState::Playing), spawn_sites)
            .add_systems(
                Update,
                (site_damage, update_infra, ground_shelling, enemy_strikes, site_fx)
                    .chain()
                    .in_set(GameplaySet),
            );
    }
}

fn site_name(side: Side, kind: SiteKind) -> String {
    let prefix = match side {
        Side::Nato => "NATO",
        Side::Enemy => "Enemy",
    };
    format!("{prefix} {}", site_stats(kind).name)
}

fn spawn_sites(
    mut commands: Commands,
    hm: Res<Heightmap>,
    mut infra: ResMut<Infra>,
    mut timers: ResMut<InfraTimers>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    *infra = Infra::default();
    *timers = InfraTimers { strike: ENEMY_STRIKE_FIRST, ..default() };
    let concrete = materials.add(StandardMaterial {
        base_color: Color::srgb(0.62, 0.61, 0.58),
        perceptual_roughness: 0.9,
        ..default()
    });
    let tank = materials.add(Color::srgb(0.82, 0.82, 0.78));
    let steel = materials.add(Color::srgb(0.32, 0.34, 0.38));
    let rubble = materials.add(Color::srgb(0.12, 0.11, 0.10));
    let hall = meshes.add(Cuboid::new(60.0, 22.0, 35.0));
    let tower = meshes.add(Cylinder::new(14.0, 45.0));
    let chimney = meshes.add(Cylinder::new(2.5, 70.0));
    let fuel_tank = meshes.add(Cylinder::new(9.0, 10.0));
    let pump = meshes.add(Cuboid::new(10.0, 5.0, 8.0));
    let radar_hut = meshes.add(Cuboid::new(12.0, 8.0, 12.0));
    let mast = meshes.add(Cylinder::new(1.2, 16.0));
    let dish = meshes.add(Cuboid::new(14.0, 6.0, 1.0));

    let sites = NATO_SITES.iter().map(|s| (Side::Nato, *s)).chain(ENEMY_SITES.iter().map(|s| (Side::Enemy, *s)));
    for (side, (kind, ll)) in sites {
        let [x, z] = geo(ll.0, ll.1);
        let stats = site_stats(kind);
        let ground = hm.height(x, z).max(SEA_LEVEL);
        let pos = Vec3::new(x, ground, z);
        let roof = materials.add(match side {
            Side::Nato => Color::srgb(0.35, 0.42, 0.50),
            Side::Enemy => Color::srgb(0.50, 0.30, 0.25),
        });
        let root = commands
            .spawn((
                Name::new(site_name(side, kind)),
                Site {
                    name: site_name(side, kind),
                    side,
                    kind,
                    hp: stats.hp,
                    alive: true,
                    aim: pos + Vec3::Y * stats.hit_radius * 0.5,
                },
                Transform::from_translation(pos),
                // Stationary target for guided strike missiles.
                Velocity(Vec3::ZERO),
                Visibility::default(),
                StateScoped(AppState::Playing),
            ))
            .id();
        commands.entity(root).with_children(|p| {
            p.spawn((
                SiteHitbox,
                HitSphere { radius: stats.hit_radius, side: Some(side) },
                DamageRelay(root),
                Transform::from_xyz(0.0, stats.hit_radius * 0.5, 0.0),
            ));
            p.spawn((
                SiteRubble,
                Mesh3d(meshes.add(Cuboid::new(stats.hit_radius * 1.6, 3.0, stats.hit_radius * 1.4))),
                MeshMaterial3d(rubble.clone()),
                Transform::from_xyz(0.0, 1.5, 0.0),
                Visibility::Hidden,
            ));
            let mut part = |mesh: &Handle<Mesh>, mat: &Handle<StandardMaterial>, tf: Transform| {
                p.spawn((SiteIntact, Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), tf));
            };
            match kind {
                SiteKind::PowerPlant => {
                    part(&hall, &roof, Transform::from_xyz(10.0, 11.0, 0.0));
                    part(&tower, &concrete, Transform::from_xyz(-40.0, 22.5, 22.0));
                    part(&tower, &concrete, Transform::from_xyz(-40.0, 22.5, -22.0));
                    part(&chimney, &concrete, Transform::from_xyz(32.0, 35.0, 10.0));
                }
                SiteKind::FuelDepot => {
                    for (tx, tz) in [(-14.0, -14.0), (14.0, -14.0), (-14.0, 14.0), (14.0, 14.0)] {
                        part(&fuel_tank, &tank, Transform::from_xyz(tx, 5.0, tz));
                    }
                    part(&pump, &roof, Transform::from_xyz(0.0, 2.5, 30.0));
                }
                SiteKind::Radar => {
                    part(&radar_hut, &roof, Transform::from_xyz(0.0, 4.0, 0.0));
                    part(&mast, &steel, Transform::from_xyz(0.0, 16.0, 0.0));
                    p.spawn((
                        SiteIntact,
                        RadarDish,
                        Mesh3d(dish.clone()),
                        MeshMaterial3d(steel.clone()),
                        Transform::from_xyz(0.0, 26.0, 0.0).with_rotation(Quat::from_rotation_x(-0.35)),
                    ));
                }
            }
        });
    }
}

/// Bullets and blasts relayed from the hitbox (§6.4).
#[allow(clippy::too_many_arguments)]
fn site_damage(
    mut events: EventReader<DamageEvent>,
    mut sites: Query<(&mut Site, &Children)>,
    mut parts: Query<(&mut Visibility, Has<SiteIntact>, Has<SiteRubble>)>,
    mut hitboxes: Query<&mut HitSphere, With<SiteHitbox>>,
    mut detonations: EventWriter<Detonation>,
    mut log: ResMut<EventLog>,
    mut stats: ResMut<RoundStats>,
) {
    for ev in events.read() {
        let Ok((mut site, children)) = sites.get_mut(ev.target) else {
            continue;
        };
        if !site.alive {
            continue;
        }
        let factor = if ev.kind == DamageKind::Bullet { SITE_BULLET_FACTOR } else { 1.0 };
        site.hp -= ev.amount * factor;
        if site.hp > 0.0 {
            continue;
        }
        site.alive = false;
        site.hp = 0.0;
        let effect = match site.kind {
            SiteKind::PowerPlant => "radar and fuel pumps lose power",
            SiteKind::FuelDepot => match site.side {
                Side::Nato => "restocking will be slow",
                Side::Enemy => "enemy jets will launch less often",
            },
            SiteKind::Radar => match site.side {
                Side::Nato => "our AA is weaker and distant enemy jets drop off the map",
                Side::Enemy => "enemy AA is weaker and their pilots are half-blind",
            },
        };
        log.push(format!("{} destroyed: {effect}", site.name));
        match site.side {
            Side::Enemy => stats.enemy_sites_destroyed += 1,
            Side::Nato => stats.nato_sites_lost += 1,
        }
        let r = site.stats().hit_radius;
        for k in 0..3 {
            let off = Vec3::new((k as f32 - 1.0) * r * 0.6, 0.0, 0.0);
            detonations.send(Detonation::fx(site.aim + off, Boom::Large));
        }
        for &c in children {
            if let Ok((mut vis, intact, rubble)) = parts.get_mut(c) {
                if intact {
                    *vis = Visibility::Hidden;
                } else if rubble {
                    *vis = Visibility::Inherited;
                }
            }
            if let Ok(mut sphere) = hitboxes.get_mut(c) {
                sphere.radius = 0.0;
            }
        }
    }
}

fn update_infra(sites: Query<&Site>, mut infra: ResMut<Infra>) {
    let mut next = Infra::default();
    for s in &sites {
        if s.alive {
            continue;
        }
        let side = match s.side {
            Side::Nato => &mut next.nato,
            Side::Enemy => &mut next.enemy,
        };
        match s.kind {
            SiteKind::PowerPlant => side.power = false,
            SiteKind::FuelDepot => side.fuel = false,
            SiteKind::Radar => side.radar = false,
        }
    }
    if infra.nato != next.nato || infra.enemy != next.enemy {
        *infra = next;
    }
}

/// Ground units with nothing to fight shell enemy sites in range.
fn ground_shelling(
    time: Res<Time>,
    mut timers: ResMut<InfraTimers>,
    units: Query<(&GroundUnit, &Transform)>,
    sites: Query<(Entity, &Site)>,
    mut damage: EventWriter<DamageEvent>,
    mut detonations: EventWriter<Detonation>,
) {
    timers.shelling -= time.delta_secs();
    if timers.shelling > 0.0 {
        return;
    }
    timers.shelling = COMBAT_TICK;
    let mut rng = rand::thread_rng();
    for (u, tf) in &units {
        if u.engaged || u.retreating {
            continue;
        }
        let stats = u.stats();
        let target = sites
            .iter()
            .filter(|(_, s)| s.alive && s.side != u.side)
            .map(|(e, s)| (e, s, s.aim.xz().distance(tf.translation.xz())))
            .filter(|(.., d)| *d < stats.range)
            .min_by(|a, b| a.2.total_cmp(&b.2));
        let Some((e, s, _)) = target else {
            continue;
        };
        let amount = combat_damage(stats.attack, u.strength, 1.0, rng.gen_range(0.8..1.2)) * SITE_GROUND_FACTOR;
        damage.send(DamageEvent { target: e, amount, kind: DamageKind::Blast, by_player: false });
        if rng.gen_bool(0.5) {
            let r = s.stats().hit_radius;
            let jitter = Vec3::new(rng.gen_range(-r..r), 0.0, rng.gen_range(-r..r));
            detonations.send(Detonation::fx(s.aim + jitter, Boom::Spark));
        }
    }
}

/// Every few minutes an enemy fighter is sent to bomb a NATO site.
fn enemy_strikes(
    time: Res<Time>,
    mut timers: ResMut<InfraTimers>,
    sites: Query<(Entity, &Site)>,
    mut pilots: Query<(&Jet, &Weapons, &mut AiPilot)>,
    mut log: ResMut<EventLog>,
) {
    // Called off if the target is already gone.
    for (_, _, mut ai) in &mut pilots {
        if let Some((e, _)) = ai.strike {
            if !sites.get(e).is_ok_and(|(_, s)| s.alive) {
                ai.strike = None;
            }
        }
    }
    timers.strike -= time.delta_secs();
    if timers.strike > 0.0 {
        return;
    }
    let mut rng = rand::thread_rng();
    let targets: Vec<(Entity, &Site)> = sites.iter().filter(|(_, s)| s.side == Side::Nato && s.alive).collect();
    let Some(&(target, site)) = targets.choose(&mut rng) else {
        timers.strike = ENEMY_STRIKE_INTERVAL;
        return;
    };
    let striker = pilots.iter_mut().find(|(jet, w, ai)| {
        jet.side == Side::Enemy && ai.strike.is_none() && w.missiles > 0 && matches!(ai.state, AiState::Patrol | AiState::Engage)
    });
    let Some((_, _, mut ai)) = striker else {
        timers.strike = 15.0; // nobody free yet; try again soon
        return;
    };
    ai.strike = Some((target, site.aim));
    ai.strike_shots = 0;
    timers.strike = ENEMY_STRIKE_INTERVAL;
    log.push(format!("Enemy strike inbound on the {}!", site.name));
}

fn site_fx(
    time: Res<Time>,
    infra: Res<Infra>,
    mut timers: ResMut<InfraTimers>,
    sites: Query<(&Site, &Children)>,
    mut dishes: Query<&mut Transform, With<RadarDish>>,
    mut particles: ResMut<Particles>,
) {
    let dt = time.delta_secs();
    timers.smoke -= dt;
    let puff = timers.smoke <= 0.0;
    if puff {
        timers.smoke = 0.25;
    }
    let mut rng = rand::thread_rng();
    for (s, children) in &sites {
        if !s.alive {
            if puff {
                let r = s.stats().hit_radius * 0.5;
                let p = s.aim + Vec3::new(rng.gen_range(-r..r), 0.0, rng.gen_range(-r..r));
                particles.smoke_trail(p);
                if rng.gen_bool(0.3) {
                    particles.fire_trail(p);
                }
            }
            continue;
        }
        if s.kind == SiteKind::Radar && infra.get(s.side).radar_up() {
            for &c in children {
                if let Ok(mut tf) = dishes.get_mut(c) {
                    tf.rotate_y(dt * 1.2);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_feeds_radar_and_fuel() {
        let mut s = SideInfra::INTACT;
        assert!(s.radar_up() && s.fuel_up());
        s.power = false;
        assert!(!s.radar_up() && !s.fuel_up());
        let s = SideInfra { radar: false, ..SideInfra::INTACT };
        assert!(!s.radar_up() && s.fuel_up());
    }

    #[test]
    fn sites_are_on_land() {
        let hm = Heightmap::load();
        for (kind, ll) in NATO_SITES.iter().chain(ENEMY_SITES.iter()) {
            let [x, z] = geo(ll.0, ll.1);
            assert!(!hm.is_water(x, z), "{kind:?} at {ll:?} is in the water");
        }
        // Each side's sites sit on its own side of the front.
        for (_, ll) in NATO_SITES {
            assert!(geo(ll.0, ll.1)[0] < FRONT_CENTER[0], "NATO sites are west of the front");
        }
        for (_, ll) in ENEMY_SITES {
            assert!(geo(ll.0, ll.1)[0] > FRONT_CENTER[0], "enemy sites are east of the front");
        }
    }
}
