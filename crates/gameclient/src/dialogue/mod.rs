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

        // POST completion to dev server
        if !event_id.is_empty() {
            let url = format!("http://localhost:3001/events/{}/complete", event_id);
            wasm_bindgen_futures::spawn_local(async move {
                let client = reqwest::Client::new();
                let _ = client.post(&url).send().await;
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

fn update_shop(
    mut commands: Commands,
    font: Res<GameFont>,
    state: Res<ShopState>,
    player: Res<crate::terrain::tilemap::MyPlayerState>,
    existing: Query<Entity, With<ShopPanel>>,
) {
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
            top: Val::Percent(20.0),
            left: Val::Percent(20.0),
            right: Val::Percent(20.0),
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
            let already_has = player.inventory.iter().any(|s| s.item_id == item.item_id);

            parent.spawn((
                Button,
                Node {
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                    justify_content: JustifyContent::SpaceBetween,
                    ..default()
                },
                BackgroundColor(if can_afford && !already_has {
                    Color::srgba(0.15, 0.15, 0.3, 0.8)
                } else {
                    Color::srgba(0.1, 0.1, 0.1, 0.5)
                }),
                BorderRadius::all(Val::Px(3.0)),
                ShopItemButton(i),
            )).with_children(|btn| {
                let label = if already_has {
                    format!("{} (owned)", item.item_id)
                } else {
                    item.item_id.clone()
                };
                btn.spawn((
                    Text::new(label),
                    TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                    TextColor(if can_afford && !already_has {
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
            ShopItemButton(usize::MAX), // sentinel for close
        )).with_children(|btn| {
            btn.spawn((
                Text::new("[ESC] Leave Shop"),
                TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                TextColor(Color::srgb(0.9, 0.7, 0.7)),
            ));
        });
    });
}

fn handle_shop_input(
    mut shop: ResMut<ShopState>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    player: Res<crate::terrain::tilemap::MyPlayerState>,
    session: Res<crate::GameSession>,
    btn_q: Query<(&Interaction, &ShopItemButton)>,
) {
    // ESC to close
    if keys.just_pressed(KeyCode::Escape) && shop.active {
        let event_id = shop.event_id.clone();
        shop.active = false;
        let url = format!("http://localhost:3001/events/{}/complete", event_id);
        wasm_bindgen_futures::spawn_local(async move {
            let client = reqwest::Client::new();
            let _ = client.post(&url).send().await;
        });
        return;
    }

    if !mouse.just_pressed(MouseButton::Left) { return; }

    for (interaction, btn) in &btn_q {
        if !matches!(interaction, Interaction::Hovered | Interaction::Pressed) { continue; }

        if btn.0 == usize::MAX {
            // Close button
            let event_id = shop.event_id.clone();
            shop.active = false;
            let url = format!("http://localhost:3001/events/{}/complete", event_id);
            wasm_bindgen_futures::spawn_local(async move {
                let client = reqwest::Client::new();
                let _ = client.post(&url).send().await;
            });
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
}

// ── Notification Banners ──────────────────────────────

#[derive(Resource, Default)]
pub struct NotificationQueue {
    pub pending: Vec<NotificationData>,
}

pub struct NotificationData {
    pub text: String,
    pub duration: f32,
}

#[derive(Component)]
struct NotificationBanner {
    timer: Timer,
}

#[derive(Component)]
struct NotificationDismiss;

fn update_notifications(
    mut commands: Commands,
    font: Res<GameFont>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut queue: ResMut<NotificationQueue>,
    mut banners: Query<(Entity, &mut NotificationBanner)>,
    dismiss_q: Query<&Interaction, With<NotificationDismiss>>,
) {
    // Dismiss on X key or clicking the X button
    let mut dismiss = keys.just_pressed(KeyCode::KeyX);
    if mouse.just_pressed(MouseButton::Left) {
        for interaction in &dismiss_q {
            if matches!(interaction, Interaction::Hovered | Interaction::Pressed) {
                dismiss = true;
            }
        }
    }
    if dismiss {
        for (entity, _) in &banners {
            commands.entity(entity).despawn_recursive();
        }
        return;
    }

    // Update existing banners
    for (entity, mut banner) in &mut banners {
        banner.timer.tick(time.delta());
        if banner.timer.finished() {
            commands.entity(entity).despawn_recursive();
        }
    }

    // Show next notification if no banner active (FIFO order)
    if banners.is_empty() && !queue.pending.is_empty() {
        let notif = queue.pending.remove(0);
        {
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(40.0),
                    left: Val::Percent(10.0),
                    right: Val::Percent(10.0),
                    padding: UiRect::all(Val::Px(12.0)),
                    justify_content: JustifyContent::SpaceBetween,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundColor(Color::srgba(0.05, 0.05, 0.15, 0.9)),
                BorderColor(Color::srgb(0.4, 0.35, 0.2)),
                BorderRadius::all(Val::Px(4.0)),
                NotificationBanner {
                    timer: Timer::from_seconds(999.0, TimerMode::Once), // stays until dismissed
                },
            )).with_children(|parent| {
                parent.spawn((
                    Text::new(notif.text),
                    TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                    TextColor(Color::srgb(1.0, 0.95, 0.7)),
                ));
                // Dismiss button — clickable
                parent.spawn((
                    Button,
                    Node {
                        padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.4, 0.2, 0.2, 0.5)),
                    BorderRadius::all(Val::Px(3.0)),
                    NotificationDismiss,
                )).with_children(|btn| {
                    btn.spawn((
                        Text::new("X"),
                        TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                        TextColor(Color::srgb(1.0, 0.6, 0.6)),
                    ));
                });
            });
        }
    }
}

