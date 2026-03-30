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
    state: Res<MyPlayerState>,
    route: Res<DisplayRoute>,
    world: Option<Res<WorldGrid>>,
    mut gold_q: Query<&mut Text, With<GoldText>>,
    mut dist_q: Query<&mut Text, (With<DistanceText>, Without<GoldText>, Without<SpeedText>)>,
    mut speed_q: Query<&mut Text, (With<SpeedText>, Without<GoldText>, Without<DistanceText>)>,
) {
    if let Ok(mut text) = gold_q.get_single_mut() {
        **text = format!("Gold: {}", state.gold);
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
