//! War Game — basic prototype. See Docs/architect.md and Docs/plan.md.

mod audio;
mod config;
mod debug;
mod flight;
mod fx;
mod game_rules;
mod geo;
mod ground;
mod heightgen;
mod hud;
mod infantry;
mod infrastructure;
mod map;
mod nav;
mod pilot_ai;
mod player;
mod state;
mod weapons;
mod warheads;
mod world;

use bevy::prelude::*;
use bevy::window::PresentMode;
use bevy_egui::EguiPlugin;

fn main() {
    let sky = config::SKY_COLOR;
    App::new()
        .insert_resource(ClearColor(Color::srgb(sky[0], sky[1], sky[2])))
        .insert_resource(Time::<Fixed>::from_hz(config::FIXED_HZ))
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "War Game".into(),
                resolution: (1600.0, 900.0).into(),
                present_mode: PresentMode::AutoVsync,
                ..default()
            }),
            ..default()
        }))
        .add_plugins(EguiPlugin)
        .add_plugins((
            state::GameStatePlugin,
            world::WorldPlugin,
            flight::FlightPlugin,
            weapons::WeaponsPlugin,
            pilot_ai::PilotAiPlugin,
            ground::GroundPlugin,
            infrastructure::InfrastructurePlugin,
            infantry::InfantryPlugin,
            map::MapPlugin,
            player::PlayerPlugin,
            hud::HudPlugin,
            fx::FxPlugin,
            audio::SoundPlugin,
            game_rules::GameRulesPlugin,
            debug::DebugToolsPlugin,
        ))
        .add_plugins(warheads::WarheadsPlugin)
        .run();
}
