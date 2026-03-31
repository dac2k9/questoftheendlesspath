//! Combat overlay UI — spawns/despawns the popup, renders bars and buttons.

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
pub(crate) struct AttackButton;

#[derive(Component)]
pub(crate) struct DefendButton;

#[derive(Component)]
pub(crate) struct ActionButtonsContainer;

#[derive(Component)]
pub(crate) struct VictoryText;

/// Timer for auto-closing after victory/defeat.
#[derive(Resource)]
pub(crate) struct CombatEndTimer(Option<Timer>);

impl Default for CombatEndTimer {
    fn default() -> Self { Self(None) }
}

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
const BTN_COLOR: Color = Color::srgb(0.15, 0.15, 0.25);
const BTN_HOVER: Color = Color::srgb(0.25, 0.25, 0.4);

// ── Spawn / Despawn ──────────────────────────────────

pub fn manage_combat_overlay(
    mut commands: Commands,
    combat: Res<CombatUiState>,
    font: Res<GameFont>,
    existing: Query<Entity, With<CombatOverlay>>,
    mut end_timer: Local<Option<Timer>>,
    time: Res<Time>,
) {
    let should_show = combat.active && combat.state.is_some();

    // Handle victory/defeat auto-close
    if let Some(ref cs) = combat.state {
        use questlib::combat::CombatStatus;
        if cs.status == CombatStatus::Victory {
            if end_timer.is_none() {
                *end_timer = Some(Timer::from_seconds(3.0, TimerMode::Once));
            }
        }
    }
    if let Some(ref mut timer) = *end_timer {
        timer.tick(time.delta());
        if timer.finished() {
            *end_timer = None;
            // Combat ended — will be cleaned up when server returns null
        }
    }

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
    let font_9 = TextFont { font: font.clone(), font_size: 9.0, ..default() };

    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Percent(5.0),
            left: Val::Percent(10.0),
            right: Val::Percent(10.0),
            bottom: Val::Percent(5.0),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::SpaceBetween,
            padding: UiRect::all(Val::Px(16.0)),
            border: UiRect::all(Val::Px(2.0)),
            ..default()
        },
        BackgroundColor(BG_COLOR),
        BorderColor(BORDER_COLOR),
        BorderRadius::all(Val::Px(6.0)),
        CombatOverlay,
    )).with_children(|parent| {
        // ── Enemy Section (top) ──
        parent.spawn(Node {
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
            ..default()
        }).with_children(|enemy| {
            // Name + HP text row
            enemy.spawn(Node {
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            }).with_children(|row| {
                row.spawn((
                    Text::new("Enemy"),
                    font_10.clone(),
                    TextColor(GOLD_COLOR),
                    EnemyNameText,
                ));
                row.spawn((
                    Text::new("HP: 0/0"),
                    font_8.clone(),
                    TextColor(TEXT_COLOR),
                    EnemyHpText,
                ));
            });

            // Enemy HP bar
            spawn_bar(enemy, ENEMY_HP_COLOR, EnemyHpBarFill);
            // Enemy charge bar
            spawn_bar(enemy, CHARGE_COLOR, EnemyChargeBarFill);
        });

        // ── Combat Log (middle) ──
        parent.spawn(Node {
            flex_direction: FlexDirection::Column,
            min_height: Val::Px(60.0),
            justify_content: JustifyContent::Center,
            ..default()
        }).with_children(|log| {
            log.spawn((
                Text::new(""),
                font_8.clone(),
                TextColor(LOG_COLOR),
                CombatLogText,
            ));
        });

        // ── Player Section (bottom) ──
        parent.spawn(Node {
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
            ..default()
        }).with_children(|player| {
            // Name + HP text row
            player.spawn(Node {
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            }).with_children(|row| {
                row.spawn((
                    Text::new("Player"),
                    font_10.clone(),
                    TextColor(GOLD_COLOR),
                    PlayerNameText,
                ));
                row.spawn((
                    Text::new("HP: 0/0"),
                    font_8.clone(),
                    TextColor(TEXT_COLOR),
                    PlayerHpText,
                ));
            });

            // Player HP bar
            spawn_bar(player, PLAYER_HP_COLOR, PlayerHpBarFill);
            // Player charge bar
            spawn_bar(player, CHARGE_COLOR, PlayerChargeBarFill);

            // Action buttons (hidden until player turn)
            player.spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::Center,
                    column_gap: Val::Px(16.0),
                    margin: UiRect::top(Val::Px(8.0)),
                    ..default()
                },
                Visibility::Hidden,
                ActionButtonsContainer,
            )).with_children(|buttons| {
                spawn_action_button(buttons, font, "ATTACK", AttackButton);
                spawn_action_button(buttons, font, "DEFEND", DefendButton);
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

fn spawn_action_button(parent: &mut ChildBuilder, font: &Handle<Font>, label: &str, marker: impl Component) {
    parent.spawn((
        Button,
        Node {
            padding: UiRect::axes(Val::Px(20.0), Val::Px(8.0)),
            border: UiRect::all(Val::Px(1.0)),
            ..default()
        },
        BackgroundColor(BTN_COLOR),
        BorderColor(BORDER_COLOR),
        BorderRadius::all(Val::Px(4.0)),
        marker,
    )).with_children(|btn| {
        btn.spawn((
            Text::new(label),
            TextFont { font: font.clone(), font_size: 9.0, ..default() },
            TextColor(TEXT_COLOR),
        ));
    });
}

// ── Update UI ────────────────────────────────────────

pub fn update_combat_ui(
    combat: Res<CombatUiState>,
    mut enemy_name_q: Query<&mut Text, (With<EnemyNameText>, Without<EnemyHpText>, Without<PlayerNameText>, Without<PlayerHpText>, Without<CombatLogText>)>,
    mut enemy_hp_q: Query<&mut Text, (With<EnemyHpText>, Without<EnemyNameText>, Without<PlayerNameText>, Without<PlayerHpText>, Without<CombatLogText>)>,
    mut enemy_hp_bar: Query<&mut Node, (With<EnemyHpBarFill>, Without<EnemyChargeBarFill>, Without<PlayerHpBarFill>, Without<PlayerChargeBarFill>)>,
    mut enemy_charge_bar: Query<&mut Node, (With<EnemyChargeBarFill>, Without<EnemyHpBarFill>, Without<PlayerHpBarFill>, Without<PlayerChargeBarFill>)>,
    mut player_name_q: Query<&mut Text, (With<PlayerNameText>, Without<EnemyNameText>, Without<EnemyHpText>, Without<PlayerHpText>, Without<CombatLogText>)>,
    mut player_hp_q: Query<&mut Text, (With<PlayerHpText>, Without<EnemyNameText>, Without<EnemyHpText>, Without<PlayerNameText>, Without<CombatLogText>)>,
    mut player_hp_bar: Query<&mut Node, (With<PlayerHpBarFill>, Without<EnemyHpBarFill>, Without<EnemyChargeBarFill>, Without<PlayerChargeBarFill>)>,
    mut player_charge_bar: Query<&mut Node, (With<PlayerChargeBarFill>, Without<EnemyHpBarFill>, Without<EnemyChargeBarFill>, Without<PlayerHpBarFill>)>,
    mut log_q: Query<&mut Text, (With<CombatLogText>, Without<EnemyNameText>, Without<EnemyHpText>, Without<PlayerNameText>, Without<PlayerHpText>)>,
    mut buttons_q: Query<&mut Visibility, With<ActionButtonsContainer>>,
    mut attack_btn_q: Query<(&Interaction, &mut BackgroundColor), (With<AttackButton>, Without<DefendButton>)>,
    mut defend_btn_q: Query<(&Interaction, &mut BackgroundColor), (With<DefendButton>, Without<AttackButton>)>,
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

    // Combat log — show last 3 entries
    if let Ok(mut text) = log_q.get_single_mut() {
        let entries: Vec<&str> = cs.turn_log.iter().rev().take(3).map(|e| e.message.as_str()).collect();
        let reversed: Vec<&str> = entries.into_iter().rev().collect();
        **text = reversed.join("\n");
    }

    // Action buttons visibility
    let show_buttons = cs.status == questlib::combat::CombatStatus::PlayerTurn;
    if let Ok(mut vis) = buttons_q.get_single_mut() {
        *vis = if show_buttons { Visibility::Inherited } else { Visibility::Hidden };
    }

    // Button hover effects
    for (interaction, mut bg) in &mut attack_btn_q {
        *bg = match interaction {
            Interaction::Hovered | Interaction::Pressed => BackgroundColor(BTN_HOVER),
            _ => BackgroundColor(BTN_COLOR),
        };
    }
    for (interaction, mut bg) in &mut defend_btn_q {
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
    attack_btn: Query<&Interaction, (With<AttackButton>, Changed<Interaction>)>,
    defend_btn: Query<&Interaction, (With<DefendButton>, Changed<Interaction>)>,
) {
    let Some(ref cs) = combat.state else { return };
    if cs.status != questlib::combat::CombatStatus::PlayerTurn || combat.action_pending {
        return;
    }

    let mut action: Option<&str> = None;

    // Keyboard
    if keys.just_pressed(KeyCode::Digit1) || keys.just_pressed(KeyCode::KeyA)
        || keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space)
    {
        action = Some("attack");
    }
    if keys.just_pressed(KeyCode::Digit2) || keys.just_pressed(KeyCode::KeyD) {
        action = Some("defend");
    }

    // Mouse buttons
    if let Ok(interaction) = attack_btn.get_single() {
        if *interaction == Interaction::Pressed {
            action = Some("attack");
        }
    }
    if let Ok(interaction) = defend_btn.get_single() {
        if *interaction == Interaction::Pressed {
            action = Some("defend");
        }
    }

    if let Some(act) = action {
        combat.action_pending = true;
        super::poll::send_combat_action(act, combat.fetched.clone());
    }
}
