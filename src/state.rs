//! Game states (Menu → Playing → GameOver), pause, view modes.

use bevy::prelude::*;

#[derive(States, Default, Debug, Clone, PartialEq, Eq, Hash)]
pub enum AppState {
    #[default]
    Menu,
    Playing,
    GameOver,
}

/// Esc toggles this while playing. Pausing stops virtual time, which
/// freezes every gameplay system (Update dt = 0, FixedUpdate stops).
#[derive(SubStates, Default, Debug, Clone, PartialEq, Eq, Hash)]
#[source(AppState = AppState::Playing)]
pub enum PlayState {
    #[default]
    Running,
    Paused,
}

#[derive(SubStates, Default, Debug, Clone, PartialEq, Eq, Hash)]
#[source(AppState = AppState::Playing)]
pub enum ViewMode {
    #[default]
    Flight,
    Map,
    DeathCam,
    /// On foot with a NATO infantry squad (§6.5).
    Infantry,
}

/// Everything that simulates the war. Only runs while Playing.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct GameplaySet;

/// Camera systems write the full camera transform in `Position`;
/// screen shake adds an offset on top in `Shake`.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum CameraSet {
    Position,
    Shake,
}

/// The single 3D camera, reused by every view.
#[derive(Component)]
pub struct MainCamera;

/// While the debug free-fly camera is on, other camera rigs and player
/// flight input stand down.
#[derive(Resource, Default)]
pub struct DebugCamActive(pub bool);

pub fn debug_cam_off(d: Res<DebugCamActive>) -> bool {
    !d.0
}

pub struct GameStatePlugin;

impl Plugin for GameStatePlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<AppState>()
            .add_sub_state::<PlayState>()
            .add_sub_state::<ViewMode>()
            .enable_state_scoped_entities::<AppState>()
            .init_resource::<DebugCamActive>()
            .configure_sets(Update, GameplaySet.run_if(in_state(AppState::Playing)))
            .configure_sets(FixedUpdate, GameplaySet.run_if(in_state(AppState::Playing)))
            .configure_sets(Update, (CameraSet::Position, CameraSet::Shake).chain())
            .add_systems(Update, toggle_pause.run_if(in_state(AppState::Playing)))
            .add_systems(OnEnter(PlayState::Paused), |mut t: ResMut<Time<Virtual>>| t.pause())
            .add_systems(OnExit(PlayState::Paused), |mut t: ResMut<Time<Virtual>>| t.unpause());
    }
}

fn toggle_pause(
    keys: Res<ButtonInput<KeyCode>>,
    state: Res<State<PlayState>>,
    mut next: ResMut<NextState<PlayState>>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        next.set(match state.get() {
            PlayState::Running => PlayState::Paused,
            PlayState::Paused => PlayState::Running,
        });
    }
}
