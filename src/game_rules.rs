//! Capture, win/lose (§6.6), round stats, event log, and the menu,
//! pause and end screens.

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use crate::config::*;
use crate::ground::GroundUnit;
use crate::player::{JetReserve, PlayerStatus};
use crate::state::*;
use crate::weapons::JetDestroyed;

/// Short running log shown in the map side panel.
#[derive(Resource, Default)]
pub struct EventLog {
    pub lines: Vec<String>,
    now: f32,
}

impl EventLog {
    pub fn push(&mut self, msg: impl Into<String>) {
        let t = self.now;
        self.lines.push(format!("[{:02}:{:02}] {}", (t / 60.0) as u32, (t % 60.0) as u32, msg.into()));
        if self.lines.len() > 40 {
            self.lines.remove(0);
        }
    }
}

#[derive(Resource, Default, Debug)]
pub struct RoundStats {
    pub time: f32,
    pub air_kills: u32,
    pub enemy_jets_downed: u32,
    pub jets_lost: u32,
    pub friendly_jets_lost: u32,
    pub enemy_units_destroyed: u32,
    pub nato_units_lost: u32,
    pub enemy_sites_destroyed: u32,
    pub nato_sites_lost: u32,
}

/// Seconds each base has been held by the attacker without defenders.
#[derive(Resource, Default, Debug)]
pub struct CaptureState {
    pub nato_base: f32,
    pub enemy_base: f32,
}

#[derive(Resource, Default, Debug)]
pub struct Outcome {
    pub victory: bool,
    pub reason: String,
}

pub struct GameRulesPlugin;

impl Plugin for GameRulesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EventLog>()
            .init_resource::<RoundStats>()
            .init_resource::<CaptureState>()
            .init_resource::<Outcome>()
            .add_systems(OnEnter(AppState::Playing), reset_round)
            .add_systems(Update, (tick, count_losses, capture, check_outcome).chain().in_set(GameplaySet))
            .add_systems(Update, main_menu.run_if(in_state(AppState::Menu)))
            .add_systems(Update, pause_menu.run_if(in_state(PlayState::Paused)))
            .add_systems(Update, end_screen.run_if(in_state(AppState::GameOver)));
    }
}

fn reset_round(mut commands: Commands) {
    commands.insert_resource(EventLog::default());
    commands.insert_resource(RoundStats::default());
    commands.insert_resource(CaptureState::default());
    commands.insert_resource(Outcome::default());
}

fn tick(time: Res<Time>, mut stats: ResMut<RoundStats>, mut log: ResMut<EventLog>) {
    stats.time += time.delta_secs();
    log.now = stats.time;
}

fn count_losses(mut events: EventReader<JetDestroyed>, mut stats: ResMut<RoundStats>, mut log: ResMut<EventLog>) {
    for ev in events.read() {
        match ev.side {
            Side::Enemy => {
                stats.enemy_jets_downed += 1;
                if ev.by_player {
                    stats.air_kills += 1;
                    log.push("Splash! You shot down an enemy jet");
                } else {
                    log.push("Enemy jet shot down");
                }
            }
            Side::Nato if ev.was_player => stats.jets_lost += 1,
            Side::Nato => {
                stats.friendly_jets_lost += 1;
                log.push("Friendly jet lost");
            }
        }
    }
}

fn capture(
    time: Res<Time>,
    units: Query<(&GroundUnit, &Transform)>,
    mut cap: ResMut<CaptureState>,
    mut log: ResMut<EventLog>,
) {
    let dt = time.delta_secs();
    for base_side in [Side::Nato, Side::Enemy] {
        let base = base_side.base();
        let in_zone = |side: Side| {
            units.iter().any(|(u, tf)| {
                u.side == side
                    && !u.retreating
                    && Vec2::new(tf.translation.x - base.center[0], tf.translation.z - base.center[1]).length() < base.zone_radius
            })
        };
        let attackers = in_zone(base_side.other());
        let defenders = in_zone(base_side);
        let timer = match base_side {
            Side::Nato => &mut cap.nato_base,
            Side::Enemy => &mut cap.enemy_base,
        };
        let before = *timer;
        if attackers && !defenders {
            *timer += dt;
        } else {
            *timer = (*timer - dt).max(0.0);
        }
        if before == 0.0 && *timer > 0.0 {
            log.push(match base_side {
                Side::Nato => "Enemy troops are taking our airbase!",
                Side::Enemy => "Our troops are capturing the enemy airbase",
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_outcome(
    cap: Res<CaptureState>,
    units: Query<&GroundUnit>,
    reserve: Res<JetReserve>,
    status: Res<PlayerStatus>,
    view: Res<State<ViewMode>>,
    mut outcome: ResMut<Outcome>,
    mut next: ResMut<NextState<AppState>>,
) {
    let enemy_left = units.iter().any(|u| u.side == Side::Enemy && !u.retreating);
    let result = if cap.enemy_base >= CAPTURE_TIME {
        Some((true, "You captured the enemy airbase."))
    } else if !enemy_left {
        Some((true, "All enemy ground units are destroyed or routed."))
    } else if cap.nato_base >= CAPTURE_TIME {
        Some((false, "The enemy captured your airbase."))
    } else if reserve.remaining == 0 && !status.flying && *view.get() != ViewMode::DeathCam {
        Some((false, "You ran out of jets."))
    } else {
        None
    };
    if let Some((victory, reason)) = result {
        *outcome = Outcome { victory, reason: reason.into() };
        next.set(AppState::GameOver);
    }
}

pub const CONTROLS: &[(&str, &str)] = &[
    ("W / S", "Pitch down / up"),
    ("A / D", "Roll left / right"),
    ("Q / E", "Yaw left / right"),
    ("Shift / Ctrl", "Throttle up / down (>90% = afterburner)"),
    ("Space / LMB", "Guns"),
    ("RMB / R", "Missile (locks after 1 s in the seeker circle)"),
    ("X", "Flares"),
    ("G / B", "Gear / airbrake (+wheel brakes)"),
    ("V / C / mouse", "Chase-cockpit view / recenter / free-look"),
    ("M", "Command map (autopilot holds level flight)"),
    ("Map: LMB / RMB", "Select / order a unit (it returns to auto 30 s after arriving)"),
    ("Map: Launch warhead", "Then LMB on the map to pick the impact point"),
    ("I (map)", "Fight on foot with the selected infantry squad"),
    ("On foot: WASD / Shift / C", "Move / sprint / crouch (crouched + still hides you from drones)"),
    ("On foot: LMB / RMB / R", "Rifle / aim / reload (resupply near friendly units)"),
    ("On foot: T", "IR drone: enemies glow, WASD + Q/E fly, LMB drop bomb, T return"),
    ("Esc", "Pause"),
    ("F1", "Debug free camera (F2 debug panel)"),
];

fn controls_grid(ui: &mut egui::Ui) {
    egui::Grid::new("controls").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
        for (k, v) in CONTROLS {
            ui.strong(*k);
            ui.label(*v);
            ui.end_row();
        }
    });
}

fn main_menu(
    mut contexts: EguiContexts,
    mut next: ResMut<NextState<AppState>>,
    mut exit: EventWriter<AppExit>,
    mut show_controls: Local<bool>,
) {
    egui::Window::new("WAR GAME")
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .show(contexts.ctx_mut(), |ui| {
            ui.vertical_centered(|ui| {
                ui.label("Europe, 203X. You command NATO forces — and fly the jet.");
                ui.add_space(10.0);
                if ui.add(egui::Button::new(egui::RichText::new("Start").size(22.0)).min_size([200.0, 36.0].into())).clicked() {
                    next.set(AppState::Playing);
                }
                if ui.add(egui::Button::new("Controls").min_size([200.0, 28.0].into())).clicked() {
                    *show_controls = !*show_controls;
                }
                if ui.add(egui::Button::new("Quit").min_size([200.0, 28.0].into())).clicked() {
                    exit.send(AppExit::Success);
                }
            });
            if *show_controls {
                ui.separator();
                controls_grid(ui);
                ui.separator();
                ui.small("Take off from the NATO runway in Germany, stop on it to restock. Your troops advance on their own; blow bridges to cut enemy routes.");
                ui.small("Fire ballistic warheads from America at enemy concentrations; dodge the enemy's from Siberia.");
                ui.small("Bomb enemy power plants, fuel depots and radars; defend your own from strike raids.");
                ui.small("Win: destroy or rout every enemy ground unit, or capture the enemy airbase.");
                ui.small("Lose: the enemy captures your airbase, or you run out of jets.");
            }
        });
}

fn pause_menu(
    mut contexts: EguiContexts,
    mut play: ResMut<NextState<PlayState>>,
    mut app: ResMut<NextState<AppState>>,
    mut exit: EventWriter<AppExit>,
) {
    egui::Window::new("Paused")
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .show(contexts.ctx_mut(), |ui| {
            ui.vertical_centered(|ui| {
                if ui.add(egui::Button::new("Resume (Esc)").min_size([180.0, 30.0].into())).clicked() {
                    play.set(PlayState::Running);
                }
                if ui.add(egui::Button::new("Abandon round").min_size([180.0, 30.0].into())).clicked() {
                    app.set(AppState::Menu);
                }
                if ui.add(egui::Button::new("Quit").min_size([180.0, 30.0].into())).clicked() {
                    exit.send(AppExit::Success);
                }
            });
            ui.separator();
            controls_grid(ui);
        });
}

fn end_screen(
    mut contexts: EguiContexts,
    outcome: Res<Outcome>,
    stats: Res<RoundStats>,
    mut next: ResMut<NextState<AppState>>,
    mut exit: EventWriter<AppExit>,
) {
    let title = if outcome.victory { "VICTORY" } else { "DEFEAT" };
    egui::Window::new(title)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .show(contexts.ctx_mut(), |ui| {
            let color = if outcome.victory { egui::Color32::GREEN } else { egui::Color32::RED };
            ui.colored_label(color, egui::RichText::new(&outcome.reason).size(18.0));
            ui.separator();
            egui::Grid::new("stats").num_columns(2).show(ui, |ui| {
                let t = stats.time;
                for (k, v) in [
                    ("Time", format!("{:02}:{:02}", (t / 60.0) as u32, (t % 60.0) as u32)),
                    ("Your air kills", stats.air_kills.to_string()),
                    ("Enemy jets downed (total)", stats.enemy_jets_downed.to_string()),
                    ("Enemy ground units destroyed", stats.enemy_units_destroyed.to_string()),
                    ("Your jets lost", stats.jets_lost.to_string()),
                    ("Friendly AI jets lost", stats.friendly_jets_lost.to_string()),
                    ("NATO ground units lost", stats.nato_units_lost.to_string()),
                    ("Enemy infrastructure destroyed", stats.enemy_sites_destroyed.to_string()),
                    ("NATO infrastructure lost", stats.nato_sites_lost.to_string()),
                ] {
                    ui.label(k);
                    ui.strong(v);
                    ui.end_row();
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Main menu").clicked() {
                    next.set(AppState::Menu);
                }
                if ui.button("Play again").clicked() {
                    next.set(AppState::Playing);
                }
                if ui.button("Quit").clicked() {
                    exit.send(AppExit::Success);
                }
            });
        });
}
