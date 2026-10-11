//! Sounds (§8): engine hum (pitch by throttle), gun, missile launch,
//! explosion, lock and missile-warning tones. All synthesised at startup
//! into in-memory WAVs, so there are no audio files to ship.

use bevy::audio::{AudioSinkPlayback, Volume};
use bevy::prelude::*;
use rand::Rng;

use crate::flight::FlightState;
use crate::player::Player;
use crate::state::*;
use crate::weapons::{Boom, Detonation, LockState, Missile};
use crate::flight::ControlInput;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sfx {
    GunBurst,
    MissileLaunch,
    Hit,
}

#[derive(Event, Clone, Copy, Debug)]
pub struct SoundEvent {
    pub sfx: Sfx,
    /// None = not positional (UI / cockpit).
    pub pos: Option<Vec3>,
}

impl SoundEvent {
    pub fn at(sfx: Sfx, pos: Vec3) -> Self {
        Self { sfx, pos: Some(pos) }
    }
    pub fn ui(sfx: Sfx) -> Self {
        Self { sfx, pos: None }
    }
}

#[derive(Resource)]
struct Sounds {
    gun_burst: Handle<AudioSource>,
    missile: Handle<AudioSource>,
    explosion: Handle<AudioSource>,
    hit: Handle<AudioSource>,
}

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum Loop {
    Engine,
    Gun,
    Lock,
    Seek,
    Warning,
}

const RATE: u32 = 22_050;

fn wav(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32_000.0) as i16).to_le_bytes());
    }
    out
}

fn synth(secs: f32, f: impl Fn(f32, &mut rand::rngs::ThreadRng) -> f32) -> Vec<f32> {
    let n = (secs * RATE as f32) as usize;
    let mut rng = rand::thread_rng();
    (0..n).map(|i| f(i as f32 / RATE as f32, &mut rng)).collect()
}

/// One-pole low-pass, in place.
fn lowpass(s: &mut [f32], k: f32) {
    let mut y = 0.0;
    for v in s.iter_mut() {
        y += (*v - y) * k;
        *v = y;
    }
}

fn add(sources: &mut Assets<AudioSource>, samples: Vec<f32>) -> Handle<AudioSource> {
    sources.add(AudioSource { bytes: wav(&samples).into() })
}

pub struct SoundPlugin;

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<SoundEvent>()
            .add_systems(Startup, setup_sounds)
            .add_systems(Update, (play_one_shots, update_loops));
    }
}

fn setup_sounds(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
    use std::f32::consts::TAU;
    // Engine: 2 s loop, integer cycles so it loops cleanly.
    let mut engine = synth(2.0, |t, r| {
        0.35 * (TAU * 55.0 * t).sin() + 0.2 * ((TAU * 110.0 * t).sin()).signum() * 0.5 + 0.5 * r.gen_range(-1.0..1.0)
    });
    lowpass(&mut engine, 0.08);
    for v in &mut engine {
        *v *= 1.6;
    }
    // Gun: 25 rounds/s rattle, 0.4 s loop.
    let mut gun = synth(0.4, |t, r| {
        let ph = (t * 25.0).fract();
        (-ph * 40.0).exp() * r.gen_range(-1.0..1.0) + (-ph * 20.0).exp() * (TAU * 90.0 * t).sin() * 0.6
    });
    lowpass(&mut gun, 0.35);
    let mut burst = synth(0.3, |t, r| {
        let ph = (t * 25.0).fract();
        (-ph * 40.0).exp() * r.gen_range(-1.0..1.0) * (1.0 - t / 0.3)
    });
    lowpass(&mut burst, 0.3);
    let mut missile = synth(1.6, |t, r| {
        let env = (t * 20.0).min(1.0) * (-t * 1.6).exp();
        env * r.gen_range(-1.0..1.0)
    });
    lowpass(&mut missile, 0.25);
    let mut explosion = synth(2.8, |t, r| {
        let env = (-t * 2.2).exp();
        let crack = (-t * 40.0).exp();
        env * r.gen_range(-1.0..1.0) + crack * r.gen_range(-1.0..1.0)
    });
    lowpass(&mut explosion, 0.06);
    for v in &mut explosion {
        *v *= 3.0;
    }
    let hit = synth(0.15, |t, r| (-t * 30.0).exp() * (0.6 * (TAU * 300.0 * t).sin() + 0.4 * r.gen_range(-1.0..1.0)));
    let lock = synth(1.0, |t, _| 0.35 * (TAU * 1_100.0 * t).sin());
    let seek = synth(1.0, |t, _| 0.3 * (TAU * 520.0 * t).sin() * (0.6 + 0.4 * (TAU * 12.0 * t).sin()));
    let warning = synth(1.0, |t, _| {
        let on = (t * 8.0).fract() < 0.5;
        if on { 0.4 * (TAU * 880.0 * t).sin() } else { 0.0 }
    });

    commands.insert_resource(Sounds {
        gun_burst: add(&mut sources, burst),
        missile: add(&mut sources, missile),
        explosion: add(&mut sources, explosion),
        hit: add(&mut sources, hit),
    });
    for (kind, samples) in [
        (Loop::Engine, engine),
        (Loop::Gun, gun),
        (Loop::Lock, lock),
        (Loop::Seek, seek),
        (Loop::Warning, warning),
    ] {
        commands.spawn((
            kind,
            AudioPlayer(add(&mut sources, samples)),
            PlaybackSettings::LOOP.with_volume(Volume::new(0.0)),
        ));
    }
}

fn play_one_shots(
    mut commands: Commands,
    sounds: Res<Sounds>,
    mut events: EventReader<SoundEvent>,
    mut detonations: EventReader<Detonation>,
    cam: Query<&Transform, With<MainCamera>>,
    play: Option<Res<State<PlayState>>>,
) {
    let paused = play.is_some_and(|p| *p.get() == PlayState::Paused);
    let cam = cam.get_single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    let mut queue: Vec<(Handle<AudioSource>, f32, f32)> = Vec::new();
    for ev in events.read() {
        let (h, vol, falloff) = match ev.sfx {
            Sfx::GunBurst => (&sounds.gun_burst, 0.5, 400.0),
            Sfx::MissileLaunch => (&sounds.missile, 0.8, 500.0),
            Sfx::Hit => (&sounds.hit, 0.8, 1.0),
        };
        let v = match ev.pos {
            Some(p) => vol / (1.0 + p.distance(cam) / falloff),
            None => vol,
        };
        queue.push((h.clone(), v, 1.0));
    }
    for d in detonations.read() {
        let (falloff, speed) = match d.size {
            Boom::Spark => continue,
            Boom::Medium => (900.0, 1.1),
            Boom::Large => (1_800.0, 0.75),
            Boom::Huge => (12_000.0, 0.45),
        };
        let v = 1.0 / (1.0 + d.pos.distance(cam) / falloff);
        queue.push((sounds.explosion.clone(), v, speed));
    }
    if paused {
        return;
    }
    for (h, v, speed) in queue {
        if v < 0.04 {
            continue;
        }
        commands.spawn((
            AudioPlayer(h),
            PlaybackSettings::DESPAWN.with_volume(Volume::new(v)).with_speed(speed),
        ));
    }
}

#[allow(clippy::type_complexity)]
fn update_loops(
    app: Res<State<AppState>>,
    play: Option<Res<State<PlayState>>>,
    view: Option<Res<State<ViewMode>>>,
    player: Query<(Entity, &FlightState, &ControlInput, &LockState), With<Player>>,
    missiles: Query<&Missile>,
    sinks: Query<(&Loop, &AudioSink)>,
) {
    let active = *app.get() == AppState::Playing
        && play.is_some_and(|p| *p.get() == PlayState::Running)
        && view.is_some_and(|v| *v.get() == ViewMode::Flight);
    let p = player.get_single().ok();
    for (kind, sink) in &sinks {
        let (vol, speed) = match (active, p) {
            (true, Some((me, st, input, lock))) => match kind {
                Loop::Engine => {
                    if st.engine_out {
                        (0.0, 1.0)
                    } else {
                        (0.25 + st.throttle * 0.35, 0.6 + st.throttle * 0.9)
                    }
                }
                Loop::Gun => (if input.fire_gun { 0.45 } else { 0.0 }, 1.0),
                Loop::Lock => (if lock.locked { 0.25 } else { 0.0 }, 1.0),
                Loop::Seek => (if lock.candidate.is_some() && !lock.locked { 0.2 } else { 0.0 }, 1.0),
                Loop::Warning => (if missiles.iter().any(|m| m.target == Some(me)) { 0.35 } else { 0.0 }, 1.0),
            },
            _ => (0.0, 1.0),
        };
        sink.set_volume(vol);
        sink.set_speed(speed);
    }
}
