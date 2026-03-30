use std::collections::HashMap;

use anyhow::Result;
use questlib::events::{EventCatalog, EventOutcome, EventStatus, TriggerContext};
use questlib::fog::FogBitfield;
use questlib::mapgen::WorldMap;
use questlib::route::{self, position_along_route};
use tracing::{debug, info};

use crate::devserver::{DevPlayerState, SharedState};
use crate::{SharedEvents, SharedNotifs};

pub fn run_tick_dev(
    state: &SharedState,
    world: &WorldMap,
    shared_events: &SharedEvents,
    shared_notifs: &SharedNotifs,
    player_fogs: &mut HashMap<String, FogBitfield>,
    player_last_distance: &mut HashMap<String, i32>,
    rng_roll: f32,
) -> Result<()> {
    // Snapshot player state — release lock so walker/debug can write
    let players: Vec<DevPlayerState> = {
        let lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        lock.values().cloned().collect()
    };

    let mut events_lock = shared_events.lock().map_err(|e| anyhow::anyhow!("{e}"))?;

    for player in &players {
        let player_id = &player.id;

        // Init fog
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

        // Init last distance
        if !player_last_distance.contains_key(player_id) {
            player_last_distance.insert(player_id.clone(), player.total_distance_m);
        }

        info!(
            "[{}] tile=({},{}) dist={}m gold={} walking={} route_len={} route_m={:.0}",
            player.name, player.map_tile_x, player.map_tile_y,
            player.total_distance_m, player.gold, player.is_walking,
            player.planned_route.len(), player.route_meters_walked
        );

        if !player.is_walking {
            player_last_distance.insert(player_id.clone(), player.total_distance_m);
            continue;
        }

        // Distance delta
        let last_dist = *player_last_distance.get(player_id).unwrap_or(&player.total_distance_m);
        let raw_delta = (player.total_distance_m - last_dist).max(0);
        let delta_m = raw_delta.min(50);
        player_last_distance.insert(player_id.clone(), player.total_distance_m);

        if raw_delta != delta_m {
            info!("[{}] delta capped: {}m → {}m", player.name, raw_delta, delta_m);
        }
        info!("[{}] delta_m={} (total={} last={})", player.name, delta_m, player.total_distance_m, last_dist);

        if delta_m == 0 {
            continue;
        }

        // Blocking event check
        let has_blocking = events_lock.active_events().iter().any(|e| e.requires_browser);

        // Parse route
        let route_tiles = if !player.planned_route.is_empty() {
            route::parse_route_json(&player.planned_route).unwrap_or_default()
        } else {
            Vec::new()
        };

        // Collect updates to write back
        let mut gold_delta = 0i32;
        let mut new_tile: Option<(i32, i32)> = None;
        let mut new_route_meters = player.route_meters_walked;
        let mut new_revealed: Option<String> = None;
        let mut clear_route = false;

        if has_blocking {
            gold_delta = (delta_m / 10).max(1);
            debug!("[{}] paused (blocking event), +{} gold", player.name, gold_delta);
        } else if route_tiles.is_empty() {
            gold_delta = (delta_m / 10).max(1);
            info!("[{}] no route, +{} gold", player.name, gold_delta);
        } else {
            info!("[{}] route has {} waypoints, advancing {}m", player.name, route_tiles.len(), delta_m);

            new_route_meters += delta_m as f64;
            let (tile_x, tile_y, _idx, route_complete) =
                position_along_route(&route_tiles, new_route_meters, world);

            let current_pos = (player.map_tile_x as usize, player.map_tile_y as usize);
            let new_pos = (tile_x, tile_y);

            let should_move = if new_pos == current_pos {
                false
            } else {
                let cur_idx = route_tiles.iter().position(|&w| w == current_pos);
                let new_idx = route_tiles.iter().position(|&w| w == new_pos);
                match (cur_idx, new_idx) {
                    (Some(cur), Some(ni)) => ni > cur,
                    (None, Some(_)) => true,
                    (Some(_), None) => false,
                    (None, None) => true,
                }
            };

            if should_move {
                info!("[{}] moved ({},{}) → ({},{})", player.name, player.map_tile_x, player.map_tile_y, tile_x, tile_y);
                new_tile = Some((tile_x as i32, tile_y as i32));
            }

            // Fog
            let fog = player_fogs.get_mut(player_id).unwrap();
            let tx = new_tile.map(|(x, _)| x as usize).unwrap_or(player.map_tile_x as usize);
            let ty = new_tile.map(|(_, y)| y as usize).unwrap_or(player.map_tile_y as usize);
            if fog.reveal_radius(tx, ty, 5) {
                new_revealed = Some(fog.to_base64());
            }

            if route_complete {
                info!("[{}] reached destination", player.name);
                clear_route = true;
            }

            gold_delta = (delta_m / 10).max(1);
        }

        // Write changes back (re-acquire lock briefly)
        {
            let mut lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Some(p) = lock.get_mut(player_id) {
                p.gold += gold_delta;
                p.route_meters_walked = new_route_meters;
                if let Some((tx, ty)) = new_tile {
                    p.map_tile_x = tx;
                    p.map_tile_y = ty;
                }
                if let Some(revealed) = new_revealed {
                    p.revealed_tiles = revealed;
                }
                if clear_route {
                    p.planned_route = String::new();
                    p.route_meters_walked = 0.0;
                }
            }
        }

        // Event triggers
        let tile_x = new_tile.map(|(x, _)| x as usize).unwrap_or(player.map_tile_x as usize);
        let tile_y = new_tile.map(|(_, y)| y as usize).unwrap_or(player.map_tile_y as usize);
        let poi_id = world.poi_at(tile_x, tile_y).map(|poi| poi.id);
        let nearby_pois = world.pois_near(tile_x, tile_y, 5);
        let biome = world.biome_at(tile_x, tile_y);

        let ctx = TriggerContext {
            player_tile: (tile_x, tile_y),
            player_poi: poi_id,
            nearby_poi_ids: nearby_pois,
            player_biome: biome,
            total_distance_m: player.total_distance_m as u32,
            inventory: vec![],
            completed_events: events_lock.completed_ids(),
            rng_roll,
        };

        let triggered_ids: Vec<String> = events_lock
            .check_triggers(&ctx)
            .iter()
            .map(|e| e.id.clone())
            .collect();

        for event_id in &triggered_ids {
            let event = events_lock.get_mut(event_id).unwrap();
            if event.transition(EventStatus::Active).is_ok() {
                info!("[{}] Event triggered: {} ({})", player.name, event.name, event.id);

                if event.auto_completes() {
                    if event.transition(EventStatus::Completed).is_ok() {
                        info!("  Auto-completed: {}", event.name);
                        let fog = player_fogs.get_mut(player_id).unwrap();
                        let mut lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
                        if let Some(p) = lock.get_mut(player_id) {
                            for outcome in &event.outcomes {
                                apply_outcome(outcome, p, fog);
                                if let EventOutcome::Notification { text } = outcome {
                                    if let Ok(mut notifs) = shared_notifs.lock() {
                                        notifs.push(text.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn apply_outcome(outcome: &EventOutcome, player: &mut DevPlayerState, fog: &mut FogBitfield) {
    match outcome {
        EventOutcome::Gold { amount } => {
            player.gold += amount;
            info!("  +{} gold", amount);
        }
        EventOutcome::Item { name } => {
            info!("  +item: {}", name);
        }
        EventOutcome::RevealFog { x, y, radius } => {
            fog.reveal_radius(*x, *y, *radius);
            player.revealed_tiles = fog.to_base64();
            info!("  Fog revealed ({},{}) r={}", x, y, radius);
        }
        EventOutcome::Notification { text } => {
            info!("  Notification: {}", text);
        }
        EventOutcome::SpawnEvents { event_ids } => {
            info!("  Spawn events: {:?}", event_ids);
        }
        EventOutcome::TileCostModifier { multiplier, duration_tiles } => {
            info!("  Cost modifier: {}x for {} tiles", multiplier, duration_tiles);
        }
    }
}
