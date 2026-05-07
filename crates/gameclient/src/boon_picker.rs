//! Boon picker modal — opens whenever the server sets
//! `MyPlayerState.pending_boon_choice` (after a climactic-quest
//! victory). Shows the 3 offered boons as cards; clicking one POSTs
//! `/select_boon` and the server clears the pending field, closing
//! the modal on the next poll.
//!
//! Looked up in the static `questlib::boons` catalog so we don't ship
//! duplicate metadata over the wire — server only sends boon ids,
//! the client renders name + description.

use bevy::prelude::*;
use wasm_bindgen::JsValue;

use crate::states::AppState;
use crate::terrain::tilemap::MyPlayerState;
use crate::{api_url, GameFont, GameSession};

pub struct BoonPickerPlugin;

impl Plugin for BoonPickerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (rebuild_panel, handle_clicks).run_if(in_state(AppState::InGame)),
        );
    }
}

/// Run condition — true while the picker is open. Used (later) by other
/// input handlers if they need to suppress while the picker has focus;
/// for now the picker's full-screen scrim already eats clicks.
pub fn picker_active(player: Res<MyPlayerState>) -> bool {
    player.pending_boon_choice.is_some()
}

#[derive(Component)]
struct BoonPickerRoot;

#[derive(Component)]
struct BoonChoiceButton {
    boon_id: String,
}

/// (Re)build the modal whenever the pending choice changes — appears
/// when the field flips to Some, vanishes when it flips back to None.
/// Cheap to fully respawn since the panel is small.
fn rebuild_panel(
    mut commands: Commands,
    player: Res<MyPlayerState>,
    font: Res<GameFont>,
    panel_q: Query<Entity, With<BoonPickerRoot>>,
    mut last_pending: Local<Option<crate::supabase::PendingBoonChoice>>,
) {
    let pending = player.pending_boon_choice.clone();
    if pending == *last_pending && (pending.is_none() == panel_q.is_empty()) {
        return;
    }
    *last_pending = pending.clone();
    for e in &panel_q {
        commands.entity(e).despawn_recursive();
    }
    let Some(pending) = pending else { return };

    // Full-screen scrim so the rest of the world is visually paused
    // and clicks outside the cards don't leak to the map.
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(16.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)),
            ZIndex(50),
            BoonPickerRoot,
        ))
        .with_children(|root| {
            // Header
            root.spawn((
                Text::new("CHOOSE YOUR BOON"),
                TextFont {
                    font: font.0.clone(),
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 0.92, 0.55)),
            ));
            root.spawn((
                Text::new("Permanent reward — survives adventure resets."),
                TextFont {
                    font: font.0.clone(),
                    font_size: 8.0,
                    ..default()
                },
                TextColor(Color::srgb(0.7, 0.7, 0.7)),
            ));

            // Card row
            root.spawn(Node {
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Stretch,
                column_gap: Val::Px(12.0),
                margin: UiRect::top(Val::Px(12.0)),
                ..default()
            })
            .with_children(|row| {
                for boon_id in &pending.choices {
                    let Some(boon) = questlib::boons::lookup(boon_id) else {
                        // Unknown id (catalog drift between server / client).
                        // Render a minimal placeholder so the player can still
                        // pick something — server will reject if it's bad.
                        spawn_card(row, &font, boon_id, "Unknown", "—");
                        continue;
                    };
                    spawn_card(row, &font, boon.id, boon.name, boon.description);
                }
            });
        });
}

fn spawn_card(
    parent: &mut ChildBuilder,
    font: &GameFont,
    boon_id: &str,
    name: &str,
    description: &str,
) {
    parent
        .spawn((
            Button,
            Node {
                width: Val::Px(180.0),
                min_height: Val::Px(160.0),
                padding: UiRect::all(Val::Px(12.0)),
                border: UiRect::all(Val::Px(2.0)),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                row_gap: Val::Px(8.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.10, 0.07, 0.03, 0.98)),
            BorderColor(Color::srgb(0.85, 0.65, 0.20)),
            BorderRadius::all(Val::Px(6.0)),
            BoonChoiceButton {
                boon_id: boon_id.to_string(),
            },
        ))
        .with_children(|card| {
            // Name
            card.spawn((
                Text::new(name),
                TextFont {
                    font: font.0.clone(),
                    font_size: 12.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 0.92, 0.55)),
            ));
            // Description
            card.spawn((
                Text::new(description),
                TextFont {
                    font: font.0.clone(),
                    font_size: 8.0,
                    ..default()
                },
                TextColor(Color::srgb(0.85, 0.85, 0.85)),
            ));
            // CTA
            card.spawn((
                Text::new("CHOOSE"),
                TextFont {
                    font: font.0.clone(),
                    font_size: 10.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 0.55, 0.15)),
            ));
        });
}

fn handle_clicks(
    session: Res<GameSession>,
    interactions: Query<(&Interaction, &BoonChoiceButton), Changed<Interaction>>,
) {
    for (interaction, button) in &interactions {
        if !matches!(interaction, Interaction::Pressed) {
            continue;
        }
        let player_id = session.player_id.clone();
        let boon_id = button.boon_id.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let url = api_url("/select_boon");
            let body = format!(
                r#"{{"player_id":"{}","boon_id":"{}"}}"#,
                player_id, boon_id
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
                        "[boons] selected '{}'",
                        boon_id
                    )));
                }
                Ok(resp) => {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "[boons] /select_boon HTTP {}",
                        resp.status()
                    )));
                }
                Err(e) => {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "[boons] /select_boon error: {}",
                        e
                    )));
                }
            }
        });
    }
}
