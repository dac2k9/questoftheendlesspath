use std::collections::HashMap;

use anyhow::Result;
use questlib::fog::FogBitfield;
use questlib::mapgen::WorldMap;
use questlib::route::{self, position_along_route};
use questlib::supabase::SupabaseClient;
use questlib::types::PlayerUpdate;
use tracing::{debug, info};

/// Main game tick — runs every second.
///
/// For each walking player:
/// 1. Compute distance delta since last tick (from treadmill total_distance_m)
/// 2. Advance along their planned route
/// 3. Update tile position + fog of war
/// 4. Award gold
pub async fn run_tick(
    supabase: &SupabaseClient,
    game_id: &str,
    world: &WorldMap,
    player_fogs: &mut HashMap<String, FogBitfield>,
    player_last_distance: &mut HashMap<String, i32>,
) -> Result<()> {
    let players = supabase.read_players(game_id).await?;

    for player in &players {
        // Initialize fog for new players
        if !player_fogs.contains_key(&player.id) {
            let fog = if let Some(ref encoded) = player.revealed_tiles {
                FogBitfield::from_base64(encoded).unwrap_or_default()
            } else {
                FogBitfield::new()
            };
            player_fogs.insert(player.id.clone(), fog);
        }

        // Initialize last distance
        if !player_last_distance.contains_key(&player.id) {
            player_last_distance.insert(player.id.clone(), player.total_distance_m);
        }

        if !player.is_walking {
            // Update last distance even when not walking (treadmill might reset)
            player_last_distance.insert(player.id.clone(), player.total_distance_m);
            continue;
        }

        // ── Distance Delta ────────────────────────────────
        let last_dist = *player_last_distance.get(&player.id).unwrap_or(&0);
        let delta_m = (player.total_distance_m - last_dist).max(0);
        player_last_distance.insert(player.id.clone(), player.total_distance_m);

        if delta_m == 0 {
            continue;
        }

        // ── Route Advancement ─────────────────────────────
        let route = player.planned_route.as_deref()
            .and_then(route::parse_route_json)
            .unwrap_or_default();

        if route.is_empty() {
            // No route planned — just earn gold
            award_gold(supabase, player, delta_m).await?;
            continue;
        }

        // Advance meters along the route
        let prev_meters = player.route_meters_walked.unwrap_or(0.0);
        let new_meters = prev_meters + delta_m as f64;

        let (tile_x, tile_y, _idx, route_complete) =
            position_along_route(&route, new_meters, world);

        let prev_x = player.map_tile_x.unwrap_or(0) as usize;
        let prev_y = player.map_tile_y.unwrap_or(0) as usize;
        let moved = tile_x != prev_x || tile_y != prev_y;

        // ── Fog of War ────────────────────────────────────
        let fog = player_fogs.get_mut(&player.id).unwrap();
        let fog_changed = fog.reveal_radius(tile_x, tile_y, 5);

        // ── Build Update ──────────────────────────────────
        let mut update = PlayerUpdate {
            route_meters_walked: Some(new_meters),
            ..Default::default()
        };

        if moved {
            update.map_tile_x = Some(tile_x as i32);
            update.map_tile_y = Some(tile_y as i32);
            debug!(
                "{} moved to ({},{}) [{:.0}m walked]",
                player.name, tile_x, tile_y, new_meters
            );
        }

        if fog_changed {
            update.revealed_tiles = Some(fog.to_base64());
        }

        if route_complete {
            info!("{} reached route destination at ({},{})", player.name, tile_x, tile_y);
            // Clear the route — player stays here and earns gold
            update.planned_route = Some(String::new());
            update.route_meters_walked = Some(0.0);
        }

        supabase.upsert_player(&player.id, &update).await?;

        // ── Gold ──────────────────────────────────────────
        award_gold(supabase, player, delta_m).await?;
    }

    Ok(())
}

/// Award gold based on distance walked.
/// Base rate: 1 gold per 10 meters walked.
async fn award_gold(
    supabase: &SupabaseClient,
    player: &questlib::types::Player,
    delta_m: i32,
) -> Result<()> {
    if delta_m <= 0 {
        return Ok(());
    }

    let gold_earned = (delta_m / 10).max(1);
    let new_gold = player.gold + gold_earned;

    supabase
        .upsert_player(
            &player.id,
            &PlayerUpdate {
                gold: Some(new_gold),
                ..Default::default()
            },
        )
        .await?;

    debug!("{} earned {} gold (total: {})", player.name, gold_earned, new_gold);
    Ok(())
}
