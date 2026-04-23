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
            .insert_resource(JournalHover::default())
            .add_systems(
                Update,
                (
                    toggle_journal,
                    fetch_on_open,
                    update_journal_panel,
                    detect_entry_hover,
                    update_popup,
                    handle_scroll,
                ).run_if(in_state(AppState::InGame)),
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

/// Which entry (by id) the cursor is currently hovering. Drives a popup
/// that renders to the right of the panel with the full dialogue lines.
/// None = no popup.
#[derive(Resource, Default)]
struct JournalHover(Option<String>);

#[derive(Deserialize, Clone)]
struct JournalEntry {
    id: String,
    name: String,
    description: String,
    kind: String,
    /// Dialogue lines / story text for replay. Populated by server for
    /// NpcDialogue / StoryBeat / Quest / Boss / CaveEntrance kinds.
    #[serde(default)]
    lines: Vec<String>,
}

#[derive(Component)]
struct JournalPanel;

#[derive(Component)]
struct JournalCloseButton;

/// Tag + id pair on each entry row. Button component is added too so
/// Bevy tracks Interaction on the node.
#[derive(Component)]
struct JournalEntryRow(String);

/// Root of the hover popup panel.
#[derive(Component)]
struct JournalPopup;

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

/// Refetch the journal while the panel is open — immediately on open,
/// then every 10 s. Previously this only fired on the closed→open
/// transition; if session.player_id was empty on that single frame
/// (still joining, etc.), the fetch was silently skipped and the panel
/// stayed stuck on "adventure is just beginning" forever.
fn fetch_on_open(
    time: Res<Time>,
    open: Res<JournalOpen>,
    session: Res<GameSession>,
    data: Res<JournalData>,
    mut timer: Local<f32>,
) {
    if !open.0 {
        *timer = 0.0; // next open fires immediately
        return;
    }
    *timer -= time.delta_secs();
    if *timer > 0.0 { return; }
    if session.player_id.is_empty() { return; } // retry next tick
    *timer = 10.0;

    let url = crate::api_url(&format!("/journal?player_id={}", session.player_id));
    let slot = data.entries.clone();

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
    popup_q: Query<Entity, With<JournalPopup>>,
    mut last_count: Local<Option<usize>>,
) {
    if !open.0 {
        for e in &panel_q { commands.entity(e).despawn_recursive(); }
        // Close popup immediately when journal is dismissed — otherwise
        // a stale popup can linger if the mouse was over an entry at
        // the moment J was pressed.
        for e in &popup_q { commands.entity(e).despawn_recursive(); }
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
            // scroll_y lets entries keep their natural size and become
            // scrollable when overflowing. Without this, flexbox squishes
            // every entry row to fit inside max_height — 20 entries in
            // 500 px ends up as 20 near-zero-height rows where only the
            // first one is visibly taller than the rest.
            overflow: Overflow::scroll_y(),
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
                    // Rows are Buttons so Bevy tracks Interaction for
                    // hover detection. The Button comp doesn't visually
                    // change anything — we only read Interaction.
                    panel.spawn((
                        Button,
                        Node {
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(2.0),
                            // Extra bottom padding so descriptions don't
                            // collide with the next entry's title bar.
                            padding: UiRect { top: Val::Px(4.0), bottom: Val::Px(8.0), ..default() },
                            border: UiRect { left: Val::Px(3.0), ..default() },
                            // Don't let flexbox squish this entry to fit
                            // inside the scrollable parent. Each row must
                            // keep its intrinsic height so scrolling works.
                            flex_shrink: 0.0,
                            ..default()
                        },
                        BackgroundColor(Color::NONE),
                        JournalEntryRow(entry.id.clone()),
                    ))
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

/// Walk each entry row's Interaction component and update which entry
/// (if any) is currently hovered.
fn detect_entry_hover(
    rows: Query<(&Interaction, &JournalEntryRow)>,
    mut hover: ResMut<JournalHover>,
) {
    let new_hover: Option<String> = rows.iter()
        .find(|(i, _)| matches!(i, Interaction::Hovered | Interaction::Pressed))
        .map(|(_, row)| row.0.clone());
    if new_hover != hover.0 { hover.0 = new_hover; }
}

/// Render a popup to the right of the journal panel when an entry is
/// hovered. Despawned when hover clears or the journal closes.
fn update_popup(
    mut commands: Commands,
    hover: Res<JournalHover>,
    open: Res<JournalOpen>,
    data: Res<JournalData>,
    font: Res<GameFont>,
    popup_q: Query<Entity, With<JournalPopup>>,
    mut last_id: Local<Option<String>>,
) {
    // Close popup when journal is hidden or hover cleared.
    let effective = if open.0 { hover.0.clone() } else { None };
    if effective == *last_id { return; }
    *last_id = effective.clone();

    for e in &popup_q { commands.entity(e).despawn_recursive(); }
    let Some(id) = effective else { return; };

    let Some(entries) = data.entries.lock().ok().and_then(|g| g.clone()) else { return; };
    let Some(entry) = entries.iter().find(|e| e.id == id).cloned() else { return; };

    let tag_color = kind_color(&entry.kind);
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            // Journal sits at top:40 left:12 width:440 — pop to the right.
            top: Val::Px(40.0),
            left: Val::Px(460.0),
            width: Val::Px(340.0),
            padding: UiRect::all(Val::Px(10.0)),
            border: UiRect::all(Val::Px(2.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(6.0),
            ..default()
        },
        ZIndex(15),
        BackgroundColor(Color::srgba(0.03, 0.03, 0.10, 0.96)),
        BorderColor(tag_color),
        BorderRadius::all(Val::Px(4.0)),
        JournalPopup,
    )).with_children(|p| {
        p.spawn((
            Text::new(entry.name.clone()),
            TextFont { font: font.0.clone(), font_size: 11.0, ..default() },
            TextColor(Color::srgb(0.95, 0.85, 0.55)),
        ));
        p.spawn((
            Text::new(format!("[{}]", entry.kind)),
            TextFont { font: font.0.clone(), font_size: 7.0, ..default() },
            TextColor(tag_color),
        ));
        p.spawn((
            Text::new(entry.description.clone()),
            TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
            TextColor(Color::srgb(0.80, 0.80, 0.70)),
            Node { margin: UiRect::top(Val::Px(4.0)), ..default() },
        ));
        if entry.lines.is_empty() {
            p.spawn((
                Text::new("(no recorded dialogue)"),
                TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                TextColor(Color::srgb(0.50, 0.50, 0.45)),
                Node { margin: UiRect::top(Val::Px(4.0)), ..default() },
            ));
        } else {
            for line in &entry.lines {
                p.spawn((
                    Text::new(format!("\"{}\"", line)),
                    TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
                    TextColor(Color::srgb(0.75, 0.85, 0.95)),
                ));
            }
        }
    });
}

/// Scroll the journal panel with the mouse wheel while it's open. Bevy
/// UI doesn't wire wheel events automatically to ScrollPosition — we
/// forward them here.
fn handle_scroll(
    mut wheel: EventReader<bevy::input::mouse::MouseWheel>,
    open: Res<JournalOpen>,
    mut panel_q: Query<&mut ScrollPosition, With<JournalPanel>>,
) {
    if !open.0 { wheel.clear(); return; }
    let mut dy: f32 = 0.0;
    for ev in wheel.read() {
        // Normalize line vs pixel scroll units. `Line` units are ~1 per
        // tick; multiply by a pixel step.
        use bevy::input::mouse::MouseScrollUnit;
        dy += match ev.unit {
            MouseScrollUnit::Line  => ev.y * 30.0,
            MouseScrollUnit::Pixel => ev.y,
        };
    }
    if dy == 0.0 { return; }
    for mut pos in &mut panel_q {
        pos.offset_y = (pos.offset_y - dy).max(0.0);
    }
}
