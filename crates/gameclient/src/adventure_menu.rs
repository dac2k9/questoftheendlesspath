//! Adventure switcher — a small button in the top-right of the HUD
//! that lets a player advance to their next adventure. **One-way
//! trip:** the button only appears once the player has completed
//! their current adventure's `completion_event_id` and there's a
//! not-yet-completed adventure waiting. Otherwise the button is
//! hidden — you can't shortcut back to an easier arc or restart
//! one you haven't finished.
//!
//! Source of truth is `GET /adventures?player_id=X`, polled every
//! 5 s. The endpoint returns the list of adventures the player is
//! eligible to advance INTO (already filters by completion); the
//! client just renders that list.
//!
//! Boons survive the switch; level / gold / inventory reset (the
//! server side of that lives in `POST /start_new_adventure`).

use bevy::prelude::*;
use std::sync::{Arc, Mutex};
use wasm_bindgen::JsValue;

use crate::states::AppState;
use crate::{api_url, GameFont, GameSession};

pub struct AdventureMenuPlugin;

impl Plugin for AdventureMenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AdventureMenuState>()
            .add_systems(OnEnter(AppState::InGame), spawn_button)
            .add_systems(
                Update,
                (
                    tick_availability_poll,
                    apply_availability,
                    sync_button_visibility,
                    handle_button_click,
                    handle_choice_click,
                )
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Clone)]
struct AvailableAdventure {
    id: String,
    display_name: String,
}

#[derive(Resource, Default)]
struct AdventureMenuState {
    panel_open: bool,
    available: Vec<AvailableAdventure>,
    /// Mutex slot the async fetcher fills with the most recent
    /// response. The Update system drains it into `available`.
    fetch_slot: Arc<Mutex<Option<Vec<AvailableAdventure>>>>,
    poll_timer: f32,
    /// Render visibility tracks `available.is_empty()` so we can
    /// react when the list flips from empty → non-empty (e.g. the
    /// player just completed the final boss).
    button_visible: bool,
    /// Wait one poll before showing the button on enter-game so we
    /// don't flash it before /adventures has answered.
    initial_poll_done: bool,
}

#[derive(Component)]
struct AdventureMenuButton;

#[derive(Component)]
struct AdventureMenuPanel;

#[derive(Component)]
struct AdventureChoiceButton(String); // adventure_id to switch to

fn spawn_button(mut commands: Commands, mut state: ResMut<AdventureMenuState>, font: Res<GameFont>) {
    // Always spawn the button entity; `sync_button_visibility`
    // flips its Visibility based on `available.is_empty()`. Cheaper
    // than spawning/despawning on every poll.
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
            Visibility::Hidden,
            AdventureMenuButton,
        ))
        .with_children(|btn| {
            btn.spawn((
                Text::new("Next Adventure"),
                TextFont {
                    font: font.0.clone(),
                    font_size: 9.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 0.92, 0.55)),
            ));
        });
    // Fire the first poll immediately on enter-game; don't wait the
    // 5 s tick.
    state.poll_timer = 100.0;
}

/// Tick a 5 s poll timer and kick off a fetch when it elapses.
fn tick_availability_poll(
    time: Res<Time>,
    session: Res<GameSession>,
    mut state: ResMut<AdventureMenuState>,
) {
    if session.player_id.is_empty() {
        return;
    }
    state.poll_timer += time.delta_secs();
    if state.poll_timer < 5.0 {
        return;
    }
    state.poll_timer = 0.0;
    kick_off_fetch(session.player_id.clone(), state.fetch_slot.clone());
}

fn kick_off_fetch(player_id: String, slot: Arc<Mutex<Option<Vec<AvailableAdventure>>>>) {
    wasm_bindgen_futures::spawn_local(async move {
        let url = api_url(&format!("/adventures?player_id={}", player_id));
        let Ok(resp) = reqwest::Client::new().get(&url).send().await else { return };
        let Ok(text) = resp.text().await else { return };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        let mut parsed = Vec::new();
        if let Some(arr) = json.get("available").and_then(|v| v.as_array()) {
            for entry in arr {
                let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = entry.get("display_name").and_then(|v| v.as_str()).unwrap_or("");
                if !id.is_empty() && !name.is_empty() {
                    parsed.push(AvailableAdventure {
                        id: id.to_string(),
                        display_name: name.to_string(),
                    });
                }
            }
        }
        if let Ok(mut g) = slot.lock() {
            *g = Some(parsed);
        }
    });
}

/// Drain the async fetch's result into the resource so the
/// rendering systems can read it from a regular Bevy `Res`.
fn apply_availability(mut state: ResMut<AdventureMenuState>) {
    let new_list = {
        let Ok(mut g) = state.fetch_slot.lock() else { return };
        g.take()
    };
    if let Some(list) = new_list {
        state.available = list;
        state.initial_poll_done = true;
    }
}

/// Show the button only when `available` is non-empty AND we've
/// already received at least one poll response (avoids a
/// one-frame flash of the button before /adventures replies on
/// game enter).
fn sync_button_visibility(
    mut state: ResMut<AdventureMenuState>,
    mut button_q: Query<&mut Visibility, With<AdventureMenuButton>>,
    mut commands: Commands,
    panel_q: Query<Entity, With<AdventureMenuPanel>>,
) {
    let want_visible = state.initial_poll_done && !state.available.is_empty();
    if want_visible == state.button_visible {
        return;
    }
    state.button_visible = want_visible;
    for mut vis in &mut button_q {
        *vis = if want_visible { Visibility::Visible } else { Visibility::Hidden };
    }
    // Also close the panel if the button just vanished — happens
    // when the player started the listed adventure (next poll
    // returns empty because they're now in it).
    if !want_visible && state.panel_open {
        for e in &panel_q {
            commands.entity(e).despawn_recursive();
        }
        state.panel_open = false;
    }
}

fn handle_button_click(
    mut commands: Commands,
    mut state: ResMut<AdventureMenuState>,
    font: Res<GameFont>,
    button_q: Query<&Interaction, (Changed<Interaction>, With<AdventureMenuButton>)>,
    panel_q: Query<Entity, With<AdventureMenuPanel>>,
) {
    for interaction in &button_q {
        if !matches!(interaction, Interaction::Pressed) {
            continue;
        }
        if state.panel_open {
            for e in &panel_q {
                commands.entity(e).despawn_recursive();
            }
            state.panel_open = false;
            return;
        }
        state.panel_open = true;
        let available = state.available.clone();
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
                    Text::new("Begin Next Adventure"),
                    TextFont {
                        font: font.0.clone(),
                        font_size: 10.0,
                        ..default()
                    },
                    TextColor(Color::srgb(1.0, 0.92, 0.55)),
                ));
                panel.spawn((
                    Text::new("Resets level / gold / inventory. Boons are kept."),
                    TextFont {
                        font: font.0.clone(),
                        font_size: 8.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.85, 0.55, 0.55)),
                ));
                for entry in &available {
                    let label = format!("Begin: {}", entry.display_name);
                    panel
                        .spawn((
                            Button,
                            Node {
                                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                                border: UiRect::all(Val::Px(1.0)),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.10, 0.18, 0.08, 0.9)),
                            BorderColor(Color::srgb(0.65, 0.55, 0.20)),
                            BorderRadius::all(Val::Px(3.0)),
                            AdventureChoiceButton(entry.id.clone()),
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

fn handle_choice_click(
    session: Res<GameSession>,
    interactions: Query<(&Interaction, &AdventureChoiceButton), Changed<Interaction>>,
) {
    for (interaction, choice) in &interactions {
        if !matches!(interaction, Interaction::Pressed) {
            continue;
        }
        let window = match web_sys::window() {
            Some(w) => w,
            None => continue,
        };
        let prompt = format!(
            "Begin '{}'?\nYour current character resets (level, gold, inventory). Boons are kept.\nThis is one-way — you can't go back until you complete the new adventure.",
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
