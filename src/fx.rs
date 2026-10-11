//! Explosions and effects (§8): pooled camera-facing particles (fireball,
//! smoke, sparks, debris), point-light flashes, camera shake, trails.

use bevy::prelude::*;
use rand::Rng;

use crate::config::*;
use crate::state::{AppState, CameraSet, MainCamera, ViewMode};
use crate::weapons::{Boom, Detonation};
use crate::world::Heightmap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PKind {
    Fire,
    Smoke,
    DarkSmoke,
    Spark,
    Debris,
    Vapor,
    Dust,
    Splash,
}

const ALPHA_STEPS: usize = 6;

pub struct PSpawn {
    pub kind: PKind,
    pub pos: Vec3,
    pub vel: Vec3,
    pub size0: f32,
    pub size1: f32,
    pub life: f32,
    pub gravity: f32,
    pub drag: f32,
}

/// Queue of particle spawn requests. Anything can push; `spawn_particles`
/// assigns them to pooled entities.
#[derive(Resource, Default)]
pub struct Particles {
    queue: Vec<PSpawn>,
    flashes: Vec<(Vec3, f32, f32)>,
}

fn rand_dir(rng: &mut impl Rng) -> Vec3 {
    loop {
        let v = Vec3::new(rng.gen_range(-1.0..1.0), rng.gen_range(-1.0..1.0), rng.gen_range(-1.0..1.0));
        let l = v.length_squared();
        if l > 0.01 && l <= 1.0 {
            return v / l.sqrt();
        }
    }
}

impl Particles {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(&mut self, kind: PKind, pos: Vec3, vel: Vec3, size0: f32, size1: f32, life: f32, gravity: f32, drag: f32) {
        self.queue.push(PSpawn { kind, pos, vel, size0, size1, life, gravity, drag });
    }

    pub fn trail(&mut self, pos: Vec3, burning: bool) {
        self.spawn(PKind::Smoke, pos, Vec3::Y * 1.5, 1.4, 7.0, 2.4, 0.0, 0.5);
        if burning {
            self.spawn(PKind::Fire, pos, Vec3::ZERO, 1.6, 0.6, 0.1, 0.0, 0.0);
        }
    }

    pub fn flare_smoke(&mut self, pos: Vec3) {
        self.spawn(PKind::Smoke, pos, Vec3::ZERO, 1.5, 6.0, 1.6, 0.0, 0.5);
        self.spawn(PKind::Spark, pos, Vec3::ZERO, 2.5, 0.5, 0.12, 0.0, 0.0);
    }

    pub fn fire_trail(&mut self, pos: Vec3) {
        self.spawn(PKind::Fire, pos, Vec3::ZERO, 2.5, 4.5, 0.35, 0.0, 0.0);
        self.spawn(PKind::DarkSmoke, pos, Vec3::Y * 2.0, 3.0, 11.0, 2.5, 0.0, 0.3);
    }

    pub fn smoke_trail(&mut self, pos: Vec3) {
        self.spawn(PKind::DarkSmoke, pos, Vec3::Y, 2.0, 8.0, 2.0, 0.0, 0.3);
    }

    pub fn vapor(&mut self, pos: Vec3) {
        self.spawn(PKind::Vapor, pos, Vec3::ZERO, 1.0, 4.5, 1.2, 0.0, 0.3);
    }

    pub fn splash(&mut self, pos: Vec3) {
        self.spawn(PKind::Splash, pos, Vec3::Y * 12.0, 2.0, 5.0, 0.8, GRAVITY, 0.0);
    }

    pub fn dust(&mut self, pos: Vec3) {
        self.spawn(PKind::Dust, pos + Vec3::Y, Vec3::Y * 4.0, 2.0, 7.0, 1.3, 0.0, 0.5);
    }

    pub fn explosion(&mut self, pos: Vec3, size: Boom) {
        let mut rng = rand::thread_rng();
        let (fires, fsize, smokes, ssize, sparks, debris, light) = match size {
            Boom::Spark => (1, 2.0, 1, 3.0, 3, 0, 0.0),
            Boom::Medium => (14, 9.0, 12, 12.0, 10, 6, 1.5e8),
            Boom::Large => (28, 20.0, 24, 24.0, 18, 14, 5e8),
            Boom::Huge => (60, 110.0, 40, 120.0, 30, 30, 4e10),
        };
        let speed = fsize * 1.4;
        for _ in 0..fires {
            let d = rand_dir(&mut rng);
            let s = fsize * rng.gen_range(0.7..1.2);
            self.spawn(PKind::Fire, pos + d * fsize * 0.3, d * speed * rng.gen_range(0.3..1.0), s, s * 2.4, rng.gen_range(0.6..1.2) * (fsize / 9.0).sqrt(), -2.0, 1.5);
        }
        for _ in 0..smokes {
            let d = rand_dir(&mut rng);
            let s = ssize * rng.gen_range(0.6..1.1);
            let kind = if rng.gen_bool(0.5) { PKind::DarkSmoke } else { PKind::Smoke };
            self.spawn(kind, pos + d * ssize * 0.4, d * speed * 0.5 + Vec3::Y * 4.0, s, s * 3.0, rng.gen_range(3.0..6.0) * (ssize / 12.0).sqrt(), -1.0, 0.6);
        }
        for _ in 0..sparks {
            let d = rand_dir(&mut rng);
            let s = if size == Boom::Spark { 0.8 } else { 1.6 };
            self.spawn(PKind::Spark, pos, d * rng.gen_range(20.0..60.0) * (fsize / 9.0).sqrt(), s, s * 0.3, rng.gen_range(0.2..0.6), GRAVITY, 0.5);
        }
        for _ in 0..debris {
            let d = rand_dir(&mut rng) + Vec3::Y * 0.6;
            let s = rng.gen_range(0.6..1.8) * (fsize / 9.0).sqrt();
            self.spawn(PKind::Debris, pos, d * rng.gen_range(15.0..45.0), s, s, rng.gen_range(3.0..6.0), GRAVITY, 0.05);
        }
        if size == Boom::Huge {
            // Mushroom cloud: a stem of fire and smoke climbing ~900 m
            // (velocity / drag), a wide cap on top, and a dust ring racing
            // out across the ground to the edge of the blast.
            for k in 0..36 {
                let up = k as f32 / 36.0;
                let jitter = Vec3::new(rng.gen_range(-1.0..1.0), 0.0, rng.gen_range(-1.0..1.0)) * 50.0;
                let v = Vec3::Y * (60.0 + 200.0 * up);
                if k % 3 == 0 {
                    self.spawn(PKind::Fire, pos + jitter, v, 90.0, 160.0, rng.gen_range(2.0..4.0), 0.0, 0.3);
                } else {
                    self.spawn(PKind::DarkSmoke, pos + jitter, v, 70.0, 190.0, rng.gen_range(14.0..20.0), 0.0, 0.3);
                }
            }
            for k in 0..30 {
                let spread = Vec3::new(rng.gen_range(-1.0..1.0), 0.0, rng.gen_range(-1.0..1.0)) * 70.0;
                let kind = if k % 6 == 0 { PKind::Fire } else if k % 2 == 0 { PKind::Smoke } else { PKind::DarkSmoke };
                let life = if kind == PKind::Fire { 3.5 } else { rng.gen_range(16.0..22.0) };
                self.spawn(kind, pos + Vec3::Y * 60.0, Vec3::Y * 290.0 + spread, 150.0, 430.0, life, 0.0, 0.3);
            }
            for _ in 0..36 {
                let d = rand_dir(&mut rng).with_y(0.0).normalize_or_zero();
                self.spawn(PKind::Dust, pos + Vec3::Y * 8.0, d * rng.gen_range(300.0..400.0), 40.0, 170.0, rng.gen_range(5.0..7.0), 0.0, 0.6);
            }
        }
        if light > 0.0 {
            let range = match size {
                Boom::Huge => 6_000.0,
                Boom::Large => 900.0,
                _ => 450.0,
            };
            self.flashes.push((pos, light, range));
        }
    }
}

#[derive(Component)]
struct Particle {
    active: bool,
    kind: PKind,
    age: f32,
    life: f32,
    vel: Vec3,
    size0: f32,
    size1: f32,
    gravity: f32,
    drag: f32,
    spin: Vec3,
    step: usize,
}

#[derive(Component)]
struct Flash {
    age: f32,
    intensity: f32,
}

#[derive(Resource)]
struct FxPool {
    particles: Vec<Entity>,
    next: usize,
    lights: Vec<Entity>,
    next_light: usize,
}

#[derive(Resource)]
struct FxAssets {
    quad: Handle<Mesh>,
    cube: Handle<Mesh>,
    mats: Vec<Vec<Handle<StandardMaterial>>>,
}

fn kind_index(k: PKind) -> usize {
    k as usize
}

/// Screen shake "trauma" 0..1; decays over time.
#[derive(Resource, Default)]
pub struct Shake {
    pub trauma: f32,
}

pub struct FxPlugin;

impl Plugin for FxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Particles>()
            .init_resource::<Shake>()
            .add_systems(Startup, setup_pool)
            .add_systems(Update, (detonation_fx, spawn_particles, update_particles, update_flashes).chain().after(CameraSet::Shake))
            .add_systems(Update, camera_shake.in_set(CameraSet::Shake))
            .add_systems(OnExit(AppState::Playing), clear_pool);
    }
}

/// Soft round puff with a little lumpy noise, white with alpha falloff.
fn puff_image() -> Image {
    use bevy::render::render_asset::RenderAssetUsages;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
    let n = 64u32;
    let mut data = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let dx = (x as f32 + 0.5) / n as f32 * 2.0 - 1.0;
            let dy = (y as f32 + 0.5) / n as f32 * 2.0 - 1.0;
            let angle = dy.atan2(dx);
            let lump = 1.0 + 0.12 * (angle * 5.0).sin() + 0.06 * (angle * 11.0 + 1.0).sin();
            let r = (dx * dx + dy * dy).sqrt() * lump;
            let a = (1.0 - r).clamp(0.0, 1.0).powf(1.4);
            data.extend_from_slice(&[255, 255, 255, (a * 255.0) as u8]);
        }
    }
    Image::new(
        Extent3d { width: n, height: n, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn setup_pool(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let puff = images.add(puff_image());
    // Per kind: ALPHA_STEPS materials fading out.
    let palette: [(PKind, [f32; 3], [f32; 3], f32, bool); 8] = [
        (PKind::Fire, [1.0, 0.85, 0.4], [0.9, 0.25, 0.05], 0.95, true),
        (PKind::Smoke, [0.75, 0.75, 0.75], [0.6, 0.6, 0.62], 0.55, false),
        (PKind::DarkSmoke, [0.25, 0.23, 0.22], [0.35, 0.34, 0.33], 0.65, false),
        (PKind::Spark, [1.0, 0.95, 0.6], [1.0, 0.6, 0.2], 1.0, true),
        (PKind::Debris, [0.15, 0.13, 0.12], [0.15, 0.13, 0.12], 1.0, false),
        (PKind::Vapor, [0.92, 0.92, 0.95], [0.9, 0.9, 0.95], 0.35, false),
        (PKind::Dust, [0.55, 0.47, 0.36], [0.5, 0.45, 0.38], 0.55, false),
        (PKind::Splash, [0.85, 0.9, 0.95], [0.8, 0.88, 0.95], 0.7, false),
    ];
    let mut mats = vec![Vec::new(); palette.len()];
    for (kind, c0, c1, a0, glow) in palette {
        for i in 0..ALPHA_STEPS {
            let t = i as f32 / (ALPHA_STEPS - 1) as f32;
            let c = Vec3::from(c0).lerp(Vec3::from(c1), t);
            let alpha = if kind == PKind::Debris { 1.0 } else { a0 * (1.0 - t * 0.85) };
            let base = Color::srgba(c.x, c.y, c.z, alpha);
            mats[kind_index(kind)].push(materials.add(StandardMaterial {
                base_color: base,
                base_color_texture: (kind != PKind::Debris).then(|| puff.clone()),
                emissive: if glow { LinearRgba::from(base) * (6.0 * (1.0 - t) + 1.0) } else { LinearRgba::BLACK },
                unlit: kind != PKind::Debris,
                alpha_mode: if kind == PKind::Debris {
                    AlphaMode::Opaque
                } else if glow {
                    AlphaMode::Add
                } else {
                    AlphaMode::Blend
                },
                double_sided: true,
                cull_mode: None,
                fog_enabled: true,
                ..default()
            }));
        }
    }
    let quad = meshes.add(Rectangle::new(1.0, 1.0));
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let particles = (0..PARTICLE_POOL)
        .map(|_| {
            commands
                .spawn((
                    Particle {
                        active: false,
                        kind: PKind::Smoke,
                        age: 0.0,
                        life: 1.0,
                        vel: Vec3::ZERO,
                        size0: 1.0,
                        size1: 1.0,
                        gravity: 0.0,
                        drag: 0.0,
                        spin: Vec3::ZERO,
                        step: 0,
                    },
                    Mesh3d(quad.clone()),
                    MeshMaterial3d(mats[0][0].clone()),
                    Transform::default(),
                    Visibility::Hidden,
                    bevy::render::view::NoFrustumCulling,
                ))
                .id()
        })
        .collect();
    let lights = (0..LIGHT_POOL)
        .map(|_| {
            commands
                .spawn((
                    Flash { age: 10.0, intensity: 0.0 },
                    PointLight {
                        intensity: 0.0,
                        range: 500.0,
                        color: Color::srgb(1.0, 0.7, 0.4),
                        shadows_enabled: false,
                        ..default()
                    },
                    Transform::default(),
                ))
                .id()
        })
        .collect();
    commands.insert_resource(FxPool { particles, next: 0, lights, next_light: 0 });
    commands.insert_resource(FxAssets { quad, cube, mats });
}

fn detonation_fx(
    mut events: EventReader<Detonation>,
    mut particles: ResMut<Particles>,
    mut shake: ResMut<Shake>,
    cam: Query<&Transform, With<MainCamera>>,
) {
    let cam_pos = cam.get_single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    for ev in events.read() {
        particles.explosion(ev.pos, ev.size);
        let strength = match ev.size {
            Boom::Spark => 0.0,
            Boom::Medium => 0.45,
            Boom::Large => 0.9,
            Boom::Huge => 1.0,
        };
        let reach = if ev.size == Boom::Huge { 3_000.0 } else { 350.0 };
        let d = cam_pos.distance(ev.pos);
        shake.trauma = (shake.trauma + strength / (1.0 + (d / reach).powi(2))).min(1.0);
    }
}

fn spawn_particles(
    mut particles: ResMut<Particles>,
    mut pool: ResMut<FxPool>,
    assets: Res<FxAssets>,
    mut q: Query<(&mut Particle, &mut Transform, &mut Visibility, &mut Mesh3d, &mut MeshMaterial3d<StandardMaterial>)>,
    mut lights: Query<(&mut Flash, &mut Transform, &mut PointLight), Without<Particle>>,
) {
    let mut rng = rand::thread_rng();
    for s in particles.queue.drain(..) {
        let e = pool.particles[pool.next];
        pool.next = (pool.next + 1) % pool.particles.len();
        let Ok((mut p, mut tf, mut vis, mut mesh, mut mat)) = q.get_mut(e) else {
            continue;
        };
        *p = Particle {
            active: true,
            kind: s.kind,
            age: 0.0,
            life: s.life,
            vel: s.vel,
            size0: s.size0,
            size1: s.size1,
            gravity: s.gravity,
            drag: s.drag,
            spin: rand_dir(&mut rng) * 6.0,
            step: 0,
        };
        tf.translation = s.pos;
        tf.scale = Vec3::splat(s.size0);
        *vis = Visibility::Visible;
        let want_mesh = if s.kind == PKind::Debris { &assets.cube } else { &assets.quad };
        if mesh.0 != *want_mesh {
            mesh.0 = want_mesh.clone();
        }
        mat.0 = assets.mats[kind_index(s.kind)][0].clone();
    }
    for (pos, intensity, range) in particles.flashes.drain(..) {
        let e = pool.lights[pool.next_light];
        pool.next_light = (pool.next_light + 1) % pool.lights.len();
        if let Ok((mut f, mut tf, mut light)) = lights.get_mut(e) {
            f.age = 0.0;
            f.intensity = intensity;
            tf.translation = pos + Vec3::Y * 5.0;
            light.range = range;
            light.intensity = intensity;
        }
    }
}

fn update_particles(
    time: Res<Time>,
    hm: Res<Heightmap>,
    assets: Res<FxAssets>,
    cam: Query<&Transform, (With<MainCamera>, Without<Particle>)>,
    mut q: Query<(&mut Particle, &mut Transform, &mut Visibility, &mut MeshMaterial3d<StandardMaterial>)>,
) {
    let dt = time.delta_secs();
    let cam_rot = cam.get_single().map(|t| t.rotation).unwrap_or_default();
    for (mut p, mut tf, mut vis, mut mat) in &mut q {
        if !p.active {
            continue;
        }
        p.age += dt;
        if p.age >= p.life {
            p.active = false;
            *vis = Visibility::Hidden;
            continue;
        }
        let t = p.age / p.life;
        let drag = p.drag;
        let g = p.gravity;
        p.vel.y -= g * dt;
        let v = p.vel;
        p.vel -= v * drag.min(10.0) * dt;
        tf.translation += p.vel * dt;
        if p.kind == PKind::Debris {
            let ground = hm.surface(tf.translation.x, tf.translation.z);
            if tf.translation.y < ground {
                tf.translation.y = ground;
                p.vel = Vec3::ZERO;
                p.spin = Vec3::ZERO;
            }
            tf.rotation *= Quat::from_scaled_axis(p.spin * dt);
        } else {
            tf.rotation = cam_rot;
        }
        let size = p.size0 + (p.size1 - p.size0) * (1.0 - (1.0 - t).powi(2));
        tf.scale = Vec3::splat(size.max(0.01));
        let step = ((t * ALPHA_STEPS as f32) as usize).min(ALPHA_STEPS - 1);
        if step != p.step {
            p.step = step;
            mat.0 = assets.mats[kind_index(p.kind)][step].clone();
        }
    }
}

fn update_flashes(time: Res<Time>, mut q: Query<(&mut Flash, &mut PointLight)>) {
    let dt = time.delta_secs();
    for (mut f, mut light) in &mut q {
        if f.age > 1.0 {
            continue;
        }
        f.age += dt;
        let k = (1.0 - f.age / 0.5).max(0.0);
        light.intensity = f.intensity * k * k;
    }
}

fn camera_shake(
    time: Res<Time>,
    mut shake: ResMut<Shake>,
    view: Option<Res<State<ViewMode>>>,
    mut cam: Query<&mut Transform, With<MainCamera>>,
) {
    let dt = time.delta_secs();
    shake.trauma = (shake.trauma - SHAKE_DECAY * dt).max(0.0);
    let Some(view) = view else {
        return;
    };
    // On foot the camera is right on the ground: shake much less.
    let amp = match view.get() {
        ViewMode::Flight | ViewMode::DeathCam => 1.0,
        ViewMode::Infantry => 0.3,
        ViewMode::Map => return,
    };
    if shake.trauma <= 0.0 {
        return;
    }
    let Ok(mut tf) = cam.get_single_mut() else {
        return;
    };
    let mut rng = rand::thread_rng();
    let s = shake.trauma * shake.trauma * amp;
    let offset = rand_dir(&mut rng) * s * 2.0;
    tf.translation += offset;
    tf.rotation *= Quat::from_euler(
        EulerRot::YXZ,
        rng.gen_range(-1.0..1.0) * s * 0.03,
        rng.gen_range(-1.0..1.0) * s * 0.03,
        rng.gen_range(-1.0..1.0) * s * 0.05,
    );
}

fn clear_pool(mut q: Query<(&mut Particle, &mut Visibility)>, mut particles: ResMut<Particles>) {
    particles.queue.clear();
    particles.flashes.clear();
    for (mut p, mut vis) in &mut q {
        p.active = false;
        *vis = Visibility::Hidden;
    }
}
