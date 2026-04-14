pub mod event_poll;

use bevy::prelude::*;

use crate::states::AppState;
use crate::GameFont;

pub struct DialoguePlugin;

impl Plugin for DialoguePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(DialogueState::default())
            .insert_resource(NotificationQueue::default())
            .insert_resource(ShopState::default())
            .insert_resource(MessageLog::default())
            .add_systems(
                Update,
                (
                    event_poll::poll_active_events,
                    update_dialogue,
                    update_notifications,
                    handle_dialogue_input,
                    update_shop,
                    handle_shop_input,
                )
                    .run_if(in_state(AppState::InGame).and(not(crate::combat::combat_active))),
            );
    }
}

/// Returns true when dialogue is active — used as a run condition.
pub fn dialogue_active(state: Res<DialogueState>) -> bool {
    state.active
}

// ── Dialogue Box ──────────────────────────────────────

#[derive(Resource, Default)]
pub struct DialogueState {
    pub active: bool,
    pub event_id: String,
    pub speaker: String,
    pub lines: Vec<String>,
    pub current_line: usize,
    pub typewriter_index: usize,
    pub typewriter_timer: f32,
    pub choices: Vec<String>,
}

#[derive(Component)]
struct DialogueBox;

#[derive(Component)]
struct DialogueSpeaker;

#[derive(Component)]
struct DialogueText;

#[derive(Component)]
struct DialogueContinue;

fn update_dialogue(
    mut commands: Commands,
    font: Res<GameFont>,
    mut state: ResMut<DialogueState>,
    time: Res<Time>,
    existing: Query<Entity, With<DialogueBox>>,
    mut text_q: Query<&mut Text, With<DialogueText>>,
) {
    if !state.active {
        // Remove dialogue box if it exists
        for entity in &existing {
            commands.entity(entity).despawn_recursive();
        }
        return;
    }

    // Typewriter effect
    state.typewriter_timer += time.delta_secs();
    if state.typewriter_timer > 0.03 {
        state.typewriter_timer = 0.0;
        if state.current_line < state.lines.len() {
            let full_line = &state.lines[state.current_line];
            let char_count = full_line.chars().count();
            if state.typewriter_index < char_count {
                state.typewriter_index += 1;
            }
        }
    }

    // Update typewriter text on existing dialogue box (char-safe slicing)
    if !existing.is_empty() {
        if let Ok(mut text) = text_q.get_single_mut() {
            if state.current_line < state.lines.len() {
                let full = &state.lines[state.current_line];
                let visible: String = full.chars().take(state.typewriter_index).collect();
                **text = visible;
            }
        }
        return;
    }

    // Build dialogue box UI
    let speaker = state.speaker.clone();
    let line_text = if state.current_line < state.lines.len() {
        let full = &state.lines[state.current_line];
        full.chars().take(state.typewriter_index).collect::<String>()
    } else {
        String::new()
    };

    // Container centered on screen — narrower and taller
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Percent(10.0),
            left: Val::Percent(25.0),
            right: Val::Percent(25.0),
            min_height: Val::Px(160.0),
            padding: UiRect::all(Val::Px(20.0)),
            border: UiRect::all(Val::Px(2.0)),
            flex_direction: FlexDirection::Column,
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.02, 0.08, 0.92)),
        BorderColor(Color::srgb(0.4, 0.35, 0.2)),
        BorderRadius::all(Val::Px(6.0)),
        DialogueBox,
    )).with_children(|parent| {
        // Speaker name
        parent.spawn((
            Text::new(speaker),
            TextFont { font: font.0.clone(), font_size: 14.0, ..default() },
            TextColor(Color::srgb(1.0, 0.85, 0.3)),
            DialogueSpeaker,
        ));

        // Dialogue text
        parent.spawn((
            Text::new(line_text),
            TextFont { font: font.0.clone(), font_size: 12.0, ..default() },
            TextColor(Color::srgb(0.9, 0.9, 0.9)),
            Node { margin: UiRect::top(Val::Px(12.0)), ..default() },
            DialogueText,
        ));

        // Continue button — bottom right
        parent.spawn((
            Button,
            Node {
                padding: UiRect::axes(Val::Px(14.0), Val::Px(6.0)),
                align_self: AlignSelf::FlexEnd,
                margin: UiRect::top(Val::Px(16.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.2, 0.2, 0.4, 0.8)),
            BorderRadius::all(Val::Px(4.0)),
            DialogueContinue,
        )).with_children(|btn| {
            btn.spawn((
                Text::new("Continue"),
                TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                TextColor(Color::srgb(0.8, 0.8, 0.9)),
            ));
        });
    });
}

fn handle_dialogue_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut state: ResMut<DialogueState>,
    mut commands: Commands,
    existing: Query<Entity, With<DialogueBox>>,
    session: Res<crate::GameSession>,
    time: Res<Time>,
    mut debounce: Local<f32>,
) {
    if !state.active {
        *debounce = 0.0;
        return;
    }

    // Debounce: ignore clicks for 0.3s after dialogue appears or advances
    // (prevents accidental dismiss when focusing browser window)
    *debounce += time.delta_secs();
    let advance = (*debounce > 0.3)
        && (mouse.just_pressed(MouseButton::Left)
            || keys.just_pressed(KeyCode::Enter)
            || keys.just_pressed(KeyCode::Space));

    if !advance {
        return;
    }

    // If typewriter hasn't finished, show full line
    if state.current_line < state.lines.len() {
        let char_count = state.lines[state.current_line].chars().count();
        if state.typewriter_index < char_count {
            state.typewriter_index = char_count;
            *debounce = 0.0; // reset debounce for next click
            // Rebuild UI to show full text
            for entity in &existing {
                commands.entity(entity).despawn_recursive();
            }
            return;
        }
    }

    // Advance to next line
    *debounce = 0.0; // reset debounce for next line
    state.current_line += 1;
    state.typewriter_index = 0;

    if state.current_line >= state.lines.len() {
        // Dialogue complete — dismiss and notify server
        let event_id = state.event_id.clone();
        state.active = false;
        state.lines.clear();
        state.current_line = 0;

        // Remove dialogue box
        for entity in &existing {
            commands.entity(entity).despawn_recursive();
        }

        // POST completion to dev server with player_id
        if !event_id.is_empty() {
            let url = format!("http://localhost:3001/events/{}/complete", event_id);
            let player_id = session.player_id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let client = reqwest::Client::new();
                let _ = client.post(&url)
                    .json(&serde_json::json!({"player_id": player_id}))
                    .send().await;
            });
        }
    } else {
        // Rebuild UI for new line
        for entity in &existing {
            commands.entity(entity).despawn_recursive();
        }
    }
}

// ── Shop UI ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ShopItem {
    pub item_id: String,
    pub cost: i32,
}

#[derive(Resource, Default)]
pub struct ShopState {
    pub active: bool,
    pub event_id: String,
    pub merchant_name: String,
    pub items: Vec<ShopItem>,
    /// A shop is available at the current location (show button in HUD).
    pub available: bool,
}

/// Returns true when shop is active — used as a run condition.
pub fn shop_active(state: Res<ShopState>) -> bool {
    state.active
}

#[derive(Component)]
struct ShopPanel;

#[derive(Component)]
struct ShopItemButton(usize); // index into ShopState.items

#[derive(Component)]
struct SellItemButton(String); // item_id

fn update_shop(
    mut commands: Commands,
    font: Res<GameFont>,
    mut state: ResMut<ShopState>,
    player: Res<crate::terrain::tilemap::MyPlayerState>,
    catalog: Res<crate::hud::ItemCatalogRes>,
    existing: Query<Entity, With<ShopPanel>>,
) {
    // Auto-close when player leaves the shop POI
    if state.active && !state.available {
        state.active = false;
    }
    if !state.active {
        for entity in &existing {
            commands.entity(entity).despawn_recursive();
        }
        return;
    }

    // Rebuild every frame to keep gold amounts current
    for entity in &existing {
        commands.entity(entity).despawn_recursive();
    }

    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Percent(15.0),
            left: Val::Percent(30.0),
            right: Val::Percent(30.0),
            min_height: Val::Px(150.0),
            padding: UiRect::all(Val::Px(16.0)),
            border: UiRect::all(Val::Px(2.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(8.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.02, 0.08, 0.95)),
        BorderColor(Color::srgb(0.4, 0.35, 0.2)),
        BorderRadius::all(Val::Px(6.0)),
        ShopPanel,
    )).with_children(|parent| {
        // Merchant name
        parent.spawn((
            Text::new(format!("{}", state.merchant_name)),
            TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(1.0, 0.85, 0.3)),
        ));

        // Gold display
        parent.spawn((
            Text::new(format!("Your gold: {}", player.gold)),
            TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
            TextColor(Color::srgb(1.0, 0.85, 0.2)),
        ));

        // Items
        for (i, item) in state.items.iter().enumerate() {
            let can_afford = player.gold >= item.cost;
            let def = catalog.0.get(&item.item_id);
            let in_inventory = player.inventory.iter().find(|s| s.item_id == item.item_id);
            let is_equipped = player.equipment.has_equipped(&item.item_id);
            let is_full = if is_equipped {
                // Equipment already equipped — can't buy another
                true
            } else {
                in_inventory.map_or(false, |slot| {
                    let max = def.filter(|d| d.stackable).map(|d| d.max_stack).unwrap_or(1);
                    slot.quantity >= max
                })
            };
            let display_name = def.map(|d| d.display_name.as_str()).unwrap_or(&item.item_id);

            parent.spawn((
                Button,
                Node {
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                    justify_content: JustifyContent::SpaceBetween,
                    ..default()
                },
                BackgroundColor(if can_afford && !is_full {
                    Color::srgba(0.15, 0.15, 0.3, 0.8)
                } else {
                    Color::srgba(0.1, 0.1, 0.1, 0.5)
                }),
                BorderRadius::all(Val::Px(3.0)),
                ShopItemButton(i),
                crate::hud::InventoryItemRow(item.item_id.clone()),
            )).with_children(|btn| {
                let label = if is_full {
                    format!("{} (full)", display_name)
                } else {
                    display_name.to_string()
                };
                btn.spawn((
                    Text::new(label),
                    TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                    TextColor(if can_afford && !is_full {
                        Color::srgb(0.9, 0.9, 0.9)
                    } else {
                        Color::srgb(0.5, 0.5, 0.5)
                    }),
                ));
                btn.spawn((
                    Text::new(format!("{} gold", item.cost)),
                    TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                    TextColor(Color::srgb(1.0, 0.85, 0.2)),
                ));
            });
        }

        // Sell section — show player's sellable items
        let sellable: Vec<_> = player.inventory.iter()
            .filter(|slot| {
                catalog.0.get(&slot.item_id)
                    .map_or(false, |d| d.category != questlib::items::ItemCategory::KeyItem)
            })
            .collect();
        if !sellable.is_empty() {
            parent.spawn((
                Text::new("-- Sell --"),
                TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                TextColor(Color::srgb(0.6, 0.6, 0.5)),
                Node { margin: UiRect::top(Val::Px(8.0)), ..default() },
            ));
            for slot in &sellable {
                let def = catalog.0.get(&slot.item_id);
                let display_name = def.map(|d| d.display_name.as_str()).unwrap_or(&slot.item_id);
                let sell_price = self::sell_price(&slot.item_id, &catalog.0);
                let label = if slot.quantity > 1 {
                    format!("{} x{}", display_name, slot.quantity)
                } else {
                    display_name.to_string()
                };

                parent.spawn((
                    Button,
                    Node {
                        padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                        justify_content: JustifyContent::SpaceBetween,
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.2, 0.15, 0.1, 0.8)),
                    BorderRadius::all(Val::Px(3.0)),
                    SellItemButton(slot.item_id.clone()),
                    crate::hud::InventoryItemRow(slot.item_id.clone()),
                )).with_children(|btn| {
                    btn.spawn((
                        Text::new(label),
                        TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                        TextColor(Color::srgb(0.9, 0.8, 0.7)),
                    ));
                    btn.spawn((
                        Text::new(format!("+{} gold", sell_price)),
                        TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                        TextColor(Color::srgb(0.5, 0.9, 0.4)),
                    ));
                });
            }
        }

        // Close button
        parent.spawn((
            Button,
            Node {
                padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                align_self: AlignSelf::Center,
                margin: UiRect::top(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.3, 0.15, 0.15, 0.8)),
            BorderRadius::all(Val::Px(3.0)),
            ShopItemButton(usize::MAX),
        )).with_children(|btn| {
            btn.spawn((
                Text::new("[ESC] Leave Shop"),
                TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                TextColor(Color::srgb(0.9, 0.7, 0.7)),
            ));
        });
    });
}

/// Sell price = half the cheapest shop price for that item, minimum 5 gold.
fn sell_price(item_id: &str, catalog: &questlib::items::ItemCatalog) -> i32 {
    // Look up a reasonable price from item effects
    let def = catalog.get(item_id);
    let base = match item_id {
        "wooden_club" => 40, "iron_sword" => 120, "fire_blade" => 200, "frost_axe" => 180,
        "leather_vest" => 50, "chainmail" => 150, "dragonscale_armor" => 300,
        "warm_cloak" => 60, "bog_charm" => 60, "ring_of_vigor" => 100, "berserker_pendant" => 80,
        "health_potion" => 30, "greater_health_potion" => 60, "speed_potion" => 80,
        "mystery_potion" => 40, "battle_elixir" => 120,
        "torch" => 20, "compass" => 60, "explorers_map" => 180,
        _ => 20,
    };
    let _ = def; // suppress unused
    (base / 2).max(5)
}

fn handle_shop_input(
    mut shop: ResMut<ShopState>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    player: Res<crate::terrain::tilemap::MyPlayerState>,
    session: Res<crate::GameSession>,
    btn_q: Query<(&Interaction, &ShopItemButton)>,
    sell_q: Query<(&Interaction, &SellItemButton)>,
) {
    // ESC to close shop (no server completion — shops are always available)
    if keys.just_pressed(KeyCode::Escape) && shop.active {
        shop.active = false;
        return;
    }

    if !mouse.just_pressed(MouseButton::Left) { return; }

    for (interaction, btn) in &btn_q {
        if !matches!(interaction, Interaction::Hovered | Interaction::Pressed) { continue; }

        if btn.0 == usize::MAX {
            shop.active = false;
            return;
        }

        if let Some(item) = shop.items.get(btn.0) {
            if player.gold >= item.cost {
                let player_id = session.player_id.clone();
                let item_id = item.item_id.clone();
                let cost = item.cost;
                wasm_bindgen_futures::spawn_local(async move {
                    let client = reqwest::Client::new();
                    let _ = client.post("http://localhost:3001/buy_item")
                        .json(&serde_json::json!({
                            "player_id": player_id,
                            "item_id": item_id,
                            "cost": cost,
                        }))
                        .send()
                        .await;
                });
            }
        }
    }

    // Sell items
    for (interaction, sell_btn) in &sell_q {
        if !matches!(interaction, Interaction::Hovered | Interaction::Pressed) { continue; }
        let player_id = session.player_id.clone();
        let item_id = sell_btn.0.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let client = reqwest::Client::new();
            let _ = client.post("http://localhost:3001/sell_item")
                .json(&serde_json::json!({"player_id": player_id, "item_id": item_id}))
                .send().await;
        });
    }
}

// ── Message Log (bottom-left panel) ──────────────────

const MAX_LOG_LINES: usize = 8;
const LOG_TYPEWRITER_SPEED: f32 = 0.01; // seconds per character

#[derive(Resource, Default)]
pub struct NotificationQueue {
    pub pending: Vec<NotificationData>,
}

pub struct NotificationData {
    pub text: String,
    pub duration: f32,
}

#[derive(Resource)]
struct MessageLog {
    /// Completed lines (fully typed out).
    lines: Vec<String>,
    /// Currently typing line.
    current_text: Option<String>,
    /// Characters revealed so far in current line.
    typewriter_index: usize,
    typewriter_timer: f32,
}

impl Default for MessageLog {
    fn default() -> Self {
        Self { lines: Vec::new(), current_text: None, typewriter_index: 0, typewriter_timer: 0.0 }
    }
}

#[derive(Component)]
struct MessageLogPanel;

#[derive(Component)]
struct MessageLogText;

fn update_notifications(
    mut commands: Commands,
    font: Res<GameFont>,
    time: Res<Time>,
    mut queue: ResMut<NotificationQueue>,
    mut log: ResMut<MessageLog>,
    panel_q: Query<Entity, With<MessageLogPanel>>,
    mut text_q: Query<&mut Text, With<MessageLogText>>,
) {
    // Advance typewriter on current line
    let mut finished_line: Option<String> = None;
    if let Some(current) = &log.current_text {
        let char_count = current.chars().count();
        if log.typewriter_index >= char_count {
            finished_line = Some(current.clone());
        }
    }
    if let Some(line) = finished_line {
        log.lines.push(line);
        if log.lines.len() > MAX_LOG_LINES {
            log.lines.remove(0);
        }
        log.current_text = None;
    } else if log.current_text.is_some() {
        log.typewriter_timer += time.delta_secs();
        while log.typewriter_timer > LOG_TYPEWRITER_SPEED {
            log.typewriter_timer -= LOG_TYPEWRITER_SPEED;
            log.typewriter_index += 1;
        }
    }

    // Start next pending message if no line is currently typing
    if log.current_text.is_none() && !queue.pending.is_empty() {
        let notif = queue.pending.remove(0);
        log.current_text = Some(notif.text);
        log.typewriter_index = 0;
        log.typewriter_timer = 0.0;
    }

    // Build display text
    let mut display = String::new();
    for line in &log.lines {
        if !display.is_empty() { display.push('\n'); }
        display.push_str(line);
    }
    if let Some(current) = &log.current_text {
        if !display.is_empty() { display.push('\n'); }
        let visible: String = current.chars().take(log.typewriter_index).collect();
        display.push_str(&visible);
    }

    // Update existing panel text
    if let Ok(mut text) = text_q.get_single_mut() {
        **text = display;
        return;
    }

    // Spawn panel if it doesn't exist
    if panel_q.is_empty() {
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(8.0),
                left: Val::Px(8.0),
                width: Val::Px(350.0),
                max_height: Val::Px(200.0),
                padding: UiRect::all(Val::Px(10.0)),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::FlexEnd,
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.4)),
            BorderRadius::all(Val::Px(4.0)),
            MessageLogPanel,
        )).with_children(|parent| {
            parent.spawn((
                Text::new(""),
                TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
                TextColor(Color::srgb(0.8, 0.8, 0.7)),
                MessageLogText,
            ));
        });
    }
}

