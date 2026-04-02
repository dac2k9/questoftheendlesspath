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
        app.insert_resource(InventoryOpen(false))
            .add_systems(OnEnter(AppState::InGame), spawn_hud)
            .add_systems(
                Update,
                (update_hud, detect_gold_change, detect_level_up, update_floating_texts, toggle_inventory, update_inventory)
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
    inv_btn_q: Query<&Interaction, (With<InventoryButton>, Changed<Interaction>)>,
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
    for interaction in &inv_btn_q {
        if *interaction == Interaction::Pressed {
            open.0 = !open.0;
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

#[derive(Component)]
struct InventoryPanel;

#[derive(Component)]
struct InventoryContent;

fn toggle_inventory(
    keys: Res<ButtonInput<KeyCode>>,
    mut open: ResMut<InventoryOpen>,
) {
    if keys.just_pressed(KeyCode::KeyI) {
        open.0 = !open.0;
    }
}

fn build_inventory_items(parent: &mut ChildBuilder, state: &MyPlayerState, font: &GameFont) {
    if state.inventory.is_empty() {
        parent.spawn((
            Text::new("(empty)"),
            TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
            TextColor(Color::srgb(0.5, 0.5, 0.5)),
        ));
        return;
    }
    for slot in &state.inventory {
        let text = if slot.quantity > 1 {
            format!("{} x{}", slot.item_id, slot.quantity)
        } else {
            slot.item_id.clone()
        };
        parent.spawn((
            Text::new(text),
            TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
            TextColor(Color::srgb(0.9, 0.9, 0.9)),
        ));
    }
}

fn update_inventory(
    mut commands: Commands,
    open: Res<InventoryOpen>,
    state: Res<MyPlayerState>,
    font: Res<GameFont>,
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
                width: Val::Px(220.0),
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
            parent.spawn((
                Text::new("Inventory"),
                TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                TextColor(Color::srgb(1.0, 0.85, 0.3)),
            ));
            parent.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(2.0),
                    ..default()
                },
                InventoryContent,
            )).with_children(|content| {
                build_inventory_items(content, &state, &font);
            });
        });
        return;
    }

    // Update content
    let Ok(content_entity) = content_q.get_single() else { return; };
    commands.entity(content_entity).despawn_descendants();
    commands.entity(content_entity).with_children(|content| {
        build_inventory_items(content, &state, &font);
    });
}
