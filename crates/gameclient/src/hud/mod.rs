pub mod floating_text;

use bevy::prelude::*;

use crate::states::AppState;
use crate::terrain::path::DisplayRoute;
use crate::terrain::tilemap::MyPlayerState;
use crate::terrain::world::WorldGrid;
use crate::{GameFont, GameSession};
use floating_text::{spawn_floating_text, update_floating_texts};

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        let catalog = questlib::items::ItemCatalog::from_json(
            include_str!("../../../../adventures/items.json")
        ).unwrap_or_default();
        app.insert_resource(InventoryOpen(false))
            .insert_resource(LastInventorySnapshot::default())
            .insert_resource(ItemCatalogRes(catalog))
            .add_systems(OnEnter(AppState::InGame), spawn_hud)
            .add_systems(
                Update,
                (update_hud, detect_gold_change, detect_level_up, update_floating_texts, toggle_inventory, update_inventory, show_item_tooltip, update_shop_button, handle_inventory_click)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Component)]
struct HudRoot;

#[derive(Component)]
struct GoldText;

#[derive(Component)]
struct LevelText;

#[derive(Component)]
struct XpBarFill;

#[derive(Component)]
struct DistanceText;

#[derive(Component)]
struct SpeedText;

#[derive(Component)]
struct BiomeText;

#[derive(Component)]
struct InventoryButton;

#[derive(Component)]
struct ShopButton;

#[derive(Component)]
struct ShopButtonRoot;

#[derive(Resource, Default)]
struct LastKnownGold(i32);

#[derive(Resource)]
struct LastKnownLevel(u32);

impl Default for LastKnownLevel {
    fn default() -> Self { Self(1) }
}

fn spawn_hud(mut commands: Commands, font: Res<GameFont>) {
    commands.insert_resource(LastKnownGold::default());
    commands.insert_resource(LastKnownLevel::default());

    // Top bar container
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(0.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            height: Val::Px(28.0),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(4.0)),
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
        HudRoot,
    )).with_children(|parent| {
        // Left group: Gold + Inventory button
        parent.spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(10.0),
            ..default()
        }).with_children(|left| {
            left.spawn((
                Text::new("Gold: 0"),
                TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                TextColor(Color::srgb(1.0, 0.85, 0.2)),
                GoldText,
            ));
            left.spawn((
                Button,
                Node {
                    padding: UiRect::axes(Val::Px(8.0), Val::Px(2.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.3, 0.25, 0.15, 0.7)),
                BorderRadius::all(Val::Px(3.0)),
                InventoryButton,
            )).with_children(|btn| {
                btn.spawn((
                    Text::new("[I]nv"),
                    TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
                    TextColor(Color::srgb(0.8, 0.75, 0.6)),
                ));
            });
        });

        // Level + XP bar container
        parent.spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(6.0),
            ..default()
        }).with_children(|level_parent| {
            // Level text
            level_parent.spawn((
                Text::new("Lv 1"),
                TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                TextColor(Color::srgb(0.6, 0.8, 1.0)),
                LevelText,
            ));

            // XP bar background
            level_parent.spawn((
                Node {
                    width: Val::Px(60.0),
                    height: Val::Px(6.0),
                    ..default()
                },
                BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.15)),
            )).with_children(|bar_parent| {
                // XP bar fill
                bar_parent.spawn((
                    Node {
                        width: Val::Percent(0.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.4, 0.7, 1.0)),
                    XpBarFill,
                ));
            });
        });

        // Distance to target
        parent.spawn((
            Text::new("No route"),
            TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(0.8, 0.8, 0.8)),
            DistanceText,
        ));

        // Biome
        parent.spawn((
            Text::new(""),
            TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(0.6, 0.8, 0.6)),
            BiomeText,
        ));

        // Speed
        parent.spawn((
            Text::new("0.0 km/h"),
            TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(0.5, 0.9, 0.5)),
            SpeedText,
        ));
    });
}

fn update_hud(
    state: Res<MyPlayerState>,
    route: Res<DisplayRoute>,
    world: Option<Res<WorldGrid>>,
    mut gold_q: Query<&mut Text, With<GoldText>>,
    mut level_q: Query<&mut Text, (With<LevelText>, Without<GoldText>, Without<DistanceText>, Without<SpeedText>, Without<BiomeText>)>,
    mut xp_bar_q: Query<&mut Node, With<XpBarFill>>,
    mut dist_q: Query<&mut Text, (With<DistanceText>, Without<GoldText>, Without<LevelText>, Without<SpeedText>, Without<BiomeText>)>,
    mut speed_q: Query<&mut Text, (With<SpeedText>, Without<GoldText>, Without<LevelText>, Without<DistanceText>, Without<BiomeText>)>,
    mut biome_q: Query<&mut Text, (With<BiomeText>, Without<GoldText>, Without<LevelText>, Without<DistanceText>, Without<SpeedText>)>,
    mut open: ResMut<InventoryOpen>,
    mouse: Res<ButtonInput<MouseButton>>,
    inv_btn_q: Query<&Interaction, With<InventoryButton>>,
) {
    if let Ok(mut text) = gold_q.get_single_mut() {
        **text = format!("Gold: {}", state.gold);
    }
    // Level + XP bar
    let total_m = state.total_distance_m as u64;
    let level = questlib::leveling::level_from_meters(total_m);
    let progress = questlib::leveling::level_progress(total_m);
    if let Ok(mut text) = level_q.get_single_mut() {
        **text = format!("Lv {}", level);
    }
    if let Ok(mut node) = xp_bar_q.get_single_mut() {
        node.width = Val::Percent(progress * 100.0);
    }
    if let Ok(mut text) = speed_q.get_single_mut() {
        **text = format!("{:.1} km/h", state.speed_kmh);
    }
    if let Ok(mut text) = dist_q.get_single_mut() {
        if !route.waypoints.is_empty() {
            if let Some(world) = &world {
                let tile_idx = crate::terrain::path::tile_index_from_meters(&route.waypoints, state.route_meters, world);
                let remaining: u32 = route.waypoints[(tile_idx + 1).min(route.waypoints.len())..]
                    .iter()
                    .map(|&(x, y)| { let c = world.get(x, y).movement_cost(); if c == u32::MAX { 0 } else { c } })
                    .sum();
                **text = format!("{}m to target", remaining);
            }
        } else {
            **text = "No route".to_string();
        }
    }
    // Biome
    if let Ok(mut text) = biome_q.get_single_mut() {
        if let Some(world) = &world {
            let biome = world.map.biome_at(state.tile_x as usize, state.tile_y as usize);
            **text = biome.display_name().to_string();
        }
    }
    // Inventory button
    if mouse.just_pressed(MouseButton::Left) {
        for interaction in &inv_btn_q {
            if matches!(interaction, Interaction::Hovered | Interaction::Pressed) {
                open.0 = !open.0;
            }
        }
    }
}

fn detect_gold_change(
    state: Res<MyPlayerState>,
    font: Res<GameFont>,
    mut last_gold: ResMut<LastKnownGold>,
    mut commands: Commands,
    player_q: Query<&Transform, With<crate::terrain::tilemap::PlayerSprite>>,
) {
    let current_gold = state.gold;
    if current_gold > last_gold.0 && last_gold.0 > 0 {
        let delta = current_gold - last_gold.0;

        // Spawn floating text at player position
        if let Ok(player_tf) = player_q.get_single() {
            spawn_floating_text(
                &mut commands,
                &font.0,
                &format!("+{} gold", delta),
                Color::srgb(1.0, 0.85, 0.2),
                player_tf.translation,
            );
        }
    }
    last_gold.0 = current_gold;
}

fn detect_level_up(
    state: Res<MyPlayerState>,
    font: Res<GameFont>,
    mut last_level: ResMut<LastKnownLevel>,
    mut commands: Commands,
    player_q: Query<&Transform, With<crate::terrain::tilemap::PlayerSprite>>,
) {
    let current_level = questlib::leveling::level_from_meters(state.total_distance_m as u64);
    if current_level > last_level.0 && last_level.0 > 0 {
        if let Ok(player_tf) = player_q.get_single() {
            spawn_floating_text(
                &mut commands,
                &font.0,
                &format!("Level {}!", current_level),
                Color::srgb(0.4, 0.7, 1.0),
                player_tf.translation,
            );
        }
    }
    last_level.0 = current_level;
}

// ── Inventory Panel ──────────────────────────────────

#[derive(Resource)]
struct InventoryOpen(bool);

#[derive(Resource)]
pub struct ItemCatalogRes(pub questlib::items::ItemCatalog);

#[derive(Component)]
struct InventoryPanel;

#[derive(Component)]
struct InventoryContent;

/// Tracks last known inventory to detect changes without rebuilding every frame.
#[derive(Resource, Default)]
struct LastInventorySnapshot {
    inventory: Vec<questlib::items::InventorySlot>,
    equipment: questlib::items::EquipmentLoadout,
}

#[derive(Component)]
pub struct InventoryItemRow(pub String); // item_id

#[derive(Component)]
struct ItemTooltip;

#[derive(Component)]
struct InventoryCloseButton;

fn toggle_inventory(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut open: ResMut<InventoryOpen>,
    close_btn: Query<&Interaction, With<InventoryCloseButton>>,
) {
    if keys.just_pressed(KeyCode::KeyI) {
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

fn build_inventory_items(parent: &mut ChildBuilder, state: &MyPlayerState, font: &GameFont, catalog: &questlib::items::ItemCatalog) {
    // Equipment section
    parent.spawn((
        Text::new("-- Equipment --"),
        TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
        TextColor(Color::srgb(0.6, 0.6, 0.5)),
    ));
    for (label, slot) in [("Weapon", questlib::items::EquipmentSlot::Weapon), ("Armor", questlib::items::EquipmentSlot::Armor), ("Accessory", questlib::items::EquipmentSlot::Accessory)] {
        let equipped_id = state.equipment.get_slot(slot);
        let name = equipped_id
            .and_then(|id| catalog.get(id))
            .map(|d| d.display_name.as_str())
            .unwrap_or("(none)");
        let color = if equipped_id.is_some() { Color::srgb(0.7, 0.85, 1.0) } else { Color::srgb(0.4, 0.4, 0.4) };

        parent.spawn((
            Button,
            Node { padding: UiRect::axes(Val::Px(6.0), Val::Px(2.0)), ..default() },
            BackgroundColor(Color::NONE),
            InventoryItemRow(equipped_id.unwrap_or("").to_string()),
        )).with_children(|row| {
            row.spawn((
                Text::new(format!("{}: {}", label, name)),
                TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                TextColor(color),
            ));
        });
    }

    // Items section
    parent.spawn((
        Text::new("-- Items --"),
        TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
        TextColor(Color::srgb(0.6, 0.6, 0.5)),
        Node { margin: UiRect::top(Val::Px(6.0)), ..default() },
    ));

    if state.inventory.is_empty() {
        parent.spawn((
            Text::new("(empty)"),
            TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
            TextColor(Color::srgb(0.5, 0.5, 0.5)),
        ));
        return;
    }
    for slot in &state.inventory {
        let def = catalog.get(&slot.item_id);
        let name = def.map(|d| d.display_name.as_str()).unwrap_or(&slot.item_id);
        let text = if slot.quantity > 1 {
            format!("{} x{}", name, slot.quantity)
        } else {
            name.to_string()
        };
        let color = def.map(|d| match d.category {
            questlib::items::ItemCategory::Consumable => Color::srgb(0.6, 0.9, 0.6),
            questlib::items::ItemCategory::Equipment => Color::srgb(0.6, 0.7, 1.0),
            questlib::items::ItemCategory::KeyItem => Color::srgb(1.0, 0.85, 0.4),
        }).unwrap_or(Color::srgb(0.9, 0.9, 0.9));

        parent.spawn((
            Button,
            Node { padding: UiRect::axes(Val::Px(6.0), Val::Px(3.0)), ..default() },
            BackgroundColor(Color::NONE),
            InventoryItemRow(slot.item_id.clone()),
        )).with_children(|row| {
            row.spawn((
                Text::new(text),
                TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
                TextColor(color),
            ));
        });
    }
}

fn update_inventory(
    mut commands: Commands,
    open: Res<InventoryOpen>,
    state: Res<MyPlayerState>,
    session: Res<crate::GameSession>,
    font: Res<GameFont>,
    catalog: Res<ItemCatalogRes>,
    mut snapshot: ResMut<LastInventorySnapshot>,
    panel_q: Query<Entity, With<InventoryPanel>>,
    content_q: Query<Entity, With<InventoryContent>>,
) {
    if !open.0 {
        for entity in &panel_q {
            commands.entity(entity).despawn_recursive();
        }
        return;
    }

    // Spawn panel if it doesn't exist
    if panel_q.is_empty() {
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(40.0),
                right: Val::Px(12.0),
                width: Val::Px(440.0),
                min_height: Val::Px(100.0),
                max_height: Val::Px(400.0),
                padding: UiRect::all(Val::Px(12.0)),
                border: UiRect::all(Val::Px(2.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.02, 0.02, 0.08, 0.92)),
            BorderColor(Color::srgb(0.4, 0.35, 0.2)),
            BorderRadius::all(Val::Px(6.0)),
            InventoryPanel,
        )).with_children(|parent| {
            // Header row: title + close button
            let title = if !session.player_name.is_empty() {
                let champ = if session.champion.is_empty() { "adventurer" } else { session.champion.as_str() };
                format!("{} the {}", session.player_name, champ)
            } else {
                "Inventory".to_string()
            };
            parent.spawn(Node {
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                margin: UiRect::bottom(Val::Px(4.0)),
                ..default()
            }).with_children(|header| {
                header.spawn(Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(1.0),
                    ..default()
                }).with_children(|title_col| {
                    title_col.spawn((
                        Text::new(title),
                        TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                        TextColor(Color::srgb(1.0, 0.85, 0.3)),
                    ));
                    title_col.spawn((
                        Text::new("Inventory"),
                        TextFont { font: font.0.clone(), font_size: 7.0, ..default() },
                        TextColor(Color::srgb(0.55, 0.5, 0.35)),
                    ));
                });
                header.spawn((
                    Button,
                    Node { padding: UiRect::axes(Val::Px(6.0), Val::Px(2.0)), ..default() },
                    BackgroundColor(Color::srgba(0.4, 0.2, 0.2, 0.6)),
                    BorderRadius::all(Val::Px(3.0)),
                    InventoryCloseButton,
                )).with_children(|btn| {
                    btn.spawn((
                        Text::new("X"),
                        TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
                        TextColor(Color::srgb(1.0, 0.6, 0.6)),
                    ));
                });
            });
            parent.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(2.0),
                    ..default()
                },
                InventoryContent,
            )).with_children(|content| {
                build_inventory_items(content, &state, &font, &catalog.0);
            });
        });
        return;
    }

    // Only rebuild content when inventory/equipment actually changes
    let Ok(content_entity) = content_q.get_single() else { return; };
    let changed = snapshot.inventory != state.inventory || snapshot.equipment != state.equipment;
    if changed {
        snapshot.inventory = state.inventory.clone();
        snapshot.equipment = state.equipment.clone();
        commands.entity(content_entity).despawn_descendants();
        commands.entity(content_entity).with_children(|content| {
            build_inventory_items(content, &state, &font, &catalog.0);
        });
    }
}

fn show_item_tooltip(
    mut commands: Commands,
    font: Res<GameFont>,
    catalog: Res<ItemCatalogRes>,
    item_q: Query<(&Interaction, &InventoryItemRow)>,
    tooltip_q: Query<Entity, With<ItemTooltip>>,
    windows: Query<&Window>,
) {
    // Find hovered item
    let hovered = item_q.iter().find(|(i, _)| **i == Interaction::Hovered);

    // Remove old tooltip
    for entity in &tooltip_q {
        commands.entity(entity).despawn_recursive();
    }

    let Some((_, row)) = hovered else { return };
    let Some(def) = catalog.0.get(&row.0) else { return };

    // Use cursor position for tooltip placement
    let (cx, cy) = windows.get_single().ok()
        .and_then(|w| w.cursor_position())
        .map(|p| (p.x, p.y))
        .unwrap_or((100.0, 100.0));

    let category = match def.category {
        questlib::items::ItemCategory::Consumable => "Consumable",
        questlib::items::ItemCategory::Equipment => "Equipment",
        questlib::items::ItemCategory::KeyItem => "Key Item",
    };

    // Show effects in tooltip
    let mut effect_lines = Vec::new();
    for effect in &def.effects {
        match effect {
            questlib::items::ItemEffect::StatBonus { stat, amount } => {
                let sign = if *amount > 0 { "+" } else { "" };
                let stat_name = match stat {
                    questlib::items::StatType::Attack => "ATK",
                    questlib::items::StatType::Defense => "DEF",
                    questlib::items::StatType::MaxHp => "HP",
                };
                effect_lines.push(format!("{}{} {}", sign, amount, stat_name));
            }
            questlib::items::ItemEffect::Heal { amount } => {
                effect_lines.push(format!("Heals {} HP", amount));
            }
            questlib::items::ItemEffect::RevealFog { radius } => {
                effect_lines.push(format!("Reveals fog (radius {})", radius));
            }
            questlib::items::ItemEffect::SpeedMultiplier { multiplier } => {
                let pct = ((multiplier - 1.0) * 100.0).round() as i32;
                effect_lines.push(format!("+{}% movement speed", pct));
            }
            questlib::items::ItemEffect::BuffSpeed { multiplier, duration_secs } => {
                let pct = ((multiplier - 1.0) * 100.0).round() as i32;
                effect_lines.push(format!("+{}% speed for {} min", pct, duration_secs / 60));
            }
        }
    }

    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(cy + 15.0),
            left: Val::Px((cx - 140.0).max(10.0)),
            width: Val::Px(280.0),
            padding: UiRect::all(Val::Px(10.0)),
            border: UiRect::all(Val::Px(1.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.05, 0.05, 0.12, 0.95)),
        BorderColor(Color::srgb(0.5, 0.45, 0.3)),
        BorderRadius::all(Val::Px(4.0)),
        ItemTooltip,
        // High z-index so it appears on top
        ZIndex(100),
    )).with_children(|parent| {
        // Item name
        parent.spawn((
            Text::new(&def.display_name),
            TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(1.0, 0.9, 0.5)),
        ));
        // Category
        parent.spawn((
            Text::new(category),
            TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
            TextColor(Color::srgb(0.6, 0.6, 0.6)),
        ));
        // Description
        parent.spawn((
            Text::new(&def.description),
            TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
            TextColor(Color::srgb(0.85, 0.85, 0.85)),
        ));
        // Effects
        if !effect_lines.is_empty() {
            parent.spawn((
                Text::new(effect_lines.join("  ")),
                TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
                TextColor(Color::srgb(0.4, 0.9, 0.4)),
            ));
        }
    });
}

// ── Shop Button (appears when at shop POI) ──────────

fn update_shop_button(
    mut commands: Commands,
    font: Res<GameFont>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut shop: ResMut<crate::dialogue::ShopState>,
    existing: Query<Entity, With<ShopButtonRoot>>,
    btn_q: Query<&Interaction, With<ShopButton>>,
) {
    // Click to open shop
    if mouse.just_pressed(MouseButton::Left) {
        for interaction in &btn_q {
            if matches!(interaction, Interaction::Hovered | Interaction::Pressed) {
                shop.active = true;
            }
        }
    }

    let should_show = shop.available && !shop.active;

    if should_show && existing.is_empty() {
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(12.0),
                left: Val::Percent(50.0),
                margin: UiRect::left(Val::Px(-60.0)),
                width: Val::Px(120.0),
                justify_content: JustifyContent::Center,
                ..default()
            },
            ShopButtonRoot,
        )).with_children(|parent| {
            parent.spawn((
                Button,
                Node {
                    padding: UiRect::axes(Val::Px(16.0), Val::Px(8.0)),
                    border: UiRect::all(Val::Px(2.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.15, 0.12, 0.05, 0.9)),
                BorderColor(Color::srgb(0.6, 0.5, 0.2)),
                BorderRadius::all(Val::Px(6.0)),
                ShopButton,
            )).with_children(|btn| {
                btn.spawn((
                    Text::new("Shop"),
                    TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                    TextColor(Color::srgb(1.0, 0.85, 0.3)),
                ));
            });
        });
    } else if !should_show && !existing.is_empty() {
        for entity in &existing {
            commands.entity(entity).despawn_recursive();
        }
    }
}

// ── Inventory Click Handler ─────────────────────────

fn handle_inventory_click(
    mouse: Res<ButtonInput<MouseButton>>,
    catalog: Res<ItemCatalogRes>,
    session: Res<GameSession>,
    open: Res<InventoryOpen>,
    item_q: Query<(&Interaction, &InventoryItemRow)>,
) {
    if !open.0 || !mouse.just_pressed(MouseButton::Left) { return; }

    for (interaction, row) in &item_q {
        if !matches!(interaction, Interaction::Hovered | Interaction::Pressed) { continue; }
        if row.0.is_empty() { continue; } // empty equipment slot

        let Some(def) = catalog.0.get(&row.0) else { continue };
        let player_id = session.player_id.clone();
        let item_id = row.0.clone();

        match def.category {
            questlib::items::ItemCategory::Consumable => {
                // Use the item
                wasm_bindgen_futures::spawn_local(async move {
                    let client = reqwest::Client::new();
                    let _ = client.post(&crate::api_url("/use_item"))
                        .json(&serde_json::json!({"player_id": player_id, "item_id": item_id}))
                        .send().await;
                });
            }
            questlib::items::ItemCategory::Equipment => {
                // Equip the item
                wasm_bindgen_futures::spawn_local(async move {
                    let client = reqwest::Client::new();
                    let _ = client.post(&crate::api_url("/equip_item"))
                        .json(&serde_json::json!({"player_id": player_id, "item_id": item_id}))
                        .send().await;
                });
            }
            questlib::items::ItemCategory::KeyItem => {
                // Key items can't be used or equipped
            }
        }
    }
}
