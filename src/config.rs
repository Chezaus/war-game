//! ALL hardcoded numbers live here: world layout, jet stats, weapons,
//! ground units, starting positions, rules.
//!
//! Conventions: meters, seconds, kg, radians. +Y up, -Z north, +X east.
//! This file is also included by `examples/gen_heightmap.rs`, so keep it
//! free of Bevy imports.

#![allow(dead_code)]

pub use crate::geo::{geo, ENEMY_AIRBASE, NATO_AIRBASE};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Nato,
    Enemy,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::Nato => Side::Enemy,
            Side::Enemy => Side::Nato,
        }
    }
    pub fn base(self) -> AirbaseDef {
        match self {
            Side::Nato => NATO_BASE,
            Side::Enemy => ENEMY_BASE,
        }
    }
}

// ---------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------

/// The map is WORLD_SIZE x WORLD_SIZE, centred on the origin: a mini Earth
/// with Europe magnified (see geo.rs).
pub const WORLD_SIZE: f32 = 128_000.0;
pub const WORLD_HALF: f32 = WORLD_SIZE / 2.0;
/// Heightmap is HEIGHTMAP_RES x HEIGHTMAP_RES samples (vertices), 125 m apart.
pub const HEIGHTMAP_RES: usize = 1025;
pub const HEIGHTMAP_PATH: &str = "assets/heightmap.png";
/// 16-bit pixel value 0 maps to HEIGHT_MIN, 65535 to HEIGHT_MAX.
pub const HEIGHT_MIN: f32 = -100.0;
pub const HEIGHT_MAX: f32 = 2_600.0;
pub const SEA_LEVEL: f32 = 0.0;
pub const TERRAIN_CHUNKS: usize = 16;

pub const FOG_VISIBILITY: f32 = 50_000.0;
pub const FOG_VISIBILITY_MAP: f32 = 600_000.0;
pub const SKY_COLOR: [f32; 3] = [0.55, 0.70, 0.88];

/// Rivers get a gentle meander on top of their authored course (m).
pub const RIVER_MEANDER: f32 = 160.0;
/// River valleys: open water, then banks sloping up over this distance plus
/// a bit more for high ground.
pub const RIVER_VALLEY_BASE: f32 = 350.0;
pub const RIVER_VALLEY_PER_M: f32 = 5.0;
pub const RIVER_VALLEY_MAX: f32 = 3_000.0;

#[derive(Clone, Copy, Debug)]
pub struct AirbaseDef {
    /// Runway centre (x, z).
    pub center: [f32; 2],
    /// Runway elevation (terrain is flattened to this).
    pub elevation: f32,
    /// Runways run north-south. Takeoff is towards -Z (north).
    pub runway_length: f32,
    pub runway_width: f32,
    /// Radius of the "base zone" used for capture.
    pub zone_radius: f32,
}

/// Central Germany.
pub const NATO_BASE: AirbaseDef = AirbaseDef {
    center: geo(NATO_AIRBASE.0, NATO_AIRBASE.1),
    elevation: 60.0,
    runway_length: 2_600.0,
    runway_width: 50.0,
    zone_radius: 2_200.0,
};

/// East of Moscow.
pub const ENEMY_BASE: AirbaseDef = AirbaseDef {
    center: geo(ENEMY_AIRBASE.0, ENEMY_AIRBASE.1),
    elevation: 80.0,
    runway_length: 2_600.0,
    runway_width: 50.0,
    zone_radius: 2_200.0,
};

/// Flattened area around each runway (half sizes) plus blend margin.
pub const RUNWAY_FLAT_HALF_X: f32 = 450.0;
pub const RUNWAY_FLAT_HALF_Z: f32 = 1_700.0;
pub const RUNWAY_FLAT_BLEND: f32 = 900.0;
/// Also keep the whole base zone fairly gentle.
pub const BASE_GENTLE_RADIUS: f32 = 3_000.0;

/// Where the fighting starts: between Warsaw and Minsk.
pub const FRONT_CENTER: [f32; 2] = geo(25.0, 53.0);

// Roads (§4): resampled every ROAD_SAMPLE m with a gentle meander.
pub const ROAD_SAMPLE: f32 = 20.0;
pub const ROAD_MEANDER: f32 = 120.0;
pub const ROAD_WIDTH: f32 = 14.0;
/// Within this distance of a road centre line a unit counts as on the road.
pub const ROAD_SNAP: f32 = 45.0;
/// Units on roads and bridges move this much faster.
pub const ROAD_SPEED_FACTOR: f32 = 1.8;

// Bridges: wherever a road crosses water no wider than this.
pub const BRIDGE_MAX_LENGTH: f32 = 2_000.0;
/// Deck rises this far above the straight line between the banks, mid-span.
pub const BRIDGE_ARCH: f32 = 12.0;
pub const BRIDGE_WIDTH: f32 = 16.0;
pub const BRIDGE_SEGMENT: f32 = 110.0;
pub const BRIDGE_HP: f32 = 260.0;
/// Bullets do little to concrete.
pub const BRIDGE_BULLET_FACTOR: f32 = 0.25;

// Forests (§6.2, §6.5)
/// geo::forest_density above this counts as forest.
pub const FOREST_THRESHOLD: f32 = 0.5;
/// Trees are modelled inside this lon/lat box (the theatre); elsewhere the
/// forest only shows as darker ground.
pub const TREE_REGION: [f32; 4] = [-11.0, 35.0, 45.0, 66.0];
pub const TREE_SPACING: f32 = 55.0;
/// Trees beyond this distance from the camera are hidden.
pub const TREE_VIEW_DIST: f32 = 14_000.0;
/// Vehicles crawl through forest; infantry barely slows.
pub const FOREST_VEHICLE_SPEED: f32 = 0.55;
pub const FOREST_INFANTRY_SPEED: f32 = 0.85;
/// Infantry units in forest take this fraction of combat damage.
pub const FOREST_INFANTRY_DAMAGE: f32 = 0.7;
/// On foot in forest: enemy hit chance and drone spotting range factors.
pub const FOREST_HIT_FACTOR: f64 = 0.5;
pub const FOREST_SPOT_FACTOR: f32 = 0.4;

// Ground navigation grid for path finding.
pub const NAV_CELL: f32 = 250.0;

// ---------------------------------------------------------------------------
// Physics
// ---------------------------------------------------------------------------

pub const GRAVITY: f32 = 9.81;
pub const RHO0: f32 = 1.225;
pub const ATMOS_SCALE_HEIGHT: f32 = 8_500.0;
pub const SPEED_OF_SOUND: f32 = 340.0;
pub const FIXED_HZ: f64 = 60.0;

// ---------------------------------------------------------------------------
// Jets (§5.1)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct JetStats {
    pub name: &'static str,
    pub empty_mass: f32,
    pub max_fuel: f32,
    pub wing_area: f32,
    pub thrust_dry: f32,
    pub thrust_ab: f32,
    /// Lift slope per radian.
    pub cl_alpha: f32,
    pub stall_aoa: f32,
    pub cd0: f32,
    pub induced_k: f32,
    pub max_g: f32,
    pub min_g: f32,
    pub max_pitch_rate: f32,
    pub max_roll_rate: f32,
    pub max_yaw_rate: f32,
    /// Speed at which control authority reaches 100%.
    pub full_authority_speed: f32,
    pub fuel_flow_idle: f32,
    pub fuel_flow_dry: f32,
    pub fuel_flow_ab: f32,
    pub hp: f32,
    pub gun_ammo: u32,
    pub missiles: u32,
    pub flares: u32,
    pub hit_radius: f32,
}

pub const NATO_JET: JetStats = JetStats {
    name: "F-35 (NATO)",
    empty_mass: 13_300.0,
    max_fuel: 3_200.0,
    wing_area: 42.7,
    thrust_dry: 125_000.0,
    thrust_ab: 191_000.0,
    cl_alpha: 4.0,
    stall_aoa: 0.33,
    cd0: 0.032,
    induced_k: 0.12,
    max_g: 9.0,
    min_g: -3.0,
    max_pitch_rate: 1.1,
    max_roll_rate: 3.6,
    max_yaw_rate: 0.35,
    full_authority_speed: 150.0,
    fuel_flow_idle: 0.12,
    fuel_flow_dry: 1.4,
    fuel_flow_ab: 5.5,
    hp: 100.0,
    gun_ammo: 400,
    missiles: 4,
    flares: 16,
    hit_radius: 9.0,
};

pub const ENEMY_JET: JetStats = JetStats {
    name: "Su-35 (Enemy)",
    empty_mass: 18_400.0,
    max_fuel: 4_500.0,
    wing_area: 62.0,
    thrust_dry: 165_000.0,
    thrust_ab: 270_000.0,
    cl_alpha: 3.9,
    stall_aoa: 0.35,
    cd0: 0.034,
    induced_k: 0.13,
    max_g: 9.0,
    min_g: -3.0,
    max_pitch_rate: 1.0,
    max_roll_rate: 3.2,
    max_yaw_rate: 0.35,
    full_authority_speed: 155.0,
    fuel_flow_idle: 0.15,
    fuel_flow_dry: 1.8,
    fuel_flow_ab: 7.0,
    hp: 110.0,
    gun_ammo: 300,
    missiles: 4,
    flares: 12,
    hit_radius: 10.0,
};

/// Throttle above this is afterburner.
pub const AFTERBURNER_THRESHOLD: f32 = 0.9;
pub const THROTTLE_RATE: f32 = 0.5;
/// Thrust scales with (rho/rho0)^THRUST_LAPSE.
pub const THRUST_LAPSE: f32 = 0.7;
/// Extra drag coefficients.
pub const CD_GEAR: f32 = 0.025;
pub const CD_AIRBRAKE: f32 = 0.06;
/// Transonic drag rise added between Mach 0.9 and 1.1.
pub const CD_WAVE: f32 = 0.035;
pub const SIDE_FORCE_COEF: f32 = 1.2;
/// Lift keeps this fraction after the stall (reached STALL_DROP rad past it).
pub const POST_STALL_LIFT: f32 = 0.35;
pub const STALL_DROP: f32 = 0.3;
/// How quickly actual rotation rates follow the commanded ones (1/s).
pub const RATE_RESPONSE: f32 = 7.0;
/// Weathervane: nose follows the velocity vector when stalled / slipping.
pub const WEATHERVANE_PITCH: f32 = 1.4;
pub const WEATHERVANE_YAW: f32 = 1.2;
/// Keyboard stick ramp speed (per second).
pub const STICK_RAMP: f32 = 4.0;

// Ground handling
pub const GEAR_HEIGHT: f32 = 2.2;
pub const ROLLING_FRICTION: f32 = 0.02;
pub const BRAKE_FRICTION: f32 = 0.45;
pub const NOSEWHEEL_STEER_RATE: f32 = 0.5;
pub const MAX_GROUND_PITCH: f32 = 0.26;
pub const CRASH_SINK_RATE: f32 = 7.0;
pub const CRASH_BANK: f32 = 0.35;
pub const CRASH_MAX_GROUND_SPEED: f32 = 140.0;
pub const GEAR_MAX_SPEED: f32 = 150.0;

// G tolerance (§5.1)
pub const G_TOL_ONSET: f32 = 7.0;
pub const G_TOL_FILL: f32 = 0.27;
pub const G_TOL_RECOVER: f32 = 0.22;
pub const REDOUT_ONSET: f32 = -2.5;
pub const REDOUT_FILL: f32 = 0.45;
/// Control fades between these blackout-meter values.
pub const BLACKOUT_FADE_START: f32 = 0.65;

// Fuel leak
pub const LEAK_RATE_MIN: f32 = 2.5;
pub const LEAK_RATE_MAX: f32 = 7.0;
pub const DEBUG_LEAK_RATE: f32 = 5.0;

// Damage (§5.2)
pub const ENGINE_DAMAGE_THRUST: f32 = 0.55;
pub const CHANCE_ENGINE_HIT: f64 = 0.10;
pub const CHANCE_TANK_HIT: f64 = 0.14;
pub const CHANCE_FIRE: f64 = 0.06;
pub const FIRE_DPS: f32 = 1.6;
pub const FIRE_OUT_CHANCE_PER_S: f64 = 0.03;

// ---------------------------------------------------------------------------
// Weapons (§5.3)
// ---------------------------------------------------------------------------

pub const GUN_RATE: f32 = 25.0;
pub const GUN_MUZZLE_SPEED: f32 = 1_000.0;
pub const GUN_SPREAD: f32 = 0.004;
pub const BULLET_LIFE: f32 = 2.5;
pub const BULLET_DAMAGE: f32 = 8.0;
pub const MAX_BULLETS: usize = 500;

pub const MISSILE_LOCK_CONE: f32 = 0.22;
pub const MISSILE_LOCK_RANGE: f32 = 6_000.0;
pub const MISSILE_LOCK_TIME: f32 = 1.0;
pub const MISSILE_SEEKER_GIMBAL: f32 = 0.85;
pub const MISSILE_BOOST_ACCEL: f32 = 260.0;
pub const MISSILE_BURN_TIME: f32 = 2.6;
pub const MISSILE_DRAG_K: f32 = 0.00006;
pub const MISSILE_NAV_GAIN: f32 = 4.0;
pub const MISSILE_MAX_G: f32 = 32.0;
pub const MISSILE_LIFE: f32 = 18.0;
pub const MISSILE_ARM_TIME: f32 = 0.4;
pub const MISSILE_PROX_RADIUS: f32 = 16.0;
pub const MISSILE_DAMAGE: f32 = 75.0;
pub const MISSILE_BLAST_RADIUS: f32 = 35.0;
pub const MISSILE_COOLDOWN: f32 = 1.0;
pub const MAX_MISSILES: usize = 30;

pub const FLARE_DECOY_CHANCE: f64 = 0.35;
pub const FLARE_LIFE: f32 = 3.5;
pub const FLARE_COOLDOWN: f32 = 0.25;

pub const JET_CRASH_DAMAGE: f32 = 40.0;
pub const JET_CRASH_RADIUS: f32 = 60.0;

/// Damage multipliers when the target is a ground unit (strength points).
pub const GROUND_BULLET_FACTOR: f32 = 0.18;
pub const GROUND_BLAST_FACTOR: f32 = 0.35;

// ---------------------------------------------------------------------------
// Airbase restock (§5.4)
// ---------------------------------------------------------------------------

pub const RESTOCK_WAIT: f32 = 5.0;
pub const RESTOCK_DURATION: f32 = 10.0;
pub const RESTOCK_MAX_SPEED: f32 = 1.0;

// ---------------------------------------------------------------------------
// AI pilots (§5.5)
// ---------------------------------------------------------------------------

pub const AI_CRUISE_ALT: f32 = 1_600.0;
pub const AI_MIN_AGL: f32 = 220.0;
pub const AI_DETECT_RANGE: f32 = 12_000.0;
pub const AI_GUN_RANGE: f32 = 900.0;
pub const AI_GUN_CONE: f32 = 0.05;
pub const AI_MISSILE_INTERVAL: f32 = 9.0;
pub const AI_FLARE_INTERVAL: f32 = 0.7;
pub const AI_EVADE_RANGE: f32 = 4_500.0;
pub const AI_RTB_FUEL_FRAC: f32 = 0.18;
pub const AI_RTB_HP_FRAC: f32 = 0.3;
pub const AI_DESPAWN_ALT: f32 = 300.0;
/// Reaction delay: AI only re-picks targets this often.
pub const AI_THINK_INTERVAL: f32 = 0.5;
/// Aim error on gun shots, radians.
pub const AI_GUN_ERROR: f32 = 0.012;

pub const ENEMY_AIRBORNE_MIN: u32 = 2;
pub const ENEMY_AIRBORNE_MAX: u32 = 4;
pub const ENEMY_JET_POOL: u32 = 14;
pub const ENEMY_SPAWN_COOLDOWN: f32 = 35.0;
pub const ENEMY_FIRST_SPAWN_DELAY: f32 = 25.0;
pub const FRIENDLY_FLIGHT_SIZE: u32 = 2;
pub const FRIENDLY_RESPAWN_TIME: f32 = 90.0;
pub const FRIENDLY_POOL: u32 = 4;
pub const AI_SPAWN_ALT: f32 = 1_200.0;
pub const AI_SPAWN_SPEED: f32 = 200.0;

// ---------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------

pub const JET_RESERVE: u32 = 4;
pub const DEATH_CAM_TIME: f32 = 3.0;
pub const CHASE_DISTANCE: f32 = 32.0;
pub const CHASE_HEIGHT: f32 = 7.0;
pub const MOUSE_LOOK_SENS: f32 = 0.0025;

// ---------------------------------------------------------------------------
// Ground units (§6.2)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnitKind {
    Infantry,
    Armor,
    AntiAir,
}

#[derive(Clone, Copy, Debug)]
pub struct UnitStats {
    pub name: &'static str,
    pub attack: f32,
    pub defense: f32,
    pub speed: f32,
    pub range: f32,
    pub models: usize,
}

pub fn unit_stats(kind: UnitKind) -> UnitStats {
    match kind {
        UnitKind::Infantry => UnitStats {
            name: "Infantry",
            attack: 7.0,
            defense: 1.1,
            speed: 9.0,
            range: 2_000.0,
            models: 6,
        },
        UnitKind::Armor => UnitStats {
            name: "Armor",
            attack: 10.0,
            defense: 1.5,
            speed: 18.0,
            range: 2_200.0,
            models: 5,
        },
        UnitKind::AntiAir => UnitStats {
            name: "Anti-Air",
            attack: 3.0,
            defense: 0.8,
            speed: 15.0,
            range: 1_800.0,
            models: 4,
        },
    }
}

/// Scales the §6.2 damage formula so battles take a minute or two.
pub const COMBAT_SCALE: f32 = 0.16;
pub const COMBAT_TICK: f32 = 1.0;
pub const RETREAT_STRENGTH: f32 = 25.0;
pub const UNIT_ARRIVE_DIST: f32 = 80.0;
pub const UNIT_MODEL_SPREAD: f32 = 45.0;
pub const UNIT_MODEL_SCALE: f32 = 1.6;
pub const UNIT_MODEL_HIT_RADIUS: f32 = 9.0;

pub const AA_RANGE: f32 = 5_000.0;
pub const AA_MAX_ALT: f32 = 6_000.0;
pub const AA_CHANCE_PER_S: f64 = 0.12;
pub const AA_COOLDOWN: f32 = 7.0;

/// Starting units: (kind, (longitude, latitude)). NATO is deployed in
/// Poland, enemy wave 1 in Belarus and northern Ukraine.
pub const NATO_UNITS: [(UnitKind, (f32, f32)); 6] = [
    (UnitKind::Armor, (19.6, 52.4)),
    (UnitKind::Armor, (20.6, 50.6)),
    (UnitKind::Infantry, (21.4, 52.1)),
    (UnitKind::Infantry, (18.8, 53.4)),
    (UnitKind::AntiAir, (19.0, 51.6)),
    (UnitKind::AntiAir, (11.4, 50.8)),
];

/// Wave 1 advances at once.
pub const ENEMY_WAVE1: [(UnitKind, (f32, f32)); 5] = [
    (UnitKind::Armor, (27.2, 53.6)),
    (UnitKind::Armor, (28.4, 51.9)),
    (UnitKind::Infantry, (27.0, 54.6)),
    (UnitKind::Infantry, (29.2, 50.9)),
    (UnitKind::AntiAir, (28.0, 53.0)),
];

/// Wave 2 waits near the enemy base and advances after ENEMY_WAVE2_TIME,
/// but only if it can still find a way west (bridges standing).
pub const ENEMY_WAVE2: [(UnitKind, (f32, f32)); 3] = [
    (UnitKind::Armor, (35.6, 55.0)),
    (UnitKind::Infantry, (36.4, 56.1)),
    (UnitKind::AntiAir, (36.8, 55.3)),
];
pub const ENEMY_WAVE2_TIME: f32 = 300.0;

/// Idle units pick a target and advance on their own (§6.2). They re-think
/// this often (seconds, jittered).
pub const AUTO_REPLAN: f32 = 12.0;
/// After finishing a player order a unit holds this long, then goes back
/// to auto.
pub const AUTO_RESUME: f32 = 30.0;
/// Auto targets that move less than this keep the old path.
pub const AUTO_RETARGET_DIST: f32 = 900.0;
/// Anti-air escorts stay this far behind the unit they cover.
pub const AA_ESCORT_OFFSET: f32 = 700.0;
/// Auto units only consider the nearest few enemies (path finding cost).
pub const AUTO_CANDIDATES: usize = 3;

// ---------------------------------------------------------------------------
// Infrastructure (§6.4)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SiteKind {
    /// Feeds the radar and the fuel depot's pumps.
    PowerPlant,
    /// Down (or unpowered): slower restock / enemy jet launches.
    FuelDepot,
    /// Down (or unpowered): weaker AA, shorter AI detection, NATO map
    /// loses distant enemy jets.
    Radar,
}

#[derive(Clone, Copy, Debug)]
pub struct SiteStats {
    pub name: &'static str,
    pub hp: f32,
    pub hit_radius: f32,
}

pub fn site_stats(kind: SiteKind) -> SiteStats {
    match kind {
        SiteKind::PowerPlant => SiteStats { name: "Power Plant", hp: 400.0, hit_radius: 45.0 },
        SiteKind::FuelDepot => SiteStats { name: "Fuel Depot", hp: 220.0, hit_radius: 35.0 },
        SiteKind::Radar => SiteStats { name: "Radar", hp: 160.0, hit_radius: 22.0 },
    }
}

/// Sites: (kind, (longitude, latitude)).
pub const NATO_SITES: [(SiteKind, (f32, f32)); 3] = [
    (SiteKind::PowerPlant, (12.1, 50.7)),
    (SiteKind::FuelDepot, (9.3, 51.2)),
    (SiteKind::Radar, (11.3, 49.8)),
];
pub const ENEMY_SITES: [(SiteKind, (f32, f32)); 3] = [
    (SiteKind::PowerPlant, (35.3, 54.3)),
    (SiteKind::FuelDepot, (39.6, 56.4)),
    (SiteKind::Radar, (37.2, 57.0)),
];

/// Bullets do little to buildings.
pub const SITE_BULLET_FACTOR: f32 = 0.35;
/// Ground units with no enemy unit in range shell enemy sites in range:
/// combat_damage(...) × this, per combat tick.
pub const SITE_GROUND_FACTOR: f32 = 4.0;
/// AA fire chance while the side's radar is dark.
pub const RADAR_OUT_AA_FACTOR: f64 = 0.4;
/// AI jets' detection range while their side's radar is dark.
pub const RADAR_OUT_DETECT_FACTOR: f32 = 0.5;
/// NATO radar dark: the map only shows enemy jets this close to your forces.
pub const RADAR_OUT_MAP_RANGE: f32 = 8_000.0;
/// Fuel depot dark: NATO restock takes this much longer, enemy jets
/// launch this much less often.
pub const DEPOT_OUT_RESTOCK_FACTOR: f32 = 2.5;
pub const DEPOT_OUT_SPAWN_FACTOR: f32 = 2.5;

/// Enemy strike raids on NATO sites.
pub const ENEMY_STRIKE_FIRST: f32 = 150.0;
pub const ENEMY_STRIKE_INTERVAL: f32 = 140.0;
/// Run-in height above the site; the dive starts inside STRIKE_DIVE_DIST.
pub const STRIKE_RUN_IN_AGL: f32 = 700.0;
pub const STRIKE_DIVE_DIST: f32 = 3_800.0;
pub const STRIKE_FIRE_DIST: f32 = 3_000.0;
pub const STRIKE_BREAK_DIST: f32 = 900.0;
pub const STRIKE_SHOTS: u32 = 2;
/// Strike missiles are guided: the nose only has to be roughly on target.
pub const STRIKE_FIRE_CONE: f32 = 0.3;

// ---------------------------------------------------------------------------
// Infantry mode (§6.5)
// ---------------------------------------------------------------------------

pub const SOLDIER_HP: f32 = 100.0;
pub const SOLDIER_WALK: f32 = 4.0;
pub const SOLDIER_SPRINT: f32 = 7.0;
pub const SOLDIER_CROUCH_SPEED: f32 = 1.8;
pub const SOLDIER_EYE: f32 = 1.65;
pub const SOLDIER_EYE_CROUCHED: f32 = 0.95;
pub const SOLDIER_HIT_RADIUS: f32 = 0.9;
/// Your squad loses this much strength when you are killed.
pub const SOLDIER_LOSS_STRENGTH: f32 = 3.0;
/// Weapons damage multipliers against the soldier.
pub const SOLDIER_BULLET_FACTOR: f32 = 5.0;
pub const SOLDIER_BLAST_FACTOR: f32 = 1.2;

pub const RIFLE_RATE: f32 = 10.0;
pub const RIFLE_MAG: u32 = 30;
pub const RIFLE_RESERVE: u32 = 210;
pub const RIFLE_RELOAD: f32 = 2.2;
pub const RIFLE_MUZZLE_SPEED: f32 = 850.0;
pub const RIFLE_SPREAD: f32 = 0.006;
pub const RIFLE_SPREAD_CROUCHED: f32 = 0.003;
/// Per bullet; ground units take GROUND_BULLET_FACTOR of this.
pub const RIFLE_DAMAGE: f32 = 2.0;
pub const AIM_FOV_DEG: f32 = 35.0;
pub const DEFAULT_FOV_DEG: f32 = 70.0;

/// Enemy units shoot at the soldier once per second if they can see him.
pub const SOLDIER_HIT_CHANCE: f64 = 0.10;
pub const CROUCH_HIT_FACTOR: f64 = 0.35;
pub const SPOTTED_HIT_FACTOR: f64 = 2.5;
pub const ENEMY_RIFLE_DAMAGE: f32 = 12.0;
pub const ENEMY_SHELL_DAMAGE: f32 = 40.0;

/// Your IR drone (T).
pub const DRONE_BATTERY: f32 = 60.0;
pub const DRONE_COOLDOWN: f32 = 40.0;
pub const DRONE_SPEED: f32 = 28.0;
pub const DRONE_START_AGL: f32 = 80.0;
pub const DRONE_MIN_AGL: f32 = 25.0;
pub const DRONE_MAX_AGL: f32 = 260.0;
pub const DRONE_MAX_RANGE: f32 = 2_500.0;
pub const DRONE_BOMBS: u32 = 2;
pub const DRONE_BOMB_DAMAGE: f32 = 45.0;
pub const DRONE_BOMB_RADIUS: f32 = 28.0;

/// Enemy recon drones hunting the soldier.
pub const ENEMY_DRONES: usize = 2;
pub const ENEMY_DRONE_AGL: f32 = 130.0;
pub const ENEMY_DRONE_SPEED: f32 = 22.0;
pub const ENEMY_DRONE_HP: f32 = 16.0;
pub const ENEMY_DRONE_SPOT_RANGE: f32 = 550.0;
pub const ENEMY_DRONE_RESPAWN: f32 = 60.0;

// ---------------------------------------------------------------------------
// Ballistic warheads (§6.7)
// ---------------------------------------------------------------------------

/// Silos on each side's mainland: NATO in the American Midwest, the enemy
/// in western Siberia. Both are on the mini Earth, far from the front.
pub const NATO_SILO: [f32; 2] = geo(-100.0, 41.0);
pub const ENEMY_SILO: [f32; 2] = geo(68.0, 57.0);
pub const WARHEAD_STOCK: u32 = 4;
pub const ENEMY_WARHEAD_STOCK: u32 = 3;
/// Minimum time between two launches from the same side.
pub const WARHEAD_COOLDOWN: f32 = 60.0;
/// Flight time = WARHEAD_BASE_TIME + distance / WARHEAD_SPEED.
pub const WARHEAD_BASE_TIME: f32 = 12.0;
pub const WARHEAD_SPEED: f32 = 2_200.0;
/// Apex height as a fraction of the distance flown (clamped).
pub const WARHEAD_APEX_FRAC: f32 = 0.3;
pub const WARHEAD_APEX_MIN: f32 = 6_000.0;
pub const WARHEAD_APEX_MAX: f32 = 24_000.0;
pub const WARHEAD_DAMAGE: f32 = 340.0;
pub const WARHEAD_RADIUS: f32 = 650.0;
/// Enemy launches: first after this, then every ENEMY_WARHEAD_INTERVAL ± 25%.
pub const ENEMY_WARHEAD_FIRST: f32 = 240.0;
pub const ENEMY_WARHEAD_INTERVAL: f32 = 260.0;
/// Enemy aim error (m).
pub const ENEMY_WARHEAD_ERROR: f32 = 200.0;

// ---------------------------------------------------------------------------
// Rules (§6.6)
// ---------------------------------------------------------------------------

pub const CAPTURE_TIME: f32 = 30.0;

// ---------------------------------------------------------------------------
// FX
// ---------------------------------------------------------------------------

pub const PARTICLE_POOL: usize = 1_400;
pub const LIGHT_POOL: usize = 6;
pub const SHAKE_DECAY: f32 = 1.6;
