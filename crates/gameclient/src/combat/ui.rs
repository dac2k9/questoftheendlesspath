//! Combat overlay UI — auto-attack display with run away button.

use bevy::prelude::*;

use super::CombatUiState;
use crate::GameFont;

// ── Marker Components ────────────────────────────────

#[derive(Component)]
pub(crate) struct CombatOverlay;

#[derive(Component)]
pub(crate) struct EnemyNameText;

#[derive(Component)]
pub(crate) struct EnemyHpText;

#[derive(Component)]
pub(crate) struct EnemyHpBarFill;

#[derive(Component)]
pub(crate) struct EnemyChargeBarFill;

#[derive(Component)]
pub(crate) struct PlayerNameText;

#[derive(Component)]
pub(crate) struct PlayerHpText;

#[derive(Component)]
pub(crate) struct PlayerHpBarFill;

#[derive(Component)]
pub(crate) struct PlayerChargeBarFill;

#[derive(Component)]
pub(crate) struct CombatLogText;

#[derive(Component)]
pub(crate) struct FleeButton;

#[derive(Component)]
pub(crate) struct LevelInfoText;

#[derive(Component)]
pub(crate) struct EncounterDescText;

// ── Colors ───────────────────────────────────────────

const BG_COLOR: Color = Color::srgba(0.02, 0.02, 0.08, 0.95);
const BORDER_COLOR: Color = Color::srgb(0.4, 0.35, 0.2);
const ENEMY_HP_COLOR: Color = Color::srgb(0.8, 0.2, 0.2);
const PLAYER_HP_COLOR: Color = Color::srgb(0.2, 0.7, 0.3);
const CHARGE_COLOR: Color = Color::srgb(0.3, 0.5, 0.9);
const BAR_BG_COLOR: Color = Color::srgba(1.0, 1.0, 1.0, 0.12);
const TEXT_COLOR: Color = Color::srgb(0.9, 0.9, 0.9);
const GOLD_COLOR: Color = Color::srgb(1.0, 0.85, 0.3);
const LOG_COLOR: Color = Color::srgb(0.7, 0.7, 0.6);
const DIM_TEXT: Color = Color::srgb(0.5, 0.5, 0.5);
const BTN_COLOR: Color = Color::srgb(0.25, 0.12, 0.12);
const BTN_HOVER: Color = Color::srgb(0.4, 0.15, 0.15);

// ── Spawn / Despawn ──────────────────────────────────

pub fn manage_combat_overlay(
    mut commands: Commands,
    combat: Res<CombatUiState>,
    font: Res<GameFont>,
    existing: Query<Entity, With<CombatOverlay>>,
) {
    let should_show = combat.active && combat.state.is_some();

    if should_show && existing.is_empty() {
        spawn_overlay(&mut commands, &font.0);
    } else if !should_show && !existing.is_empty() {
        for entity in &existing {
            commands.entity(entity).despawn_recursive();
        }
    }
}

fn spawn_overlay(commands: &mut Commands, font: &Handle<Font>) {
    let font_10 = TextFont { font: font.clone(), font_size: 10.0, ..default() };
    let font_8 = TextFont { font: font.clone(), font_size: 8.0, ..default() };
    let font_7 = TextFont { font: font.clone(), font_size: 7.0, ..default() };

    // Main overlay — compact bar at bottom
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            bottom: Val::Px(0.0),
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(Val::Px(12.0)),
            border: UiRect::top(Val::Px(2.0)),
            ..default()
        },
        BackgroundColor(BG_COLOR),
        BorderColor(BORDER_COLOR),
        CombatOverlay,
    )).with_children(|parent| {
        // Encounter description (shown once at start)
        parent.spawn((
            Text::new(""),
            font_8.clone(),
            TextColor(Color::srgb(0.9, 0.8, 0.5)),
            Node { margin: UiRect::bottom(Val::Px(6.0)), ..default() },
            EncounterDescText,
        ));

        // ── Main row: Player (left) | Log (center) | Enemy (right) ──
        parent.spawn(Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(16.0),
            align_items: AlignItems::FlexStart,
            ..default()
        }).with_children(|row| {
            // ── Player (left) ──
            row.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(3.0),
                width: Val::Percent(30.0),
                ..default()
            }).with_children(|player| {
                player.spawn((
                    Text::new("Player"),
                    font_10.clone(),
                    TextColor(GOLD_COLOR),
                    PlayerNameText,
                ));
                player.spawn((
                    Text::new("HP: 0/0"),
                    font_8.clone(),
                    TextColor(TEXT_COLOR),
                    PlayerHpText,
                ));
                spawn_bar(player, PLAYER_HP_COLOR, PlayerHpBarFill);
                spawn_bar(player, CHARGE_COLOR, PlayerChargeBarFill);
            });

            // ── Combat Log (center) ──
            row.spawn(Node {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                justify_content: JustifyContent::Center,
                ..default()
            }).with_children(|log| {
                log.spawn((
                    Text::new(""),
                    font_7.clone(),
                    TextColor(LOG_COLOR),
                    CombatLogText,
                ));
            });

            // ── Enemy (right) ──
            row.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(3.0),
                width: Val::Percent(30.0),
                align_items: AlignItems::FlexEnd,
                ..default()
            }).with_children(|enemy| {
                enemy.spawn((
                    Text::new("Enemy"),
                    font_10.clone(),
                    TextColor(Color::srgb(1.0, 0.4, 0.3)),
                    EnemyNameText,
                ));
                enemy.spawn((
                    Text::new("HP: 0/0"),
                    font_8.clone(),
                    TextColor(TEXT_COLOR),
                    EnemyHpText,
                ));
                spawn_bar(enemy, ENEMY_HP_COLOR, EnemyHpBarFill);
                spawn_bar(enemy, CHARGE_COLOR, EnemyChargeBarFill);
            });
        });

        // ── Bottom row: level info + flee button ──
        parent.spawn(Node {
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            margin: UiRect::top(Val::Px(6.0)),
            ..default()
        }).with_children(|bottom| {
            bottom.spawn((
                Text::new(""),
                font_7.clone(),
                TextColor(DIM_TEXT),
                LevelInfoText,
            ));

            bottom.spawn((
                Button,
                Node {
                    padding: UiRect::axes(Val::Px(16.0), Val::Px(6.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(BTN_COLOR),
                BorderColor(BORDER_COLOR),
                BorderRadius::all(Val::Px(4.0)),
                FleeButton,
            )).with_children(|btn| {
                btn.spawn((
                    Text::new("RUN AWAY"),
                    font_8.clone(),
                    TextColor(TEXT_COLOR),
                ));
            });
        });
    });
}

fn spawn_bar(parent: &mut ChildBuilder, fill_color: Color, marker: impl Component) {
    parent.spawn((
        Node {
            width: Val::Percent(100.0),
            height: Val::Px(8.0),
            ..default()
        },
        BackgroundColor(BAR_BG_COLOR),
        BorderRadius::all(Val::Px(2.0)),
    )).with_children(|bar| {
        bar.spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(fill_color),
            BorderRadius::all(Val::Px(2.0)),
            marker,
        ));
    });
}

// ── Update UI ────────────────────────────────────────

pub fn update_combat_ui(
    combat: Res<CombatUiState>,
    mut enemy_name_q: Query<&mut Text, (With<EnemyNameText>, Without<EnemyHpText>, Without<PlayerNameText>, Without<PlayerHpText>, Without<CombatLogText>, Without<LevelInfoText>)>,
    mut enemy_hp_q: Query<&mut Text, (With<EnemyHpText>, Without<EnemyNameText>, Without<PlayerNameText>, Without<PlayerHpText>, Without<CombatLogText>, Without<LevelInfoText>)>,
    mut enemy_hp_bar: Query<&mut Node, (With<EnemyHpBarFill>, Without<EnemyChargeBarFill>, Without<PlayerHpBarFill>, Without<PlayerChargeBarFill>)>,
    mut enemy_charge_bar: Query<&mut Node, (With<EnemyChargeBarFill>, Without<EnemyHpBarFill>, Without<PlayerHpBarFill>, Without<PlayerChargeBarFill>)>,
    mut player_name_q: Query<&mut Text, (With<PlayerNameText>, Without<EnemyNameText>, Without<EnemyHpText>, Without<PlayerHpText>, Without<CombatLogText>, Without<LevelInfoText>)>,
    mut player_hp_q: Query<&mut Text, (With<PlayerHpText>, Without<EnemyNameText>, Without<EnemyHpText>, Without<PlayerNameText>, Without<CombatLogText>, Without<LevelInfoText>)>,
    mut player_hp_bar: Query<&mut Node, (With<PlayerHpBarFill>, Without<EnemyHpBarFill>, Without<EnemyChargeBarFill>, Without<PlayerChargeBarFill>)>,
    mut player_charge_bar: Query<&mut Node, (With<PlayerChargeBarFill>, Without<EnemyHpBarFill>, Without<EnemyChargeBarFill>, Without<PlayerHpBarFill>)>,
    mut log_q: Query<&mut Text, (With<CombatLogText>, Without<EnemyNameText>, Without<EnemyHpText>, Without<PlayerNameText>, Without<PlayerHpText>, Without<LevelInfoText>)>,
    mut level_info_q: Query<&mut Text, (With<LevelInfoText>, Without<EnemyNameText>, Without<EnemyHpText>, Without<PlayerNameText>, Without<PlayerHpText>, Without<CombatLogText>, Without<EncounterDescText>)>,
    mut desc_q: Query<&mut Text, (With<EncounterDescText>, Without<EnemyNameText>, Without<EnemyHpText>, Without<PlayerNameText>, Without<PlayerHpText>, Without<CombatLogText>, Without<LevelInfoText>)>,
    mut flee_btn_q: Query<(&Interaction, &mut BackgroundColor), With<FleeButton>>,
) {
    let Some(ref cs) = combat.state else { return };

    // Enemy section
    if let Ok(mut text) = enemy_name_q.get_single_mut() {
        **text = cs.enemy_name.clone();
    }
    if let Ok(mut text) = enemy_hp_q.get_single_mut() {
        **text = format!("HP: {}/{}", cs.enemy_hp, cs.enemy_max_hp);
    }
    if let Ok(mut node) = enemy_hp_bar.get_single_mut() {
        let pct = if cs.enemy_max_hp > 0 { cs.enemy_hp as f32 / cs.enemy_max_hp as f32 } else { 0.0 };
        node.width = Val::Percent(pct * 100.0);
    }
    if let Ok(mut node) = enemy_charge_bar.get_single_mut() {
        node.width = Val::Percent(combat.local_enemy_charge * 100.0);
    }

    // Player section
    if let Ok(mut text) = player_name_q.get_single_mut() {
        **text = format!("Lv {} Adventurer", cs.player_level);
    }
    if let Ok(mut text) = player_hp_q.get_single_mut() {
        **text = format!("HP: {}/{}", cs.player_hp, cs.player_max_hp);
    }
    if let Ok(mut node) = player_hp_bar.get_single_mut() {
        let pct = if cs.player_max_hp > 0 { cs.player_hp as f32 / cs.player_max_hp as f32 } else { 0.0 };
        node.width = Val::Percent(pct * 100.0);
    }
    if let Ok(mut node) = player_charge_bar.get_single_mut() {
        node.width = Val::Percent(combat.local_player_charge * 100.0);
    }

    // Combat log
    if let Ok(mut text) = log_q.get_single_mut() {
        let entries: Vec<&str> = cs.turn_log.iter().rev().take(4).map(|e| e.message.as_str()).collect();
        let reversed: Vec<&str> = entries.into_iter().rev().collect();
        **text = reversed.join("\n");
    }

    // Level info
    if let Ok(mut text) = level_info_q.get_single_mut() {
        **text = format!("Min Lv {} / Rec Lv {}", cs.min_level, cs.recommended_level);
    }

    // Encounter description
    if let Ok(mut text) = desc_q.get_single_mut() {
        **text = cs.description.clone();
    }

    // Flee button hover
    for (interaction, mut bg) in &mut flee_btn_q {
        *bg = match interaction {
            Interaction::Hovered | Interaction::Pressed => BackgroundColor(BTN_HOVER),
            _ => BackgroundColor(BTN_COLOR),
        };
    }
}

// ── Input Handling ───────────────────────────────────

pub fn handle_combat_input(
    mut combat: ResMut<CombatUiState>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    flee_btn: Query<&Interaction, With<FleeButton>>,
) {
    let Some(ref cs) = combat.state else { return };
    if cs.status != questlib::combat::CombatStatus::Fighting || combat.action_pending {
        return;
    }

    let mut should_flee = false;

    // ESC or R to run away
    if keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::KeyR) {
        should_flee = true;
    }

    // Click flee button
    if mouse.just_pressed(MouseButton::Left) {
        if let Ok(interaction) = flee_btn.get_single() {
            if matches!(interaction, Interaction::Hovered | Interaction::Pressed) {
                should_flee = true;
            }
        }
    }

    if should_flee {
        combat.action_pending = true;
        super::poll::send_flee(combat.fetched.clone());
    }
}
