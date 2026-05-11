//! Adventure switcher — a small button in the top-right of the HUD
//! that lets a player jump to another adventure. Clicking opens a
//! list of registered adventures (fetched from `/adventures`),
//! confirming the switch via `window.confirm` and then reloading the
//! page so the world rebuilds from scratch under the new id.
//!
//! Boons survive the switch; level / gold / inventory reset (the
//! server side of that lives in `POST /start_new_adventure`).

use bevy::prelude::*;
use wasm_bindgen::JsValue;

use crate::states::AppState;
use crate::terrain::tilemap::MyPlayerState;
use crate::{api_url, GameFont, GameSession};

pub struct AdventureMenuPlugin;

impl Plugin for AdventureMenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AdventureMenuState>()
            .add_systems(OnEnter(AppState::InGame), spawn_button)
            .add_systems(
                Update,
                (handle_button_click, handle_choice_click)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Resource, Default)]
struct AdventureMenuState {
    panel_open: bool,
}

#[derive(Component)]
struct AdventureMenuButton;

#[derive(Component)]
struct AdventureMenuPanel;

#[derive(Component)]
struct AdventureChoiceButton(String); // adventure_id to switch to

fn spawn_button(mut commands: Commands, font: Res<GameFont>) {
    commands
        .spawn((
            Button,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(34.0),
                right: Val::Px(8.0),
                padding: UiRect::axes(Val::Px(8.0), Val::Px(3.0)),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.10, 0.07, 0.03, 0.95)),
            BorderColor(Color::srgb(0.85, 0.65, 0.20)),
            BorderRadius::all(Val::Px(3.0)),
            ZIndex(15),
            AdventureMenuButton,
        ))
        .with_children(|btn| {
            btn.spawn((
                Text::new("Adventure"),
                TextFont {
                    font: font.0.clone(),
                    font_size: 9.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 0.92, 0.55)),
            ));
        });
}

fn handle_button_click(
    mut commands: Commands,
    mut state: ResMut<AdventureMenuState>,
    player: Res<MyPlayerState>,
    font: Res<GameFont>,
    button_q: Query<&Interaction, (Changed<Interaction>, With<AdventureMenuButton>)>,
    panel_q: Query<Entity, With<AdventureMenuPanel>>,
) {
    for interaction in &button_q {
        if !matches!(interaction, Interaction::Pressed) {
            continue;
        }
        // Toggle.
        if state.panel_open {
            for e in &panel_q {
                commands.entity(e).despawn_recursive();
            }
            state.panel_open = false;
            return;
        }
        state.panel_open = true;
        // Panel listing the two registered adventures. Hard-coded
        // to match `gamemaster::adventure::presets()` for now —
        // fetching from `/adventures` is a small future task once
        // we want to author more chapters from JSON.
        commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(60.0),
                    right: Val::Px(8.0),
                    width: Val::Px(220.0),
                    padding: UiRect::all(Val::Px(8.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(6.0),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.05, 0.04, 0.02, 0.97)),
                BorderColor(Color::srgb(0.85, 0.65, 0.20)),
                BorderRadius::all(Val::Px(4.0)),
                ZIndex(45),
                AdventureMenuPanel,
            ))
            .with_children(|panel| {
                panel.spawn((
                    Text::new("Switch Adventure"),
                    TextFont {
                        font: font.0.clone(),
                        font_size: 10.0,
                        ..default()
                    },
                    TextColor(Color::srgb(1.0, 0.92, 0.55)),
                ));
                panel.spawn((
                    Text::new("Warning: resets your character. Boons are kept."),
                    TextFont {
                        font: font.0.clone(),
                        font_size: 8.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.85, 0.55, 0.55)),
                ));
                let entries: &[(&str, &str)] = &[
                    ("frost_quest", "The Frost Lord"),
                    ("chaos", "Chaos Unleashed"),
                ];
                let current = player_current_adventure(&player).to_string();
                for (id, name) in entries {
                    let is_current = *id == current;
                    let label = if is_current {
                        format!("• {} (current)", name)
                    } else {
                        format!("Start: {}", name)
                    };
                    let bg = if is_current {
                        Color::srgba(0.15, 0.13, 0.08, 0.7)
                    } else {
                        Color::srgba(0.10, 0.18, 0.08, 0.9)
                    };
                    panel
                        .spawn((
                            Button,
                            Node {
                                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                                border: UiRect::all(Val::Px(1.0)),
                                ..default()
                            },
                            BackgroundColor(bg),
                            BorderColor(Color::srgb(0.65, 0.55, 0.20)),
                            BorderRadius::all(Val::Px(3.0)),
                            AdventureChoiceButton((*id).to_string()),
                        ))
                        .with_children(|btn| {
                            btn.spawn((
                                Text::new(label),
                                TextFont {
                                    font: font.0.clone(),
                                    font_size: 9.0,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.95, 0.92, 0.78)),
                            ));
                        });
                }
            });
    }
}

/// Pulls the player's current adventure_id (mirrored from server via
/// polling). Falls back to "frost_quest" if it hasn't propagated yet
/// — first frame after enter-game.
fn player_current_adventure(player: &MyPlayerState) -> &str {
    if player.adventure_id.is_empty() {
        "frost_quest"
    } else {
        &player.adventure_id
    }
}

fn handle_choice_click(
    session: Res<GameSession>,
    interactions: Query<(&Interaction, &AdventureChoiceButton), Changed<Interaction>>,
) {
    for (interaction, choice) in &interactions {
        if !matches!(interaction, Interaction::Pressed) {
            continue;
        }
        // Browser confirm — slightly intrusive, but it makes the
        // "resets your character" warning hard to miss. Acceptable
        // for an MVP; a custom in-game confirm dialog is a polish task.
        let window = match web_sys::window() {
            Some(w) => w,
            None => continue,
        };
        let prompt = format!(
            "Switch to '{}'?\nYour current character will be reset (boons are kept).",
            choice.0
        );
        let confirmed = window.confirm_with_message(&prompt).unwrap_or(false);
        if !confirmed {
            continue;
        }
        let player_id = session.player_id.clone();
        let adventure_id = choice.0.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let url = api_url("/start_new_adventure");
            let body = format!(
                r#"{{"player_id":"{}","adventure_id":"{}"}}"#,
                player_id, adventure_id
            );
            let result = reqwest::Client::new()
                .post(&url)
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await;
            match result {
                Ok(resp) if resp.status().is_success() => {
                    web_sys::console::log_1(&JsValue::from_str(&format!(
                        "[adventure] switched to '{}'; reloading",
                        adventure_id
                    )));
                    if let Some(w) = web_sys::window() {
                        let _ = w.location().reload();
                    }
                }
                Ok(resp) => {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "[adventure] /start_new_adventure HTTP {}",
                        resp.status()
                    )));
                }
                Err(e) => {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "[adventure] /start_new_adventure error: {}",
                        e
                    )));
                }
            }
        });
    }
}
