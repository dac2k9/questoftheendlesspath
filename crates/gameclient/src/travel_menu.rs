//! Fast-travel UI — a button in the top-right that pops a list of
//! gates this player has unlocked, with a click to teleport.
//!
//! Server tracks unlocked gates per player; this just renders them
//! and forwards the teleport request. List is fetched from
//! `GET /travel_gates?player_id=X` whenever the panel opens, so we
//! always show fresh data (gates unlocked since the last open).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use serde::Deserialize;
use wasm_bindgen::JsValue;

use crate::states::AppState;
use crate::{api_url, GameFont, GameSession};

pub struct TravelMenuPlugin;

impl Plugin for TravelMenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TravelMenuState>()
            .add_systems(OnEnter(AppState::InGame), spawn_button)
            .add_systems(
                Update,
                (
                    handle_button_click,
                    apply_fetched_gates,
                    handle_gate_click,
                )
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GateRow {
    id: usize,
    name: String,
    x: usize,
    y: usize,
}

#[derive(Resource)]
struct TravelMenuState {
    panel_open: bool,
    /// Async-fetched gate list. apply_fetched_gates drains this each
    /// frame and rebuilds the panel.
    fetched: Arc<Mutex<Option<Vec<GateRow>>>>,
}

impl Default for TravelMenuState {
    fn default() -> Self {
        Self {
            panel_open: false,
            fetched: Arc::new(Mutex::new(None)),
        }
    }
}

#[derive(Component)]
struct TravelMenuButton;

#[derive(Component)]
struct TravelMenuPanel;

#[derive(Component)]
struct TravelGateButton(usize);

fn spawn_button(mut commands: Commands, font: Res<GameFont>) {
    // Slightly to the LEFT of the adventure-menu button so the two
    // don't overlap. Adventure menu is at right: 8, ~75 px wide;
    // we sit at right: 92.
    commands
        .spawn((
            Button,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(34.0),
                right: Val::Px(92.0),
                padding: UiRect::axes(Val::Px(8.0), Val::Px(3.0)),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.03, 0.08, 0.12, 0.95)),
            BorderColor(Color::srgb(0.45, 0.65, 0.85)),
            BorderRadius::all(Val::Px(3.0)),
            ZIndex(15),
            TravelMenuButton,
        ))
        .with_children(|btn| {
            btn.spawn((
                Text::new("Travel"),
                TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
                TextColor(Color::srgb(0.7, 0.92, 1.0)),
            ));
        });
}

fn handle_button_click(
    mut commands: Commands,
    mut state: ResMut<TravelMenuState>,
    session: Res<GameSession>,
    button_q: Query<&Interaction, (Changed<Interaction>, With<TravelMenuButton>)>,
    panel_q: Query<Entity, With<TravelMenuPanel>>,
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
        // Kick off the fetch; apply_fetched_gates will spawn the
        // panel content once it lands. Show an empty panel
        // immediately so the click feels responsive.
        let player_id = session.player_id.clone();
        let target = state.fetched.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let url = api_url(&format!("/travel_gates?player_id={}", player_id));
            match reqwest::Client::new().get(&url).send().await {
                Ok(resp) => match resp.text().await {
                    Ok(text) => {
                        let parsed: Vec<GateRow> = serde_json::from_str(&text).unwrap_or_default();
                        if let Ok(mut g) = target.lock() {
                            *g = Some(parsed);
                        }
                    }
                    Err(e) => {
                        web_sys::console::warn_1(&JsValue::from_str(&format!(
                            "[travel] /travel_gates body: {}", e
                        )));
                    }
                },
                Err(e) => {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "[travel] /travel_gates: {}", e
                    )));
                }
            }
        });
    }
}

fn apply_fetched_gates(
    mut commands: Commands,
    state: Res<TravelMenuState>,
    font: Res<GameFont>,
    panel_q: Query<Entity, With<TravelMenuPanel>>,
) {
    if !state.panel_open {
        return;
    }
    let Some(gates) = ({
        let mut g = match state.fetched.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        g.take()
    }) else {
        return;
    };
    // Despawn any existing panel content before rebuilding.
    for e in &panel_q {
        commands.entity(e).despawn_recursive();
    }
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(60.0),
                right: Val::Px(92.0),
                width: Val::Px(240.0),
                padding: UiRect::all(Val::Px(8.0)),
                border: UiRect::all(Val::Px(1.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(5.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.03, 0.08, 0.12, 0.97)),
            BorderColor(Color::srgb(0.45, 0.65, 0.85)),
            BorderRadius::all(Val::Px(4.0)),
            ZIndex(45),
            TravelMenuPanel,
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new("Travel To"),
                TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                TextColor(Color::srgb(0.7, 0.92, 1.0)),
            ));
            if gates.is_empty() {
                panel.spawn((
                    Text::new("No gates unlocked yet. Walk onto a travel gate to add it."),
                    TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                    TextColor(Color::srgb(0.7, 0.7, 0.7)),
                ));
            } else {
                for gate in gates {
                    panel
                        .spawn((
                            Button,
                            Node {
                                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                                border: UiRect::all(Val::Px(1.0)),
                                flex_direction: FlexDirection::Column,
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.07, 0.14, 0.20, 0.95)),
                            BorderColor(Color::srgb(0.30, 0.50, 0.70)),
                            BorderRadius::all(Val::Px(3.0)),
                            TravelGateButton(gate.id),
                        ))
                        .with_children(|btn| {
                            btn.spawn((
                                Text::new(format!("→ {}", gate.name)),
                                TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
                                TextColor(Color::srgb(0.85, 0.95, 1.0)),
                            ));
                            btn.spawn((
                                Text::new(format!("({}, {})", gate.x, gate.y)),
                                TextFont { font: font.0.clone(), font_size: 7.0, ..default() },
                                TextColor(Color::srgb(0.55, 0.65, 0.75)),
                            ));
                        });
                }
            }
        });
}

fn handle_gate_click(
    mut commands: Commands,
    mut state: ResMut<TravelMenuState>,
    session: Res<GameSession>,
    interactions: Query<(&Interaction, &TravelGateButton), Changed<Interaction>>,
    panel_q: Query<Entity, With<TravelMenuPanel>>,
) {
    for (interaction, gate) in &interactions {
        if !matches!(interaction, Interaction::Pressed) {
            continue;
        }
        let player_id = session.player_id.clone();
        let gate_id = gate.0;
        // Close the panel optimistically — the teleport is usually
        // fast enough that re-opening to confirm isn't worth the
        // friction.
        for e in &panel_q {
            commands.entity(e).despawn_recursive();
        }
        state.panel_open = false;
        wasm_bindgen_futures::spawn_local(async move {
            let url = api_url("/travel_to_gate");
            let body = format!(
                r#"{{"player_id":"{}","gate_id":{}}}"#,
                player_id, gate_id
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
                        "[travel] teleported via gate {}", gate_id
                    )));
                }
                Ok(resp) => {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "[travel] /travel_to_gate HTTP {}", resp.status()
                    )));
                }
                Err(e) => {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "[travel] /travel_to_gate err: {}", e
                    )));
                }
            }
        });
    }
}
