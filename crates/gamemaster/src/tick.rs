use std::collections::HashMap;

use anyhow::Result;
use questlib::events::{EventCatalog, EventOutcome, EventStatus, TriggerContext};
use questlib::fog::FogBitfield;
use questlib::mapgen::WorldMap;
use questlib::route::{self, position_along_route};
use tracing::{debug, info};

use crate::devserver::SharedState;
use crate::SharedEvents;

/// Dev mode tick — works with in-memory shared state.
pub fn run_tick_dev(
    state: &SharedState,
    world: &WorldMap,
    shared_events: &SharedEvents,
    player_fogs: &mut HashMap<String, FogBitfield>,
    player_last_distance: &mut HashMap<String, i32>,
    rng_roll: f32,
) -> Result<()> {
    let mut lock = state.lock().map_err(|e| anyhow::anyhow!("lock error: {e}"))?;
    let mut events_lock = shared_events.lock().map_err(|e| anyhow::anyhow!("events lock: {e}"))?;

    let player_ids: Vec<String> = lock.keys().cloned().collect();

    for player_id in &player_ids {
        let player = lock.get(player_id).unwrap().clone();

        // Initialize fog
        if !player_fogs.contains_key(player_id) {
            let fog = if !player.revealed_tiles.is_empty() {
                FogBitfield::from_base64(&player.revealed_tiles).unwrap_or_default()
            } else {
                let mut f = FogBitfield::new();
                f.reveal_radius(player.map_tile_x as usize, player.map_tile_y as usize, 5);
                f
            };
            player_fogs.insert(player_id.clone(), fog);
        }

        if !player_last_distance.contains_key(player_id) {
            player_last_distance.insert(player_id.clone(), player.total_distance_m);
        }

        if !player.is_walking {
            player_last_distance.insert(player_id.clone(), player.total_distance_m);
            continue;
        }

        // Distance delta
        let last_dist = *player_last_distance.get(player_id).unwrap_or(&0);
        let delta_m = (player.total_distance_m - last_dist).max(0);
        player_last_distance.insert(player_id.clone(), player.total_distance_m);

        if delta_m == 0 {
            continue;
        }

        // Route advancement
        let route_tiles = if !player.planned_route.is_empty() {
            route::parse_route_json(&player.planned_route).unwrap_or_default()
        } else {
            Vec::new()
        };

        let p = lock.get_mut(player_id).unwrap();

        if route_tiles.is_empty() {
            let gold_earned = (delta_m / 10).max(1);
            p.gold += gold_earned;
            debug!("{} earned {} gold (no route)", p.name, gold_earned);
            continue;
        }

        let new_meters = p.route_meters_walked + delta_m as f64;
        let (tile_x, tile_y, _idx, route_complete) =
            position_along_route(&route_tiles, new_meters, world);

        let moved = tile_x as i32 != p.map_tile_x || tile_y as i32 != p.map_tile_y;
        p.route_meters_walked = new_meters;

        if moved {
            p.map_tile_x = tile_x as i32;
            p.map_tile_y = tile_y as i32;
            debug!("{} moved to ({},{}) [{:.0}m walked]", p.name, tile_x, tile_y, new_meters);
        }

        // Fog
        let fog = player_fogs.get_mut(player_id).unwrap();
        let fog_changed = fog.reveal_radius(tile_x, tile_y, 5);
        if fog_changed {
            p.revealed_tiles = fog.to_base64();
        }

        if route_complete {
            info!("{} reached destination at ({},{})", p.name, tile_x, tile_y);
            p.planned_route = String::new();
            p.route_meters_walked = 0.0;
        }

        // Gold
        let gold_earned = (delta_m / 10).max(1);
        p.gold += gold_earned;

        // ── Event Triggers ────────────────────────────────
        let poi_id = world.poi_at(tile_x, tile_y).map(|poi| poi.id);
        let biome = world.biome_at(tile_x, tile_y);

        let ctx = TriggerContext {
            player_tile: (tile_x, tile_y),
            player_poi: poi_id,
            player_biome: biome,
            total_distance_m: p.total_distance_m as u32,
            inventory: vec![], // TODO: parse from player inventory
            completed_events: events_lock.completed_ids(),
            rng_roll,
        };

        // Find triggered events
        let triggered_ids: Vec<String> = events_lock
            .check_triggers(&ctx)
            .iter()
            .map(|e| e.id.clone())
            .collect();

        for event_id in &triggered_ids {
            let event = events_lock.get_mut(event_id).unwrap();

            if event.transition(EventStatus::Active).is_ok() {
                info!("Event triggered for {}: {} ({})", p.name, event.name, event.id);

                // Auto-complete non-browser events
                if event.auto_completes() {
                    if event.transition(EventStatus::Completed).is_ok() {
                        info!("  Auto-completed: {}", event.name);

                        // Apply outcomes
                        for outcome in &event.outcomes {
                            apply_outcome(outcome, p, fog);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Apply an event outcome to a player.
fn apply_outcome(
    outcome: &EventOutcome,
    player: &mut crate::devserver::DevPlayerState,
    fog: &mut FogBitfield,
) {
    match outcome {
        EventOutcome::Gold { amount } => {
            player.gold += amount;
            info!("  {} received {} gold", player.name, amount);
        }
        EventOutcome::Item { name } => {
            info!("  {} received item: {}", player.name, name);
            // TODO: add to player inventory
        }
        EventOutcome::RevealFog { x, y, radius } => {
            fog.reveal_radius(*x, *y, *radius);
            player.revealed_tiles = fog.to_base64();
            info!("  Fog revealed around ({},{}) radius {}", x, y, radius);
        }
        EventOutcome::Notification { text } => {
            info!("  Notification: {}", text);
            // Browser will see this via active events poll
        }
        EventOutcome::SpawnEvents { event_ids } => {
            info!("  Would spawn events: {:?}", event_ids);
            // TODO: add new events to catalog
        }
        EventOutcome::TileCostModifier { multiplier, duration_tiles } => {
            info!("  Tile cost modifier: {}x for {} tiles", multiplier, duration_tiles);
            // TODO: apply modifier
        }
    }
}
