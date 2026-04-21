//! Journal panel — "story so far" view of the player's completed events.
//!
//! Toggle with `J`. Fetches GET /journal?player_id=X from the server each
//! time it's opened (cheap enough; the list grows slowly). The panel shows
//! each completed event as a row with its name, description, and a kind
//! tag color so dialogues / treasures / encounters read distinctly.

use bevy::prelude::*;
use serde::Deserialize;
use std::sync::{Arc, Mutex};

use crate::states::AppState;
use crate::{GameFont, GameSession};

pub struct JournalPlugin;

impl Plugin for JournalPlugin {
    fn build(&self, app: &mut App) {
        app
            .insert_resource(JournalOpen(false))
            .insert_resource(JournalData::default())
            .add_systems(
                Update,
                (toggle_journal, fetch_on_open, update_journal_panel)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

// ── Data ────────────────────────────────────────────

#[derive(Resource, Default)]
struct JournalOpen(bool);

#[derive(Resource, Default, Clone)]
struct JournalData {
    entries: Arc<Mutex<Option<Vec<JournalEntry>>>>,
}

#[derive(Deserialize, Clone)]
struct JournalEntry {
    #[allow(dead_code)]
    id: String,
    name: String,
    description: String,
    kind: String,
}

#[derive(Component)]
struct JournalPanel;

#[derive(Component)]
struct JournalCloseButton;

// ── Systems ─────────────────────────────────────────

fn toggle_journal(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut open: ResMut<JournalOpen>,
    close_btn: Query<&Interaction, With<JournalCloseButton>>,
) {
    if keys.just_pressed(KeyCode::KeyJ) {
        open.0 = !open.0;
    }
    if mouse.just_pressed(MouseButton::Left) {
        for interaction in &close_btn {
            if matches!(interaction, Interaction::Hovered | Interaction::Pressed) {
                open.0 = false;
            }
        }
    }
}

/// Kick off a fetch each time the panel transitions closed → open. Cheap:
/// small JSON, grows slowly with playtime. Results land in JournalData.
fn fetch_on_open(
    open: Res<JournalOpen>,
    session: Res<GameSession>,
    data: Res<JournalData>,
    mut was_open: Local<bool>,
) {
    let just_opened = open.0 && !*was_open;
    *was_open = open.0;
    if !just_opened { return; }
    if session.player_id.is_empty() { return; }

    let url = format!("/journal?player_id={}", session.player_id);
    let slot = data.entries.clone();
    // Mark in-flight by clearing. UI shows "Loading…" until entries resolve.
    if let Ok(mut g) = slot.lock() { *g = None; }

    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        let resp = client.get(&url).send().await;
        let entries: Vec<JournalEntry> = match resp {
            Ok(r) => r.json().await.unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        if let Ok(mut g) = slot.lock() { *g = Some(entries); }
    });
}

fn update_journal_panel(
    mut commands: Commands,
    open: Res<JournalOpen>,
    font: Res<GameFont>,
    data: Res<JournalData>,
    panel_q: Query<Entity, With<JournalPanel>>,
    mut last_count: Local<Option<usize>>,
) {
    if !open.0 {
        for e in &panel_q { commands.entity(e).despawn_recursive(); }
        *last_count = None;
        return;
    }

    let entries: Option<Vec<JournalEntry>> = data.entries.lock().ok().and_then(|g| g.clone());
    let count = entries.as_ref().map(|v| v.len());

    // Only rebuild when the entry count changes (or on first open).
    let needs_rebuild = panel_q.is_empty() || *last_count != count;
    if !needs_rebuild { return; }
    *last_count = count;

    for e in &panel_q { commands.entity(e).despawn_recursive(); }

    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(40.0),
            left: Val::Px(12.0),
            width: Val::Px(440.0),
            min_height: Val::Px(100.0),
            max_height: Val::Px(500.0),
            padding: UiRect::all(Val::Px(12.0)),
            border: UiRect::all(Val::Px(2.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(6.0),
            overflow: Overflow::clip(),
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.02, 0.08, 0.92)),
        BorderColor(Color::srgb(0.4, 0.35, 0.2)),
        BorderRadius::all(Val::Px(6.0)),
        JournalPanel,
    )).with_children(|panel| {
        // Header
        panel.spawn(Node {
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            margin: UiRect::bottom(Val::Px(4.0)),
            ..default()
        }).with_children(|header| {
            header.spawn((
                Text::new("Journal — Story So Far"),
                TextFont { font: font.0.clone(), font_size: 12.0, ..default() },
                TextColor(Color::srgb(0.95, 0.85, 0.55)),
            ));
            header.spawn((
                Button,
                Node {
                    width: Val::Px(20.0), height: Val::Px(20.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundColor(Color::srgba(0.2, 0.1, 0.1, 0.9)),
                BorderRadius::all(Val::Px(3.0)),
                JournalCloseButton,
            )).with_children(|b| {
                b.spawn((
                    Text::new("X"),
                    TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                    TextColor(Color::srgb(0.9, 0.9, 0.9)),
                ));
            });
        });

        match entries {
            None => {
                panel.spawn((
                    Text::new("Loading…"),
                    TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
                    TextColor(Color::srgb(0.6, 0.6, 0.6)),
                ));
            }
            Some(entries) if entries.is_empty() => {
                panel.spawn((
                    Text::new("Your adventure is just beginning."),
                    TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
                    TextColor(Color::srgb(0.6, 0.6, 0.6)),
                ));
            }
            Some(entries) => {
                // Newest first: the completed_events list is append-order, so
                // reversing gives reverse-chronological reading — last thing
                // you did shows at the top.
                for entry in entries.iter().rev() {
                    let tag_color = kind_color(&entry.kind);
                    panel.spawn(Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(2.0),
                        padding: UiRect { top: Val::Px(4.0), bottom: Val::Px(4.0), ..default() },
                        border: UiRect { left: Val::Px(3.0), ..default() },
                        ..default()
                    })
                    .insert(BorderColor(tag_color))
                    .with_children(|row| {
                        row.spawn(Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(6.0),
                            align_items: AlignItems::Baseline,
                            padding: UiRect::left(Val::Px(6.0)),
                            ..default()
                        }).with_children(|title_row| {
                            title_row.spawn((
                                Text::new(entry.name.clone()),
                                TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                                TextColor(Color::srgb(0.9, 0.85, 0.6)),
                            ));
                            title_row.spawn((
                                Text::new(format!("[{}]", entry.kind)),
                                TextFont { font: font.0.clone(), font_size: 7.0, ..default() },
                                TextColor(tag_color),
                            ));
                        });
                        row.spawn((
                            Text::new(entry.description.clone()),
                            TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                            TextColor(Color::srgb(0.75, 0.75, 0.7)),
                            Node { padding: UiRect::left(Val::Px(6.0)), ..default() },
                        ));
                    });
                }
            }
        }
    });
}

/// Color-code the kind tag so dialogues, combats, stories, etc. are
/// visually distinct at a glance.
fn kind_color(kind: &str) -> Color {
    match kind {
        "dialogue"  => Color::srgb(0.55, 0.75, 0.95), // pale blue
        "treasure"  => Color::srgb(0.95, 0.80, 0.35), // gold
        "encounter" => Color::srgb(0.90, 0.45, 0.45), // red
        "boss"      => Color::srgb(0.95, 0.30, 0.60), // magenta
        "story"     => Color::srgb(0.70, 0.90, 0.70), // soft green
        "cave"      => Color::srgb(0.65, 0.55, 0.90), // violet
        "quest"     => Color::srgb(0.85, 0.75, 0.35), // amber
        _           => Color::srgb(0.60, 0.60, 0.60), // grey
    }
}
