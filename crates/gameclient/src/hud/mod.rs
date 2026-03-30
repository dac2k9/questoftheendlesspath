pub mod floating_text;

use bevy::prelude::*;

use crate::states::AppState;
use crate::supabase::PolledPlayerState;
use crate::terrain::path::PlannedRoute;
use crate::terrain::world::WorldGrid;
use crate::{GameFont, GameSession};
use floating_text::{spawn_floating_text, update_floating_texts};

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::InGame), spawn_hud)
            .add_systems(
                Update,
                (update_hud, detect_gold_change, update_floating_texts)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Component)]
struct HudRoot;

#[derive(Component)]
struct GoldText;

#[derive(Component)]
struct DistanceText;

#[derive(Component)]
struct SpeedText;

#[derive(Resource, Default)]
struct LastKnownGold(i32);

fn spawn_hud(mut commands: Commands, font: Res<GameFont>) {
    commands.insert_resource(LastKnownGold::default());

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
        // Gold
        parent.spawn((
            Text::new("Gold: 0"),
            TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(1.0, 0.85, 0.2)),
            GoldText,
        ));

        // Distance to target
        parent.spawn((
            Text::new("No route"),
            TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(0.8, 0.8, 0.8)),
            DistanceText,
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
    polled: Res<PolledPlayerState>,
    session: Res<GameSession>,
    route: Res<PlannedRoute>,
    world: Option<Res<WorldGrid>>,
    mut gold_q: Query<&mut Text, With<GoldText>>,
    mut dist_q: Query<&mut Text, (With<DistanceText>, Without<GoldText>, Without<SpeedText>)>,
    mut speed_q: Query<&mut Text, (With<SpeedText>, Without<GoldText>, Without<DistanceText>)>,
) {
    let Ok(players) = polled.players.lock() else { return };
    let Some(me) = players.iter().find(|p| p.name.eq_ignore_ascii_case(&session.player_name)) else { return };

    // Gold
    if let Ok(mut text) = gold_q.get_single_mut() {
        **text = format!("Gold: {}", me.gold);
    }

    // Speed
    if let Ok(mut text) = speed_q.get_single_mut() {
        **text = format!("{:.1} km/h", me.current_speed_kmh);
    }

    // Distance to target
    if let Ok(mut text) = dist_q.get_single_mut() {
        if route.waypoints.len() > route.current_index + 1 {
            // Sum remaining tile costs
            let remaining: u32 = if let Some(world) = &world {
                route.waypoints[route.current_index..]
                    .iter()
                    .map(|&(x, y)| {
                        let terrain = world.get(x, y);
                        let cost = terrain.movement_cost();
                        if cost == u32::MAX { 0 } else { cost }
                    })
                    .sum()
            } else {
                0
            };
            **text = format!("{}m to target", remaining);
        } else {
            **text = "No route".to_string();
        }
    }
}

fn detect_gold_change(
    polled: Res<PolledPlayerState>,
    session: Res<GameSession>,
    font: Res<GameFont>,
    mut last_gold: ResMut<LastKnownGold>,
    mut commands: Commands,
    player_q: Query<&Transform, With<crate::terrain::tilemap::PlayerSprite>>,
) {
    let Ok(players) = polled.players.lock() else { return };
    let Some(me) = players.iter().find(|p| p.name.eq_ignore_ascii_case(&session.player_name)) else { return };

    let current_gold = me.gold;
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
