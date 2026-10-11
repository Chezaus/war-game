//! Aggregated ground units (§6.2): auto-advance and player orders, road
//! and bridge path finding, forest cover, auto-combat, retreat, anti-air
//! fire, the enemy wave script, and the visual model clusters that shrink
//! with strength. Infantry clusters are squads of individually modelled
//! soldiers (§6.5).

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use rand::Rng;

use crate::config::*;
use crate::flight::{Jet, Velocity};
use crate::game_rules::{EventLog, RoundStats};
use crate::infrastructure::Site;
use crate::nav::{self, NavGrid, RoadNet};
use crate::state::{AppState, GameplaySet};
use crate::weapons::{spawn_missile, Boom, DamageEvent, DamageKind, DamageRelay, Detonation, HitSphere, WeaponAssets};
use crate::world::{base_center, Bridges, Heightmap};

#[derive(Component, Debug)]
pub struct GroundUnit {
    pub name: String,
    pub side: Side,
    pub kind: UnitKind,
    pub strength: f32,
    /// Remaining waypoints (x, z in world; y ignored).
    pub path: Vec<Vec3>,
    pub retreating: bool,
    pub engaged: bool,
    pub aa_cooldown: f32,
    /// Enemy wave 2 waits until released.
    pub holding: bool,
    pub heading: f32,
    pub order: Order,
    /// Seconds until an auto unit re-thinks its target.
    pub replan: f32,
    /// What an auto unit is heading for (map panel).
    pub auto_goal: Option<(Vec3, String)>,
}

/// Who decides where a unit goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Order {
    /// Picks its own targets: the nearest reachable enemy, else the enemy
    /// base, else enemy infrastructure. Anti-air escorts friendly units.
    Auto,
    /// Following a player order. Once it arrives it holds `resume`
    /// seconds, then goes back to auto.
    Manual { resume: f32 },
    /// Told to stay put until ordered again.
    Hold,
}

impl GroundUnit {
    pub fn stats(&self) -> UnitStats {
        unit_stats(self.kind)
    }
    pub fn destination(&self) -> Option<Vec3> {
        self.path.last().copied()
    }
}

#[derive(Component)]
pub struct UnitModel {
    pub unit: Entity,
    pub index: usize,
    pub offset: Vec2,
}

/// Engagement midpoints, refreshed every combat tick (map battle icons).
#[derive(Resource, Default)]
pub struct Battles(pub Vec<Vec3>);

/// IR drone view (§6.5): enemy troops and vehicles glow hot.
#[derive(Resource, Default)]
pub struct IrView(pub bool);

#[derive(Resource, Default)]
pub struct EnemyScript {
    pub elapsed: f32,
    pub wave2_done: bool,
    combat_timer: f32,
    /// Wave 2 found no way west; retry after this.
    wave2_retry: f32,
    stall_logged: bool,
    bridge_generation: u32,
}

// ---------------------------------------------------------------------------
// Pure rules (unit-tested)
// ---------------------------------------------------------------------------

/// §6.2: damage = attack × (strength/100) × random(0.8–1.2) ÷ defense,
/// scaled by COMBAT_SCALE.
pub fn combat_damage(attack: f32, attacker_strength: f32, defense: f32, roll: f32) -> f32 {
    attack * (attacker_strength / 100.0) * roll / defense * COMBAT_SCALE
}

/// Where an anti-air unit should stand to cover `friend`.
pub fn escort_point(friend: Vec3, home: Vec3) -> Vec3 {
    friend + (home - friend).with_y(0.0).normalize_or_zero() * AA_ESCORT_OFFSET
}

/// Map, roads, nav grid and bridges: everything path finding needs.
#[derive(SystemParam)]
pub struct Terrain<'w> {
    pub hm: Res<'w, Heightmap>,
    pub roads: Res<'w, RoadNet>,
    pub nav: Res<'w, NavGrid>,
    pub bridges: Res<'w, Bridges>,
}

impl Terrain<'_> {
    /// Route over land, along roads where it pays, across intact bridges.
    /// `None` if water (or a fallen bridge) is in the way.
    pub fn plan(&self, from: Vec3, to: Vec3, kind: UnitKind) -> Option<Vec<Vec3>> {
        self.nav
            .find_path(&self.hm, &self.roads, &self.bridges, from.xz(), to.xz(), kind)
            .map(|pts| pts.into_iter().map(|p| Vec3::new(p.x, 0.0, p.y)).collect())
    }

    /// Does the rest of this path still avoid water (no fallen bridges)?
    pub fn path_ok(&self, from: Vec3, path: &[Vec3]) -> bool {
        let mut a = from.xz();
        path.iter().all(|p| {
            let b = p.xz();
            let n = (a.distance(b) / 50.0).ceil() as usize;
            let ok = (1..n).all(|k| nav::passable(&self.hm, &self.bridges, a.lerp(b, k as f32 / n as f32)));
            a = b;
            ok
        })
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct GroundAssets {
    hull: Handle<Mesh>,
    turret: Handle<Mesh>,
    barrel: Handle<Mesh>,
    truck: Handle<Mesh>,
    launcher: Handle<Mesh>,
    torso: Handle<Mesh>,
    legs: Handle<Mesh>,
    head: Handle<Mesh>,
    rifle: Handle<Mesh>,
    nato: Handle<StandardMaterial>,
    enemy: Handle<StandardMaterial>,
    dark: Handle<StandardMaterial>,
}

pub struct GroundPlugin;

impl Plugin for GroundPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Battles>()
            .init_resource::<EnemyScript>()
            .init_resource::<IrView>()
            .add_systems(Startup, setup_ground_assets)
            .add_systems(OnEnter(AppState::Playing), spawn_units)
            .add_systems(
                Update,
                (
                    refresh_nav,
                    enemy_script,
                    replan_on_bridge_loss,
                    auto_orders,
                    move_units,
                    combat_tick,
                    unit_damage,
                    unit_status,
                    anti_air,
                    update_models,
                )
                    .chain()
                    .in_set(GameplaySet),
            )
            .add_systems(Update, ir_glow);
    }
}

fn setup_ground_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(GroundAssets {
        hull: meshes.add(Cuboid::new(3.6, 1.6, 7.0)),
        turret: meshes.add(Cuboid::new(2.6, 1.0, 3.0)),
        barrel: meshes.add(Cylinder::new(0.18, 5.0).mesh().resolution(6)),
        truck: meshes.add(Cuboid::new(2.6, 2.4, 6.0)),
        launcher: meshes.add(Cuboid::new(2.2, 1.4, 4.0)),
        torso: meshes.add(Cuboid::new(0.5, 0.65, 0.3)),
        legs: meshes.add(Cuboid::new(0.42, 0.85, 0.26)),
        head: meshes.add(Sphere::new(0.14).mesh().ico(1).unwrap()),
        rifle: meshes.add(Cuboid::new(0.07, 0.07, 0.9)),
        nato: materials.add(StandardMaterial {
            base_color: Color::srgb(0.30, 0.36, 0.22),
            perceptual_roughness: 0.9,
            ..default()
        }),
        enemy: materials.add(StandardMaterial {
            base_color: Color::srgb(0.52, 0.44, 0.30),
            perceptual_roughness: 0.9,
            ..default()
        }),
        dark: materials.add(Color::srgb(0.15, 0.15, 0.14)),
    });
}

fn unit_name(side: Side, kind: UnitKind, n: usize) -> String {
    let prefix = match side {
        Side::Nato => "NATO",
        Side::Enemy => "Enemy",
    };
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix} {prefix} {}", unit_stats(kind).name)
}

fn spawn_units(
    mut commands: Commands,
    hm: Res<Heightmap>,
    nav: Res<NavGrid>,
    assets: Res<GroundAssets>,
    mut script: ResMut<EnemyScript>,
    mut battles: ResMut<Battles>,
) {
    *script = EnemyScript::default();
    battles.0.clear();
    let mut rng = rand::thread_rng();
    let mut n = 0;
    let mut all: Vec<(Side, UnitKind, (f32, f32), bool)> = Vec::new();
    for (k, ll) in NATO_UNITS {
        all.push((Side::Nato, k, ll, false));
    }
    for (k, ll) in ENEMY_WAVE1 {
        all.push((Side::Enemy, k, ll, false));
    }
    for (k, ll) in ENEMY_WAVE2 {
        all.push((Side::Enemy, k, ll, true));
    }
    for (side, kind, ll, holding) in all {
        n += 1;
        // Never start in a river.
        let p = nav.nearest_land(Vec2::from(geo(ll.0, ll.1)));
        let pos = Vec3::new(p.x, hm.height(p.x, p.y), p.y);
        let heading = if side == Side::Nato { -std::f32::consts::FRAC_PI_2 } else { std::f32::consts::FRAC_PI_2 };
        let unit = commands
            .spawn((
                Name::new(unit_name(side, kind, n)),
                GroundUnit {
                    name: unit_name(side, kind, n),
                    side,
                    kind,
                    strength: 100.0,
                    path: Vec::new(),
                    retreating: false,
                    engaged: false,
                    aa_cooldown: 0.0,
                    holding,
                    heading,
                    order: Order::Auto,
                    // Stagger the first path searches.
                    replan: rng.gen_range(0.2..3.0),
                    auto_goal: None,
                },
                Transform::from_translation(pos),
                StateScoped(AppState::Playing),
            ))
            .id();
        spawn_models(&mut commands, &assets, unit, side, kind, pos);
    }
}

fn spawn_models(commands: &mut Commands, a: &GroundAssets, unit: Entity, side: Side, kind: UnitKind, pos: Vec3) {
    let mat = match side {
        Side::Nato => a.nato.clone(),
        Side::Enemy => a.enemy.clone(),
    };
    let count = unit_stats(kind).models;
    let mut rng = rand::thread_rng();
    for i in 0..count {
        // Loose column/wedge formation.
        let row = i / 2;
        let col = i % 2;
        let offset = Vec2::new(
            (col as f32 - 0.5) * UNIT_MODEL_SPREAD + rng.gen_range(-8.0..8.0),
            row as f32 * UNIT_MODEL_SPREAD * 0.9 + rng.gen_range(-8.0..8.0),
        );
        commands
            .spawn((
                UnitModel { unit, index: i, offset },
                HitSphere { radius: UNIT_MODEL_HIT_RADIUS, side: Some(side) },
                DamageRelay(unit),
                Transform::from_translation(pos).with_scale(Vec3::splat(UNIT_MODEL_SCALE)),
                Visibility::default(),
                StateScoped(AppState::Playing),
            ))
            .with_children(|p| match kind {
                UnitKind::Armor => {
                    p.spawn((Mesh3d(a.hull.clone()), MeshMaterial3d(mat.clone()), Transform::from_xyz(0.0, 0.8, 0.0)));
                    p.spawn((Mesh3d(a.turret.clone()), MeshMaterial3d(mat.clone()), Transform::from_xyz(0.0, 2.1, 0.3)));
                    p.spawn((
                        Mesh3d(a.barrel.clone()),
                        MeshMaterial3d(a.dark.clone()),
                        Transform::from_xyz(0.0, 2.1, -3.5).with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
                    ));
                }
                UnitKind::Infantry => {
                    // A fire team of five soldiers, true to scale.
                    let k = 1.0 / UNIT_MODEL_SCALE;
                    for (sx, sz) in [(0.0, 0.0), (-4.0, 3.0), (4.0, 2.5), (-2.5, -4.0), (3.0, -3.5)] {
                        let jx = sx + rng.gen_range(-1.0..1.0);
                        let jz = sz + rng.gen_range(-1.0..1.0);
                        p.spawn((
                            Transform::from_xyz(jx * k, 0.0, jz * k)
                                .with_scale(Vec3::splat(k))
                                .with_rotation(Quat::from_rotation_y(rng.gen_range(-0.5..0.5))),
                            Visibility::default(),
                        ))
                        .with_children(|s| {
                            s.spawn((Mesh3d(a.legs.clone()), MeshMaterial3d(a.dark.clone()), Transform::from_xyz(0.0, 0.43, 0.0)));
                            s.spawn((Mesh3d(a.torso.clone()), MeshMaterial3d(mat.clone()), Transform::from_xyz(0.0, 1.18, 0.0)));
                            s.spawn((Mesh3d(a.head.clone()), MeshMaterial3d(mat.clone()), Transform::from_xyz(0.0, 1.66, 0.0)));
                            s.spawn((Mesh3d(a.rifle.clone()), MeshMaterial3d(a.dark.clone()), Transform::from_xyz(0.22, 1.2, -0.3)));
                        });
                    }
                }
                UnitKind::AntiAir => {
                    p.spawn((Mesh3d(a.truck.clone()), MeshMaterial3d(mat.clone()), Transform::from_xyz(0.0, 1.2, 0.0)));
                    p.spawn((
                        Mesh3d(a.launcher.clone()),
                        MeshMaterial3d(a.dark.clone()),
                        Transform::from_xyz(0.0, 3.2, 0.5).with_rotation(Quat::from_rotation_x(0.6)),
                    ));
                }
            });
    }
}

fn ir_glow(ir: Res<IrView>, assets: Res<GroundAssets>, mut materials: ResMut<Assets<StandardMaterial>>) {
    if !ir.is_changed() {
        return;
    }
    if let Some(m) = materials.get_mut(&assets.enemy) {
        m.emissive = if ir.0 { LinearRgba::rgb(6.0, 5.5, 5.0) } else { LinearRgba::BLACK };
    }
}

fn refresh_nav(bridges: Res<Bridges>, mut nav: ResMut<NavGrid>) {
    nav.refresh_components(&bridges);
}

/// Wave 2 is released after ENEMY_WAVE2_TIME, if it can still find a way
/// to the NATO base (bridges standing).
fn enemy_script(
    time: Res<Time>,
    terrain: Terrain,
    mut script: ResMut<EnemyScript>,
    mut units: Query<(&mut GroundUnit, &Transform)>,
    mut log: ResMut<EventLog>,
) {
    let dt = time.delta_secs();
    script.elapsed += dt;
    script.wave2_retry -= dt;
    if script.wave2_done || script.elapsed < ENEMY_WAVE2_TIME || script.wave2_retry > 0.0 {
        return;
    }
    let target = base_center(&NATO_BASE);
    let reachable = units
        .iter()
        .find(|(u, _)| u.holding)
        .map(|(u, tf)| terrain.plan(tf.translation, target, u.kind).is_some());
    match reachable {
        None => script.wave2_done = true,
        Some(false) => {
            script.wave2_retry = 60.0;
            if !script.stall_logged {
                script.stall_logged = true;
                log.push("Enemy reinforcements stalled: every route west is cut");
            }
        }
        Some(true) => {
            script.wave2_done = true;
            log.push("Enemy reinforcements are moving west!");
            for (mut u, _) in &mut units {
                if u.holding {
                    u.holding = false;
                    u.order = Order::Auto;
                    u.replan = 0.0;
                }
            }
        }
    }
}

/// When a bridge falls, anyone whose route used it re-plans.
fn replan_on_bridge_loss(
    terrain: Terrain,
    mut script: ResMut<EnemyScript>,
    mut units: Query<(&mut GroundUnit, &Transform)>,
    mut log: ResMut<EventLog>,
) {
    if terrain.bridges.generation == script.bridge_generation {
        return;
    }
    script.bridge_generation = terrain.bridges.generation;
    for (mut u, tf) in &mut units {
        if u.path.is_empty() || terrain.path_ok(tf.translation, &u.path) {
            continue;
        }
        if terrain.bridges.deck_at(tf.translation.x, tf.translation.z).is_some() {
            continue; // already on a surviving deck; let them finish
        }
        let dest = u.destination().unwrap_or(tf.translation);
        match u.order {
            Order::Auto if !u.retreating => {
                u.path.clear();
                u.replan = 0.0;
                u.auto_goal = None;
            }
            _ => match terrain.plan(tf.translation, dest, u.kind) {
                Some(p) => u.path = p,
                None => {
                    u.path.clear();
                    if u.side == Side::Nato {
                        log.push(format!("{}: route cut, holding", u.name));
                    }
                }
            },
        }
    }
}

/// Auto units pick a target and a route (§6.2); finished player orders
/// hand control back after AUTO_RESUME seconds.
fn auto_orders(
    time: Res<Time>,
    terrain: Terrain,
    sites: Query<&Site>,
    mut q: Query<(Entity, &mut GroundUnit, &Transform)>,
) {
    let dt = time.delta_secs();
    let snapshot: Vec<(Entity, Side, UnitKind, Vec3, bool, String)> = q
        .iter()
        .map(|(e, u, tf)| (e, u.side, u.kind, tf.translation, u.retreating, u.name.clone()))
        .collect();
    let mut rng = rand::thread_rng();
    for (e, mut u, tf) in &mut q {
        if u.retreating || u.holding {
            continue;
        }
        match u.order {
            Order::Hold => continue,
            Order::Manual { resume } => {
                if u.path.is_empty() {
                    let resume = resume - dt;
                    u.order = if resume <= 0.0 { Order::Auto } else { Order::Manual { resume } };
                    u.replan = 0.0;
                }
                continue;
            }
            Order::Auto => {}
        }
        u.replan -= dt;
        if u.replan > 0.0 || u.engaged {
            continue;
        }
        u.replan = AUTO_REPLAN * rng.gen_range(0.8..1.2);
        let pos = tf.translation;
        let home = base_center(&u.side.base());

        // Candidate goals, best first.
        let mut goals: Vec<(Vec3, String)> = Vec::new();
        if u.kind == UnitKind::AntiAir {
            // Cover the nearest friendly combat unit from behind.
            let friend = snapshot
                .iter()
                .filter(|o| o.0 != e && o.1 == u.side && o.2 != UnitKind::AntiAir && !o.4)
                .min_by(|a, b| a.3.distance(pos).total_cmp(&b.3.distance(pos)));
            if let Some(f) = friend {
                let p = escort_point(f.3, home);
                if p.xz().distance(pos.xz()) < 400.0 {
                    continue;
                }
                goals.push((p, format!("covering {}", f.5)));
            }
        } else {
            let mut enemies: Vec<_> = snapshot.iter().filter(|o| o.1 != u.side && !o.4).collect();
            enemies.sort_by(|a, b| a.3.distance(pos).total_cmp(&b.3.distance(pos)));
            for o in enemies.iter().take(AUTO_CANDIDATES) {
                goals.push((o.3, format!("attacking {}", o.5)));
            }
            goals.push((base_center(&u.side.other().base()), "advancing on the enemy airbase".into()));
            let mut targets: Vec<&Site> = sites.iter().filter(|s| s.alive && s.side != u.side).collect();
            targets.sort_by(|a, b| a.aim.distance(pos).total_cmp(&b.aim.distance(pos)));
            for s in targets {
                goals.push((s.aim, format!("attacking {}", s.name)));
            }
        }
        for (goal, label) in goals {
            // Same target as before and still on the way: keep the route.
            if !u.path.is_empty() && u.auto_goal.as_ref().is_some_and(|(g, _)| g.xz().distance(goal.xz()) < AUTO_RETARGET_DIST) {
                break;
            }
            if let Some(path) = terrain.plan(pos, goal, u.kind) {
                u.path = path;
                u.auto_goal = Some((goal, label));
                break;
            }
        }
    }
}

fn move_units(time: Res<Time>, terrain: Terrain, mut q: Query<(&mut GroundUnit, &mut Transform)>) {
    let dt = time.delta_secs();
    let (hm, bridges) = (&terrain.hm, &terrain.bridges);
    for (mut u, mut tf) in &mut q {
        let pos = tf.translation;
        let Some(&wp) = u.path.first() else {
            continue;
        };
        if u.engaged && !u.retreating {
            continue;
        }
        let to = Vec2::new(wp.x - pos.x, wp.z - pos.z);
        let dist = to.length();
        // Bridge heads must be hit exactly, or the line across misses the deck.
        let near_bridge = bridges.list.iter().any(|b| {
            let (t, side) = b.project(wp.xz());
            (-0.2..=1.2).contains(&t) && side < 40.0
        });
        let arrive = if near_bridge { 4.0 } else { UNIT_ARRIVE_DIST };
        if dist < arrive {
            u.path.remove(0);
            continue;
        }
        let dir = to / dist;
        // Slower uphill; faster on roads; vehicles crawl through forest.
        let ahead = pos.xz() + dir * 20.0;
        let slope = (hm.height(ahead.x, ahead.y) - hm.height(pos.x, pos.z)) / 20.0;
        let factor = (1.0 - slope.max(0.0) * 4.0).clamp(0.35, 1.0)
            * nav::terrain_speed(&terrain.roads, &terrain.nav, bridges, u.kind, pos.xz());
        let step = (u.stats().speed * factor * dt).min(dist);
        // Can't drive into water, except across an intact bridge; anyone
        // already in the water (a deck fell under them) may wade out.
        let stranded = !nav::passable(hm, bridges, pos.xz());
        let mut moved = None;
        for angle in [0.0f32, 0.5, -0.5, 1.0, -1.0, 1.5, -1.5] {
            let d = Vec2::from_angle(angle).rotate(dir);
            let next = pos.xz() + d * step;
            if stranded || nav::passable(hm, bridges, next) {
                moved = Some((next, d));
                break;
            }
        }
        let Some((next, d)) = moved else {
            continue;
        };
        tf.translation = Vec3::new(next.x, bridges.ground(hm, next.x, next.y), next.y);
        let target_heading = (-d.x).atan2(-d.y);
        let diff = (target_heading - u.heading + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        u.heading += diff.clamp(-dt, dt);
    }
}

fn combat_tick(
    time: Res<Time>,
    nav: Res<NavGrid>,
    mut script: ResMut<EnemyScript>,
    mut battles: ResMut<Battles>,
    mut q: Query<(Entity, &mut GroundUnit, &Transform)>,
    mut detonations: EventWriter<Detonation>,
) {
    script.combat_timer -= time.delta_secs();
    if script.combat_timer > 0.0 {
        return;
    }
    script.combat_timer = COMBAT_TICK;
    let snapshot: Vec<(Entity, Side, Vec3, UnitStats, f32, bool, UnitKind)> = q
        .iter()
        .map(|(e, u, tf)| (e, u.side, tf.translation, u.stats(), u.strength, u.retreating, u.kind))
        .collect();
    let mut rng = rand::thread_rng();
    let mut damage: Vec<(Entity, f32)> = Vec::new();
    let mut engaged: Vec<Entity> = Vec::new();
    battles.0.clear();
    for &(e, side, pos, stats, strength, retreating, _) in &snapshot {
        // Nearest enemy in range.
        let nearest = snapshot
            .iter()
            .filter(|o| o.1 != side)
            .map(|o| (o, o.2.distance(pos)))
            .filter(|(o, d)| *d < stats.range.max(o.3.range))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let Some((o, d)) = nearest else {
            continue;
        };
        engaged.push(e);
        if !retreating && d < stats.range {
            let mut dmg = combat_damage(stats.attack, strength, o.3.defense, rng.gen_range(0.8..1.2));
            // Infantry dug into forest is hard to dig out.
            if o.6 == UnitKind::Infantry && nav.forest_at(o.2.xz()) {
                dmg *= FOREST_INFANTRY_DAMAGE;
            }
            damage.push((o.0, dmg));
            // Some visible gunfire at the receiving end.
            if rng.gen_bool(0.35) {
                let jitter = Vec3::new(rng.gen_range(-60.0..60.0), 0.0, rng.gen_range(-60.0..60.0));
                detonations.send(Detonation::fx(o.2 + jitter + Vec3::Y * 3.0, Boom::Spark));
            }
            let mid = (pos + o.2) * 0.5;
            if !battles.0.iter().any(|b| b.distance(mid) < 1_000.0) {
                battles.0.push(mid);
            }
        }
    }
    for (e, mut u, _) in &mut q {
        u.engaged = engaged.contains(&e);
        for (t, dmg) in &damage {
            if *t == e {
                u.strength -= dmg;
            }
        }
    }
}

/// Strafing and explosions (§6.2): weapons damage arrives as DamageEvents
/// relayed from the unit's models.
fn unit_damage(mut events: EventReader<DamageEvent>, mut q: Query<&mut GroundUnit>) {
    for ev in events.read() {
        let Ok(mut u) = q.get_mut(ev.target) else {
            continue;
        };
        let factor = match ev.kind {
            DamageKind::Bullet => GROUND_BULLET_FACTOR,
            DamageKind::Blast | DamageKind::Crash => GROUND_BLAST_FACTOR,
        };
        u.strength -= ev.amount * factor;
    }
}

/// Destroyed at 0, retreats below RETREAT_STRENGTH.
#[allow(clippy::too_many_arguments)]
fn unit_status(
    mut commands: Commands,
    mut q: Query<(Entity, &mut GroundUnit, &Transform)>,
    models: Query<(Entity, &UnitModel)>,
    mut detonations: EventWriter<Detonation>,
    mut log: ResMut<EventLog>,
    mut stats: ResMut<RoundStats>,
    terrain: Terrain,
) {
    for (e, mut u, tf) in &mut q {
        if u.strength <= 0.0 {
            log.push(format!("{} destroyed", u.name));
            if u.side == Side::Enemy {
                stats.enemy_units_destroyed += 1;
            } else {
                stats.nato_units_lost += 1;
            }
            detonations.send(Detonation::fx(tf.translation + Vec3::Y * 3.0, Boom::Medium));
            for (m, model) in &models {
                if model.unit == e {
                    commands.entity(m).despawn_recursive();
                }
            }
            commands.entity(e).despawn_recursive();
            continue;
        }
        if u.strength < RETREAT_STRENGTH && !u.retreating {
            u.retreating = true;
            u.holding = false;
            let home = base_center(&u.side.base());
            u.path = terrain.plan(tf.translation, home, u.kind).unwrap_or_default();
            log.push(match u.side {
                Side::Enemy => format!("{} routed!", u.name),
                Side::Nato => format!("{} is falling back", u.name),
            });
        }
    }
}

/// AA units fire real heat-seekers at enemy jets in range (§6.2).
fn anti_air(
    mut commands: Commands,
    time: Res<Time>,
    weapons: Res<WeaponAssets>,
    infra: Res<crate::infrastructure::Infra>,
    mut units: Query<(&mut GroundUnit, &Transform)>,
    jets: Query<(Entity, &Jet, &Transform, &Velocity)>,
    mut sounds: EventWriter<crate::audio::SoundEvent>,
) {
    let dt = time.delta_secs();
    let mut rng = rand::thread_rng();
    for (mut u, tf) in &mut units {
        if u.kind != UnitKind::AntiAir {
            continue;
        }
        u.aa_cooldown -= dt;
        if u.aa_cooldown > 0.0 {
            continue;
        }
        let pos = tf.translation + Vec3::Y * 4.0;
        let target = jets
            .iter()
            .filter(|(_, j, jtf, _)| {
                j.side != u.side && jtf.translation.distance(pos) < AA_RANGE && jtf.translation.y - pos.y < AA_MAX_ALT
            })
            .min_by(|a, b| a.2.translation.distance(pos).total_cmp(&b.2.translation.distance(pos)));
        let Some((je, _, jtf, _)) = target else {
            continue;
        };
        // Roll once per second.
        u.aa_cooldown = 1.0;
        // Without radar (§6.4) the launchers have to find targets alone.
        let radar = if infra.get(u.side).radar_up() { 1.0 } else { RADAR_OUT_AA_FACTOR };
        if !rng.gen_bool(AA_CHANCE_PER_S * (u.strength / 100.0) as f64 * radar) {
            continue;
        }
        u.aa_cooldown = AA_COOLDOWN;
        let dir = ((jtf.translation - pos).normalize_or_zero() + Vec3::Y * 0.4).normalize();
        spawn_missile(&mut commands, &weapons, pos, dir * 120.0, u.side, false, Some(je));
        sounds.send(crate::audio::SoundEvent::at(crate::audio::Sfx::MissileLaunch, pos));
    }
}

fn update_models(
    hm: Res<Heightmap>,
    bridges: Res<Bridges>,
    units: Query<(&GroundUnit, &Transform), Without<UnitModel>>,
    mut models: Query<(&UnitModel, &mut Transform, &mut Visibility, &mut HitSphere)>,
) {
    for (m, mut tf, mut vis, mut sphere) in &mut models {
        let Ok((u, utf)) = units.get(m.unit) else {
            continue;
        };
        let alive = ((u.strength / 100.0) * u.stats().models as f32).ceil() as usize;
        let show = m.index < alive;
        *vis = if show { Visibility::Inherited } else { Visibility::Hidden };
        sphere.radius = if show { UNIT_MODEL_HIT_RADIUS } else { 0.0 };
        let rot = Quat::from_rotation_y(u.heading);
        // Single file while crossing a bridge.
        let squeeze = if bridges.deck_at(utf.translation.x, utf.translation.z).is_some() { 0.05 } else { 1.0 };
        let off = rot * Vec3::new(m.offset.x * squeeze, 0.0, m.offset.y);
        let p = utf.translation + off;
        tf.translation = Vec3::new(p.x, bridges.ground(&hm, p.x, p.z), p.z);
        tf.rotation = rot;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combat_formula() {
        // Full-strength armor vs infantry, average roll.
        let d = combat_damage(10.0, 100.0, 1.0, 1.0);
        assert!((d - 10.0 * COMBAT_SCALE).abs() < 1e-5);
        // Half strength does half damage; double defense halves it again.
        assert!((combat_damage(10.0, 50.0, 2.0, 1.0) - d / 4.0).abs() < 1e-5);
        // Random roll bounds.
        assert!(combat_damage(10.0, 100.0, 1.0, 1.2) > combat_damage(10.0, 100.0, 1.0, 0.8));
    }

    #[test]
    fn escorts_stay_behind() {
        let friend = Vec3::new(1_000.0, 0.0, 0.0);
        let home = Vec3::new(-10_000.0, 50.0, 0.0);
        let p = escort_point(friend, home);
        assert!((p.x - (1_000.0 - AA_ESCORT_OFFSET)).abs() < 1e-3 && p.z.abs() < 1e-3);
    }
}
