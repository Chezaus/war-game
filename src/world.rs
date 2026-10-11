//! Heightmap, terrain chunks, sea, sky, airbases, bridges, roads, forests.

use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::pbr::{CascadeShadowConfigBuilder, NotShadowCaster};
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::VisibilityRange;

use crate::config::*;
use crate::game_rules::EventLog;
use crate::geo::{self, smoothstep};
use crate::heightgen;
use crate::nav::{NavGrid, RoadNet};
use crate::state::GameplaySet;
use crate::weapons::{Boom, DamageEvent, DamageKind, DamageRelay, Detonation, HitSphere};

// ---------------------------------------------------------------------------
// Heightmap (pure, unit-tested)
// ---------------------------------------------------------------------------

#[derive(Resource, Clone)]
pub struct Heightmap {
    pub res: usize,
    pub cell: f32,
    pub heights: Vec<f32>,
}

impl Heightmap {
    pub fn from_heights(res: usize, heights: Vec<f32>) -> Self {
        assert_eq!(heights.len(), res * res);
        Self {
            res,
            cell: WORLD_SIZE / (res - 1) as f32,
            heights,
        }
    }

    /// Loads `assets/heightmap.png`; regenerates procedurally if missing.
    pub fn load() -> Self {
        match std::fs::read(HEIGHTMAP_PATH).ok().and_then(|b| Self::decode_png(&b)).filter(|h| h.res == HEIGHTMAP_RES) {
            Some(h) => h,
            None => {
                warn!("{HEIGHTMAP_PATH} missing, unreadable or outdated, generating terrain");
                Self::from_heights(HEIGHTMAP_RES, heightgen::generate())
            }
        }
    }

    fn decode_png(bytes: &[u8]) -> Option<Self> {
        let img = Image::from_buffer(
            bytes,
            ImageType::Extension("png"),
            CompressedImageFormats::NONE,
            false,
            ImageSampler::Default,
            RenderAssetUsages::default(),
        )
        .ok()?;
        let (w, h) = (img.width() as usize, img.height() as usize);
        if w != h {
            return None;
        }
        let heights: Vec<f32> = match img.texture_descriptor.format {
            TextureFormat::R16Uint | TextureFormat::R16Unorm => img
                .data
                .chunks_exact(2)
                .map(|c| heightgen::decode(u16::from_ne_bytes([c[0], c[1]])))
                .collect(),
            // 8-bit images get expanded to RGBA8 by Bevy; use red.
            _ if img.data.len() == w * h * 4 => img
                .data
                .chunks_exact(4)
                .map(|c| heightgen::decode(c[0] as u16 * 257))
                .collect(),
            _ => return None,
        };
        Some(Self::from_heights(w, heights))
    }

    pub fn vertex(&self, ix: usize, iz: usize) -> f32 {
        self.heights[iz.min(self.res - 1) * self.res + ix.min(self.res - 1)]
    }

    pub fn vertex_pos(&self, ix: usize, iz: usize) -> Vec3 {
        Vec3::new(
            -WORLD_HALF + ix as f32 * self.cell,
            self.vertex(ix, iz),
            -WORLD_HALF + iz as f32 * self.cell,
        )
    }

    /// Terrain height, interpolated on exactly the triangles the mesh uses
    /// (each cell is split along its (0,0)-(1,1) diagonal).
    pub fn height(&self, x: f32, z: f32) -> f32 {
        let max = (self.res - 1) as f32 - 0.0001;
        let gx = ((x + WORLD_HALF) / self.cell).clamp(0.0, max);
        let gz = ((z + WORLD_HALF) / self.cell).clamp(0.0, max);
        let (ix, iz) = (gx.floor() as usize, gz.floor() as usize);
        let (fx, fz) = (gx - ix as f32, gz - iz as f32);
        let h00 = self.vertex(ix, iz);
        let h10 = self.vertex(ix + 1, iz);
        let h01 = self.vertex(ix, iz + 1);
        let h11 = self.vertex(ix + 1, iz + 1);
        if fx > fz {
            h00 + fx * (h10 - h00) + fz * (h11 - h10)
        } else {
            h00 + fz * (h01 - h00) + fx * (h11 - h01)
        }
    }

    /// Height of whatever you would hit: terrain or the sea surface.
    pub fn surface(&self, x: f32, z: f32) -> f32 {
        self.height(x, z).max(SEA_LEVEL)
    }

    pub fn is_water(&self, x: f32, z: f32) -> bool {
        self.height(x, z) < SEA_LEVEL
    }

    /// March a ray against terrain/sea. Returns the hit point.
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Vec3> {
        let dir = dir.normalize_or_zero();
        let step = (self.cell * 0.25).min(40.0);
        let mut t = 0.0;
        let mut prev = origin;
        while t < max_dist {
            t += step;
            let p = origin + dir * t;
            if p.y <= self.surface(p.x, p.z) {
                // Refine between prev and p.
                let (mut a, mut b) = (prev, p);
                for _ in 0..20 {
                    let m = (a + b) * 0.5;
                    if m.y <= self.surface(m.x, m.z) {
                        b = m;
                    } else {
                        a = m;
                    }
                }
                return Some(b);
            }
            prev = p;
        }
        None
    }
}

pub fn on_runway(base: &AirbaseDef, x: f32, z: f32) -> bool {
    (x - base.center[0]).abs() < base.runway_width * 0.5 + 15.0
        && (z - base.center[1]).abs() < base.runway_length * 0.5 + 30.0
}

/// Where a jet starts its takeoff roll: south end, facing north.
pub fn runway_start(base: &AirbaseDef) -> Vec3 {
    Vec3::new(
        base.center[0],
        base.elevation,
        base.center[1] + base.runway_length * 0.5 - 120.0,
    )
}

pub fn base_center(base: &AirbaseDef) -> Vec3 {
    Vec3::new(base.center[0], base.elevation, base.center[1])
}

// ---------------------------------------------------------------------------
// Bridges (one wherever a road crosses a river or strait)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct BridgeInfo {
    pub name: String,
    /// Bank-to-bank deck line.
    pub a: Vec2,
    pub b: Vec2,
    /// Ground height at each end.
    pub ha: f32,
    pub hb: f32,
    pub hp: f32,
    pub intact: bool,
}

impl BridgeInfo {
    #[cfg(test)]
    fn list_has_water(&self, hm: &Heightmap) -> bool {
        (1..20).any(|k| {
            let p = self.a.lerp(self.b, k as f32 / 20.0);
            hm.is_water(p.x, p.y)
        })
    }
    pub fn len(&self) -> f32 {
        self.a.distance(self.b)
    }
    pub fn mid(&self) -> Vec2 {
        (self.a + self.b) * 0.5
    }
    pub fn dir(&self) -> Vec2 {
        (self.b - self.a).normalize_or(Vec2::X)
    }
    /// (fraction along the deck, sideways distance from its centre line).
    pub fn project(&self, p: Vec2) -> (f32, f32) {
        let d = self.dir();
        let rel = p - self.a;
        (rel.dot(d) / self.len().max(1.0), rel.perp_dot(d).abs())
    }
    /// Road surface height at fraction `t`: bank to bank with an arch.
    pub fn deck_top(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        self.ha + (self.hb - self.ha) * t + 1.0 + BRIDGE_ARCH * (std::f32::consts::PI * t).sin()
    }
    pub fn point(&self, t: f32) -> Vec3 {
        let p = self.a.lerp(self.b, t);
        Vec3::new(p.x, self.deck_top(t), p.y)
    }
    /// The end of the bridge nearest `p`, pushed 30 m out onto the bank.
    pub fn near_end(&self, p: Vec2) -> Vec2 {
        if p.distance(self.a) < p.distance(self.b) {
            self.a - self.dir() * 30.0
        } else {
            self.b + self.dir() * 30.0
        }
    }
    pub fn far_end(&self, p: Vec2) -> Vec2 {
        if p.distance(self.a) < p.distance(self.b) {
            self.b + self.dir() * 30.0
        } else {
            self.a - self.dir() * 30.0
        }
    }
}

#[derive(Resource, Clone)]
pub struct Bridges {
    pub list: Vec<BridgeInfo>,
    /// Bumped whenever a bridge falls, so paths can be re-planned.
    pub generation: u32,
}

impl Bridges {
    pub fn build(hm: &Heightmap) -> Self {
        let list = geo::find_bridges(|x, z| hm.height(x, z))
            .iter()
            .map(|s| BridgeInfo {
                name: geo::bridge_name(s),
                a: Vec2::from(s.a),
                b: Vec2::from(s.b),
                ha: hm.height(s.a[0], s.a[1]).max(0.5),
                hb: hm.height(s.b[0], s.b[1]).max(0.5),
                hp: BRIDGE_HP,
                intact: true,
            })
            .collect();
        Self { list, generation: 0 }
    }

    /// Deck height if (x, z) is on an intact bridge.
    pub fn deck_at(&self, x: f32, z: f32) -> Option<f32> {
        let p = Vec2::new(x, z);
        self.list.iter().filter(|b| b.intact).find_map(|b| {
            let (t, side) = b.project(p);
            ((0.0..=1.0).contains(&t) && side < BRIDGE_WIDTH * 0.5 + 1.5).then(|| b.deck_top(t))
        })
    }

    /// Ground or deck height for something standing at (x, z).
    pub fn ground(&self, hm: &Heightmap, x: f32, z: f32) -> f32 {
        let y = hm.height(x, z);
        match self.deck_at(x, z) {
            Some(d) => y.max(d),
            None => y,
        }
    }

    pub fn intact_count(&self) -> usize {
        self.list.iter().filter(|b| b.intact).count()
    }

    fn reset(&mut self) {
        for b in &mut self.list {
            b.hp = BRIDGE_HP;
            b.intact = true;
        }
        self.generation = 0;
    }
}

/// Root entity of bridge `.0` (index into `Bridges::list`); its deck
/// segments relay damage to it.
#[derive(Component)]
pub struct Bridge(pub usize);

#[derive(Component)]
pub struct BridgeSegment {
    pub bridge: usize,
    pub index: usize,
    pub count: usize,
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        let hm = Heightmap::load();
        let bridges = Bridges::build(&hm);
        let roads = RoadNet::build();
        let nav = NavGrid::build(&hm, &roads, &bridges);
        info!("world: {} bridges, {} roads", bridges.list.len(), roads.lines.len());
        app.insert_resource(hm)
            .insert_resource(bridges)
            .insert_resource(roads)
            .insert_resource(nav)
            .insert_resource(AmbientLight {
                color: Color::WHITE,
                brightness: 250.0,
            })
            .add_systems(Startup, (spawn_terrain, spawn_environment, spawn_airbases, spawn_roads, spawn_trees))
            .add_systems(OnEnter(crate::state::AppState::Playing), spawn_bridges)
            .add_systems(Update, bridge_damage.in_set(GameplaySet));
    }
}

/// Ground colour from climate, forest and height (before slope shading).
fn ground_color(x: f32, z: f32, h: f32) -> Vec3 {
    let sand = Vec3::new(0.76, 0.70, 0.50);
    let grass = Vec3::new(0.36, 0.54, 0.24);
    let forest = Vec3::new(0.17, 0.33, 0.15);
    let dune = Vec3::new(0.82, 0.69, 0.45);
    let tundra = Vec3::new(0.47, 0.50, 0.38);
    let rock = Vec3::new(0.46, 0.43, 0.39);
    let snow = Vec3::new(0.86, 0.87, 0.90);
    if h < -0.5 {
        return sand * 0.6;
    }
    let (lon, lat) = geo::lonlat(x, z);
    let mut c = sand.lerp(grass, smoothstep(0.5, 4.0, h));
    c = c.lerp(tundra, smoothstep(60.0, 70.0, lat.abs()));
    c = c.lerp(dune, geo::desert(lon, lat));
    let fd = geo::forest_density(x, z, h);
    c = c.lerp(forest, smoothstep(FOREST_THRESHOLD - 0.12, FOREST_THRESHOLD + 0.05, fd) * 0.9);
    c = c.lerp(rock, smoothstep(550.0, 900.0, h));
    c.lerp(snow, geo::snow(lat, h))
}

fn spawn_terrain(
    mut commands: Commands,
    hm: Res<Heightmap>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.95,
        reflectance: 0.1,
        ..default()
    });
    let colors: Vec<Vec3> = (0..hm.res * hm.res)
        .map(|i| {
            let p = hm.vertex_pos(i % hm.res, i / hm.res);
            ground_color(p.x, p.z, p.y)
        })
        .collect();
    let quads = (hm.res - 1) / TERRAIN_CHUNKS;
    for cz in 0..TERRAIN_CHUNKS {
        for cx in 0..TERRAIN_CHUNKS {
            let mut pos = Vec::new();
            let mut nor = Vec::new();
            let mut col = Vec::new();
            for qz in 0..quads {
                for qx in 0..quads {
                    let (ix, iz) = (cx * quads + qx, cz * quads + qz);
                    let v00 = hm.vertex_pos(ix, iz);
                    let v10 = hm.vertex_pos(ix + 1, iz);
                    let v01 = hm.vertex_pos(ix, iz + 1);
                    let v11 = hm.vertex_pos(ix + 1, iz + 1);
                    let c = |x: usize, z: usize| colors[z.min(hm.res - 1) * hm.res + x.min(hm.res - 1)];
                    let tris = [
                        ([v00, v11, v10], c(ix, iz) + c(ix + 1, iz + 1) + c(ix + 1, iz)),
                        ([v00, v01, v11], c(ix, iz) + c(ix, iz + 1) + c(ix + 1, iz + 1)),
                    ];
                    for (k, (tri, csum)) in tris.iter().enumerate() {
                        // Sea floor under the (opaque) sea is never seen.
                        if tri.iter().all(|v| v.y < -0.5) {
                            continue;
                        }
                        let n = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalize();
                        let jitter = (((ix * 7 + iz * 13 + k * 5) % 11) as f32 / 11.0 - 0.5) * 0.08;
                        let rock = Vec3::new(0.46, 0.43, 0.39);
                        let cc = (*csum / 3.0).lerp(rock, smoothstep(0.93, 0.80, n.y) * 0.8) * (1.0 + jitter);
                        for v in tri {
                            pos.push(v.to_array());
                            nor.push(n.to_array());
                            col.push([cc.x, cc.y, cc.z, 1.0]);
                        }
                    }
                }
            }
            if pos.is_empty() {
                continue;
            }
            let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
                .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nor)
                .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col);
            commands.spawn((
                Name::new(format!("terrain chunk {cx},{cz}")),
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material.clone()),
                Transform::IDENTITY,
                // Flat-shaded 125 m triangles self-shadow badly; the terrain
                // only receives shadows (jets, buildings).
                NotShadowCaster,
            ));
        }
    }
}

fn spawn_environment(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Name::new("sea"),
        Mesh3d(meshes.add(Plane3d::default().mesh().size(600_000.0, 600_000.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.08, 0.24, 0.38),
            perceptual_roughness: 0.12,
            reflectance: 0.35,
            ..default()
        })),
        Transform::from_xyz(0.0, SEA_LEVEL, 0.0),
    ));
    commands.spawn((
        Name::new("sun"),
        DirectionalLight {
            illuminance: 7_000.0,
            shadows_enabled: true,
            ..default()
        },
        CascadeShadowConfigBuilder {
            first_cascade_far_bound: 250.0,
            maximum_distance: 5_000.0,
            ..default()
        }
        .build(),
        Transform::from_xyz(0.0, 0.0, 0.0).looking_to(Vec3::new(-0.4, -0.75, 0.5), Vec3::Y),
    ));
}

fn spawn_airbases(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let asphalt = materials.add(StandardMaterial {
        base_color: Color::srgb(0.17, 0.17, 0.18),
        perceptual_roughness: 0.9,
        ..default()
    });
    let paint = materials.add(Color::srgb(0.9, 0.9, 0.85));
    let concrete = materials.add(Color::srgb(0.55, 0.55, 0.52));
    for (side, base) in [(Side::Nato, NATO_BASE), (Side::Enemy, ENEMY_BASE)] {
        let roof = materials.add(match side {
            Side::Nato => Color::srgb(0.35, 0.42, 0.50),
            Side::Enemy => Color::srgb(0.50, 0.30, 0.25),
        });
        let (cx, cz, y) = (base.center[0], base.center[1], base.elevation);
        let root = commands
            .spawn((
                Name::new(format!("{side:?} airbase")),
                Transform::from_xyz(cx, y, cz),
                Visibility::default(),
            ))
            .id();
        let mut parts = vec![commands
            .spawn((
                Mesh3d(meshes.add(Cuboid::new(base.runway_width, 0.4, base.runway_length))),
                MeshMaterial3d(asphalt.clone()),
                Transform::from_xyz(0.0, 0.05, 0.0),
            ))
            .id()];
        // Centre-line dashes and threshold bars.
        let dash = meshes.add(Cuboid::new(1.2, 0.45, 30.0));
        let mut z = -base.runway_length * 0.5 + 80.0;
        while z < base.runway_length * 0.5 - 80.0 {
            parts.push(
                commands
                    .spawn((Mesh3d(dash.clone()), MeshMaterial3d(paint.clone()), Transform::from_xyz(0.0, 0.05, z)))
                    .id(),
            );
            z += 60.0;
        }
        let bar = meshes.add(Cuboid::new(2.0, 0.45, 25.0));
        for end in [-1.0, 1.0] {
            for i in 0..8 {
                let x = -base.runway_width * 0.5 + 5.0 + i as f32 * 5.7;
                parts.push(
                    commands
                        .spawn((
                            Mesh3d(bar.clone()),
                            MeshMaterial3d(paint.clone()),
                            Transform::from_xyz(x, 0.05, end * (base.runway_length * 0.5 - 20.0)),
                        ))
                        .id(),
                );
            }
        }
        // Apron, hangars, tower.
        parts.push(
            commands
                .spawn((
                    Mesh3d(meshes.add(Cuboid::new(160.0, 0.3, 500.0))),
                    MeshMaterial3d(concrete.clone()),
                    Transform::from_xyz(130.0, 0.0, 0.0),
                ))
                .id(),
        );
        let hangar = meshes.add(Cuboid::new(40.0, 14.0, 45.0));
        for i in 0..4 {
            parts.push(
                commands
                    .spawn((
                        Mesh3d(hangar.clone()),
                        MeshMaterial3d(roof.clone()),
                        Transform::from_xyz(190.0, 7.0, -180.0 + i as f32 * 60.0),
                    ))
                    .id(),
            );
        }
        parts.push(
            commands
                .spawn((
                    Mesh3d(meshes.add(Cuboid::new(9.0, 28.0, 9.0))),
                    MeshMaterial3d(concrete.clone()),
                    Transform::from_xyz(110.0, 14.0, 160.0),
                ))
                .id(),
        );
        parts.push(
            commands
                .spawn((
                    Mesh3d(meshes.add(Cuboid::new(13.0, 5.0, 13.0))),
                    MeshMaterial3d(roof.clone()),
                    Transform::from_xyz(110.0, 30.0, 160.0),
                ))
                .id(),
        );
        commands.entity(root).add_children(&parts);
    }
}

/// Road ribbons draped over the terrain; bridges carry their own deck.
fn spawn_roads(
    mut commands: Commands,
    hm: Res<Heightmap>,
    roads: Res<RoadNet>,
    bridges: Res<Bridges>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let asphalt = materials.add(StandardMaterial {
        base_color: Color::srgb(0.22, 0.22, 0.23),
        perceptual_roughness: 0.95,
        // Draw over the terrain triangles it lies on.
        depth_bias: 200.0,
        ..default()
    });
    for (r, line) in roads.lines.iter().enumerate() {
        let mut pos = Vec::new();
        let mut nor = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        let mut prev_ok = false;
        for (i, p) in line.iter().enumerate() {
            let ok = !hm.is_water(p.x, p.y) && bridges.deck_at(p.x, p.y).is_none() && !on_any_runway(p.x, p.y);
            let next = line[(i + 1).min(line.len() - 1)];
            let prev = line[i.saturating_sub(1)];
            let side = (next - prev).normalize_or(Vec2::X).perp() * ROAD_WIDTH * 0.5;
            for s in [-1.0, 1.0] {
                let q = *p + side * s;
                pos.push([q.x, hm.height(q.x, q.y) + 0.6, q.y]);
                nor.push([0.0, 1.0, 0.0]);
            }
            if ok && prev_ok {
                let v = (pos.len() - 4) as u32;
                idx.extend_from_slice(&[v, v + 2, v + 1, v + 1, v + 2, v + 3]);
            }
            prev_ok = ok;
        }
        if idx.is_empty() {
            continue;
        }
        let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nor)
            .with_inserted_indices(Indices::U32(idx));
        commands.spawn((
            Name::new(geo::ROADS[r].name),
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(asphalt.clone()),
            Transform::IDENTITY,
            NotShadowCaster,
        ));
    }
}

fn on_any_runway(x: f32, z: f32) -> bool {
    [NATO_BASE, ENEMY_BASE].iter().any(|b| on_runway(b, x, z))
}

/// Low-poly trees in the theatre's forests, merged into one mesh per 4 km
/// chunk and hidden beyond TREE_VIEW_DIST.
fn spawn_trees(
    mut commands: Commands,
    hm: Res<Heightmap>,
    roads: Res<RoadNet>,
    bridges: Res<Bridges>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    const CHUNK: f32 = 4_000.0;
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.9,
        ..default()
    });
    let [x0, z0] = geo::geo(TREE_REGION[0], TREE_REGION[3]);
    let [x1, z1] = geo::geo(TREE_REGION[2], TREE_REGION[1]);
    // Keep clear of airbases, infrastructure and bridge heads.
    let mut clear: Vec<(Vec2, f32)> = [NATO_BASE, ENEMY_BASE].iter().map(|b| (Vec2::from(b.center), BASE_GENTLE_RADIUS * 0.8)).collect();
    for (_, ll) in NATO_SITES.iter().chain(ENEMY_SITES.iter()) {
        clear.push((Vec2::from(geo::geo_ll(*ll)), 220.0));
    }
    for b in &bridges.list {
        clear.push((b.mid(), b.len() * 0.5 + 120.0));
    }
    let mut total = 0;
    let (ncx, ncz) = (((x1 - x0) / CHUNK).ceil() as i32, ((z1 - z0) / CHUNK).ceil() as i32);
    for cz in 0..ncz {
        for cx in 0..ncx {
            let origin = Vec2::new(x0 + cx as f32 * CHUNK, z0 + cz as f32 * CHUNK);
            let center = origin + Vec2::splat(CHUNK * 0.5);
            let center3 = Vec3::new(center.x, hm.height(center.x, center.y), center.y);
            let mut pos: Vec<[f32; 3]> = Vec::new();
            let mut nor: Vec<[f32; 3]> = Vec::new();
            let mut col: Vec<[f32; 4]> = Vec::new();
            let mut idx: Vec<u32> = Vec::new();
            let n = (CHUNK / TREE_SPACING) as i32;
            for gz in 0..n {
                for gx in 0..n {
                    let (ix, iz) = (((origin.x / TREE_SPACING) as i32) + gx, ((origin.y / TREE_SPACING) as i32) + gz);
                    let r = |k: u32| geo::hash(ix, iz, k);
                    let p = Vec2::new(
                        origin.x + (gx as f32 + r(1)) * TREE_SPACING,
                        origin.y + (gz as f32 + r(2)) * TREE_SPACING,
                    );
                    let h = hm.height(p.x, p.y);
                    if h < 2.0 {
                        continue;
                    }
                    let fd = geo::forest_density(p.x, p.y, h);
                    // Ragged forest edges.
                    if fd < FOREST_THRESHOLD || r(3) > 0.55 + (fd - FOREST_THRESHOLD) * 3.0 {
                        continue;
                    }
                    let slope = (hm.height(p.x + 15.0, p.y) - hm.height(p.x - 15.0, p.y)).abs()
                        + (hm.height(p.x, p.y + 15.0) - hm.height(p.x, p.y - 15.0)).abs();
                    if slope > 30.0 || clear.iter().any(|(c, rad)| c.distance(p) < *rad) || roads.nearest(p, 22.0).is_some() {
                        continue;
                    }
                    let (_, lat) = geo::lonlat(p.x, p.y);
                    let conifer = r(4) < 0.35 + smoothstep(50.0, 60.0, lat) * 0.6 + smoothstep(250.0, 500.0, h) * 0.5;
                    let base = Vec3::new(p.x - center3.x, h - center3.y - 0.5, p.y - center3.z);
                    push_tree(&mut pos, &mut nor, &mut col, &mut idx, base, conifer, r(5), r(6));
                    total += 1;
                }
            }
            if idx.is_empty() {
                continue;
            }
            let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
                .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nor)
                .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
                .with_inserted_indices(Indices::U32(idx));
            commands.spawn((
                Name::new(format!("trees {cx},{cz}")),
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(center3),
                VisibilityRange {
                    start_margin: 0.0..0.0,
                    end_margin: TREE_VIEW_DIST..TREE_VIEW_DIST + 2_000.0,
                    use_aabb: true,
                },
                NotShadowCaster,
            ));
        }
    }
    info!("planted {total} trees");
}

/// One low-poly tree: a cone (conifer) or a double cone blob (broadleaf)
/// on a thin trunk.
#[allow(clippy::too_many_arguments)]
fn push_tree(
    pos: &mut Vec<[f32; 3]>,
    nor: &mut Vec<[f32; 3]>,
    col: &mut Vec<[f32; 4]>,
    idx: &mut Vec<u32>,
    base: Vec3,
    conifer: bool,
    r1: f32,
    r2: f32,
) {
    let mut add = |p: Vec3, n: Vec3, c: [f32; 4]| -> u32 {
        pos.push(p.to_array());
        nor.push(n.normalize().to_array());
        col.push(c);
        (pos.len() - 1) as u32
    };
    let shade = 0.85 + r2 * 0.3;
    let trunk_c = [0.30, 0.22, 0.15, 1.0];
    // Trunk: a thin three-sided spike.
    let trunk_h = if conifer { 3.0 } else { 4.5 };
    let top = add(base + Vec3::Y * trunk_h, Vec3::Y, trunk_c);
    let t: Vec<u32> = (0..3)
        .map(|k| {
            let a = k as f32 * std::f32::consts::TAU / 3.0;
            let d = Vec3::new(a.cos(), 0.0, a.sin());
            add(base + d * 0.7, d, trunk_c)
        })
        .collect();
    for k in 0..3 {
        idx.extend_from_slice(&[top, t[(k + 1) % 3], t[k]]);
    }
    const SIDES: usize = 6;
    let rot = r1 * std::f32::consts::TAU;
    if conifer {
        let (hgt, rad) = (14.0 + r1 * 10.0, 3.5 + r2 * 2.0);
        let c = [0.11 * shade, 0.27 * shade, 0.13 * shade, 1.0];
        let y0 = base + Vec3::Y * 2.0;
        let apex = add(y0 + Vec3::Y * hgt, Vec3::Y, c);
        let ring: Vec<u32> = (0..SIDES)
            .map(|k| {
                let a = rot + k as f32 * std::f32::consts::TAU / SIDES as f32;
                let d = Vec3::new(a.cos(), 0.0, a.sin());
                add(y0 + d * rad, d + Vec3::Y * 0.4, c)
            })
            .collect();
        for k in 0..SIDES {
            idx.extend_from_slice(&[apex, ring[(k + 1) % SIDES], ring[k]]);
        }
    } else {
        let (hgt, rad) = (9.0 + r1 * 6.0, 4.0 + r2 * 2.5);
        let c = [0.22 * shade, 0.40 * shade, 0.15 * shade, 1.0];
        let y0 = base + Vec3::Y * 3.0;
        let topv = add(y0 + Vec3::Y * hgt, Vec3::Y, c);
        let bottom = add(y0, -Vec3::Y, c);
        let ring: Vec<u32> = (0..SIDES)
            .map(|k| {
                let a = rot + k as f32 * std::f32::consts::TAU / SIDES as f32;
                let d = Vec3::new(a.cos(), 0.0, a.sin());
                add(y0 + Vec3::Y * hgt * 0.45 + d * rad, d, c)
            })
            .collect();
        for k in 0..SIDES {
            idx.extend_from_slice(&[topv, ring[(k + 1) % SIDES], ring[k]]);
            idx.extend_from_slice(&[bottom, ring[k], ring[(k + 1) % SIDES]]);
        }
    }
}

fn spawn_bridges(
    mut commands: Commands,
    hm: Res<Heightmap>,
    mut bridges: ResMut<Bridges>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    bridges.reset();
    let concrete = materials.add(StandardMaterial {
        base_color: Color::srgb(0.6, 0.58, 0.55),
        perceptual_roughness: 0.85,
        ..default()
    });
    let steel = materials.add(Color::srgb(0.35, 0.36, 0.40));
    let pillar = meshes.add(Cylinder::new(2.5, 1.0));
    for (bi, b) in bridges.list.iter().enumerate() {
        let mid = b.point(0.5);
        let root = commands
            .spawn((
                Name::new(b.name.clone()),
                Bridge(bi),
                Transform::from_translation(mid),
                Visibility::default(),
                StateScoped(crate::state::AppState::Playing),
            ))
            .id();
        let count = ((b.len() / BRIDGE_SEGMENT).round() as usize).max(3);
        let seg_len = b.len() / count as f32;
        let deck = meshes.add(Cuboid::new(BRIDGE_WIDTH, 2.5, seg_len + 1.0));
        let rail = meshes.add(Cuboid::new(0.5, 1.2, seg_len + 1.0));
        for i in 0..count {
            let (p0, p1) = (b.point(i as f32 / count as f32), b.point((i + 1) as f32 / count as f32));
            let center = (p0 + p1) * 0.5 - Vec3::Y * 1.25;
            commands
                .spawn((
                    BridgeSegment { bridge: bi, index: i, count },
                    Mesh3d(deck.clone()),
                    MeshMaterial3d(concrete.clone()),
                    Transform::from_translation(center).looking_to(p1 - p0, Vec3::Y),
                    HitSphere { radius: seg_len * 0.55, side: None },
                    DamageRelay(root),
                    StateScoped(crate::state::AppState::Playing),
                ))
                .with_children(|p| {
                    for xs in [-1.0, 1.0] {
                        p.spawn((
                            Mesh3d(rail.clone()),
                            MeshMaterial3d(steel.clone()),
                            Transform::from_xyz(xs * (BRIDGE_WIDTH * 0.5 - 0.3), 1.8, 0.0),
                        ));
                    }
                });
        }
        // Pillars down to the river bed / banks at every joint.
        for i in 1..count {
            let top = b.point(i as f32 / count as f32) - Vec3::Y * 2.5;
            let ground = hm.height(top.x, top.z).min(top.y - 1.0) - 5.0;
            let h = top.y - ground;
            commands.spawn((
                Mesh3d(pillar.clone()),
                MeshMaterial3d(concrete.clone()),
                Transform::from_xyz(top.x, ground + h * 0.5, top.z).with_scale(Vec3::new(1.0, h, 1.0)),
                StateScoped(crate::state::AppState::Playing),
            ));
        }
    }
}

fn bridge_damage(
    mut events: EventReader<DamageEvent>,
    roots: Query<&Bridge>,
    mut bridges: ResMut<Bridges>,
    mut segments: Query<(&BridgeSegment, &mut Visibility, &mut HitSphere, &Transform)>,
    mut detonations: EventWriter<Detonation>,
    mut log: ResMut<EventLog>,
) {
    for ev in events.read() {
        let Ok(&Bridge(bi)) = roots.get(ev.target) else {
            continue;
        };
        let b = &mut bridges.list[bi];
        if !b.intact {
            continue;
        }
        let factor = if ev.kind == DamageKind::Bullet { BRIDGE_BULLET_FACTOR } else { 1.0 };
        b.hp -= ev.amount * factor;
        if b.hp > 0.0 {
            continue;
        }
        b.intact = false;
        log.push(format!("{} destroyed!", b.name));
        bridges.generation += 1;
        // The middle third drops into the water.
        for (seg, mut vis, mut sphere, tf) in &mut segments {
            if seg.bridge != bi {
                continue;
            }
            let third = seg.count as f32 / 3.0;
            if (seg.index as f32 + 0.5) > third && (seg.index as f32 + 0.5) < seg.count as f32 - third {
                *vis = Visibility::Hidden;
                sphere.radius = 0.0;
                detonations.send(Detonation::fx(tf.translation, Boom::Large));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp() -> Heightmap {
        // h = ix + 2 * iz on a 5x5 grid.
        let res = 5;
        let heights = (0..res * res).map(|i| (i % res) as f32 + 2.0 * (i / res) as f32).collect();
        Heightmap::from_heights(res, heights)
    }

    #[test]
    fn height_matches_vertices() {
        let hm = ramp();
        let p = hm.vertex_pos(2, 3);
        assert!((hm.height(p.x, p.z) - 8.0).abs() < 1e-3);
    }

    #[test]
    fn height_interpolates_planes_exactly() {
        let hm = ramp();
        // A plane is reproduced exactly by triangle interpolation.
        let x = -WORLD_HALF + 1.3 * hm.cell;
        let z = -WORLD_HALF + 2.6 * hm.cell;
        assert!((hm.height(x, z) - (1.3 + 2.0 * 2.6)).abs() < 1e-3);
        let x = -WORLD_HALF + 1.8 * hm.cell;
        let z = -WORLD_HALF + 2.1 * hm.cell;
        assert!((hm.height(x, z) - (1.8 + 2.0 * 2.1)).abs() < 1e-3);
    }

    #[test]
    fn height_clamps_outside_map() {
        let hm = ramp();
        assert!((hm.height(-1e6, -1e6) - 0.0).abs() < 1e-3);
        assert!((hm.height(1e6, 1e6) - 12.0).abs() < 1e-2);
    }

    #[test]
    fn water_is_below_sea_level() {
        let heights = vec![-5.0, -5.0, 10.0, 10.0];
        let hm = Heightmap::from_heights(2, heights);
        assert!(hm.is_water(-WORLD_HALF + 1.0, -WORLD_HALF + 1.0));
        assert!(!hm.is_water(WORLD_HALF - 1.0, WORLD_HALF - 1.0));
    }

    #[test]
    fn raycast_hits_ground() {
        let hm = Heightmap::from_heights(2, vec![100.0; 4]);
        let hit = hm.raycast(Vec3::new(0.0, 1000.0, 0.0), Vec3::NEG_Y, 5000.0).unwrap();
        assert!((hit.y - 100.0).abs() < 1.0);
    }

    #[test]
    fn generated_map_layout() {
        let hm = Heightmap::from_heights(HEIGHTMAP_RES, heightgen::generate());
        let bridges = Bridges::build(&hm);
        assert!(bridges.list.len() > 20, "roads cross rivers on bridges");
        for b in &bridges.list {
            assert!(hm.is_water(b.mid().x, b.mid().y) || b.list_has_water(&hm), "{} spans water", b.name);
        }
        let at = |ll: (f32, f32)| geo::geo_ll(ll);
        let [x, z] = at((-30.0, 45.0));
        assert!(hm.is_water(x, z), "Atlantic");
        let [x, z] = at((34.0, 43.5));
        assert!(hm.is_water(x, z), "Black Sea");
        let [x, z] = at((-2.0, 52.5));
        assert!(!hm.is_water(x, z), "Britain");
        for base in [NATO_BASE, ENEMY_BASE] {
            let s = runway_start(&base);
            assert!((hm.height(s.x, s.z) - base.elevation).abs() < 0.5, "runway is flat");
        }
    }
}
