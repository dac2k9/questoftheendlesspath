use std::collections::HashMap;

use anyhow::Result;
use questlib::events::{EventCatalog, EventOutcome, EventStatus, TriggerContext};
use questlib::fog::FogBitfield;
use questlib::mapgen::WorldMap;
use questlib::route::{self, facing_along_route, position_along_route};
use tracing::{debug, info};

use crate::combat::{self as server_combat, SharedCombat};
use crate::devserver::{DevPlayerState, SharedState};
use crate::{SharedEvents, SharedNotifs};

pub fn run_tick_dev(
    state: &SharedState,
    world: &WorldMap,
    shared_events: &SharedEvents,
    shared_notifs: &SharedNotifs,
    shared_combat: &SharedCombat,
    player_fogs: &mut HashMap<String, FogBitfield>,
    player_last_distance: &mut HashMap<String, f64>,
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
                f.reveal_radius(player.map_tile_x as usize, player.map_tile_y as usize, 8);
                // Write initial fog to player state so client can see it
                let mut lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
                if let Some(p) = lock.get_mut(player_id) {
                    p.revealed_tiles = f.to_base64();
                }
                f
            };
            player_fogs.insert(player_id.clone(), fog);
        }

        // Init last distance
        if !player_last_distance.contains_key(player_id) {
            player_last_distance.insert(player_id.clone(), player.total_distance_m);
        }

        if !player.is_walking {
            player_last_distance.insert(player_id.clone(), player.total_distance_m);
            continue;
        }

        info!(
            "[{}] tile=({},{}) dist={}m gold={} route_m={:.0}",
            player.name, player.map_tile_x, player.map_tile_y,
            player.total_distance_m, player.gold, player.route_meters_walked
        );

        // Distance delta — f64 throughout for sub-meter precision.
        // Debug walking computes delta from speed directly.
        let delta: f64 = if player.debug_walking {
            (player.current_speed_kmh as f64 / 3.6).min(20.0)
        } else {
            let last_dist = *player_last_distance.get(player_id).unwrap_or(&player.total_distance_m);
            let raw_delta = (player.total_distance_m - last_dist).max(0.0);
            let capped = raw_delta.min(20.0);
            player_last_distance.insert(player_id.clone(), player.total_distance_m);
            if (raw_delta - capped).abs() > 0.01 {
                info!("[{}] delta capped: {:.1}m → {:.1}m", player.name, raw_delta, capped);
            }
            capped
        };
        info!("[{}] delta={:.2}m (speed={:.1}km/h)", player.name, delta, player.current_speed_kmh);

        if delta < 0.01 {
            continue;
        }

        // Blocking event check — also block during THIS player's combat
        let has_blocking = events_lock.active_events().iter().any(|e| e.requires_browser)
            || server_combat::player_in_combat(shared_combat, player_id);

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
        let mut new_facing: Option<route::Facing> = None;

        if has_blocking {
            gold_delta = 0;
            info!("[{}] BLOCKED (active event/combat), +{} gold", player.name, gold_delta);
        } else if route_tiles.is_empty() {
            gold_delta = 0;
            info!("[{}] no route, +{} gold", player.name, gold_delta);
        } else {
            info!("[{}] route has {} waypoints, advancing {:.2}m", player.name, route_tiles.len(), delta);

            new_route_meters += delta;
            let (tile_x, tile_y, idx, route_complete) =
                position_along_route(&route_tiles, new_route_meters, world);

            // Compute facing direction toward next tile on route
            new_facing = Some(facing_along_route(&route_tiles, idx));

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

            // Block movement into biomes that require items — but roads are always safe
            let target_biome = world.biome_at(tile_x, tile_y);
            let has_required_item = if world.has_road_at(tile_x, tile_y) {
                true
            } else {
                target_biome.required_item().map_or(true, |req| {
                    questlib::items::has_item_or_equipped(&player.inventory, &player.equipment, req)
                })
            };

            if should_move && has_required_item {
                info!("[{}] moved ({},{}) → ({},{})", player.name, player.map_tile_x, player.map_tile_y, tile_x, tile_y);
                new_tile = Some((tile_x as i32, tile_y as i32));
            } else if should_move && !has_required_item {
                info!("[{}] blocked at ({},{}) — needs {:?}", player.name, tile_x, tile_y, target_biome.required_item());
                clear_route = true;
            }

            // Fog
            let fog = player_fogs.get_mut(player_id).unwrap();
            // Merge any fog reveals from event completions (applied to player state directly)
            if !player.revealed_tiles.is_empty() {
                if let Some(state_fog) = FogBitfield::from_base64(&player.revealed_tiles) {
                    fog.merge(&state_fog);
                }
            }
            let tx = new_tile.map(|(x, _)| x as usize).unwrap_or(player.map_tile_x as usize);
            let ty = new_tile.map(|(_, y)| y as usize).unwrap_or(player.map_tile_y as usize);
            if fog.reveal_radius(tx, ty, 5) {
                new_revealed = Some(fog.to_base64());
            }

            if route_complete {
                info!("[{}] reached destination", player.name);
                clear_route = true;
            }

            gold_delta = 0;
        }

        // Check for loot chest at player's tile
        let player_tx = new_tile.map(|(x, _)| x as usize).unwrap_or(player.map_tile_x as usize);
        let player_ty = new_tile.map(|(_, y)| y as usize).unwrap_or(player.map_tile_y as usize);
        let mut chest_loot: Option<(String, questlib::mapgen::ChestLoot)> = None;
        if let Some(idx) = world.chest_at(player_tx, player_ty) {
            let chest_id = format!("chest_{}", idx);
            if !player.opened_chests.contains(&chest_id) {
                let loot = world.chest_loot(idx);
                info!("[{}] opened {} — +{} gold, items: {:?}", player.name, chest_id, loot.gold, loot.items);
                chest_loot = Some((chest_id, loot));
            }
        }

        // Check for world monster at player's tile
        if let Some(idx) = world.monster_at(player_tx, player_ty) {
            let monster_id = format!("monster_{}", idx);
            if !player.defeated_monsters.contains(&monster_id) {
                let m = &world.monsters[idx];
                // Start combat with this monster
                let combat_event_id = monster_id.clone();
                let kind = questlib::events::kind::EventKind::RandomEncounter {
                    enemy_name: m.monster_type.display_name().to_string(),
                    description: format!("A wild {} blocks your path!", m.monster_type.display_name()),
                    difficulty: m.difficulty,
                };
                let catalog = questlib::items::ItemCatalog::from_json(
                    include_str!("../../../adventures/items.json")
                ).unwrap_or_default();
                let eq_bonus = questlib::items::equipment_bonuses(&player.equipment, &catalog);
                // Only start if THIS player isn't already in combat
                if !server_combat::player_in_combat(shared_combat, player_id) {
                    server_combat::start_combat(shared_combat, &combat_event_id, &kind, player.total_distance_m as u64, eq_bonus, player_id);
                    info!("[{}] Monster encounter: {} (difficulty {})", player.name, m.monster_type.display_name(), m.difficulty);
                }
            }
        }

        // Write changes back (re-acquire lock briefly)
        {
            let mut lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Some(p) = lock.get_mut(player_id) {
                // Apply chest loot
                if let Some((chest_id, loot)) = chest_loot {
                    p.opened_chests.push(chest_id);
                    p.gold += loot.gold;
                    let catalog = questlib::items::ItemCatalog::from_json(
                        include_str!("../../../adventures/items.json")
                    ).ok();
                    let mut parts = vec![format!("+{} gold", loot.gold)];
                    for item_id in &loot.items {
                        questlib::items::add_item(&mut p.inventory, item_id, catalog.as_ref());
                        let name = catalog.as_ref().and_then(|c| c.get(item_id)).map(|d| d.display_name.as_str()).unwrap_or(item_id);
                        parts.push(name.to_string());
                    }
                    if let Ok(mut notifs) = shared_notifs.lock() {
                        notifs.push(format!("Opened chest! {}", parts.join(", ")));
                    }
                }
                // Debug walking: tick manages total_distance since handler only sets speed
                if p.debug_walking {
                    p.total_distance_m += delta;
                }
                // Gold: 1 gold per 10 meters walked (based on distance milestones)
                let old_gold_milestone = ((p.total_distance_m - delta) / 10.0) as i32;
                let new_gold_milestone = (p.total_distance_m / 10.0) as i32;
                gold_delta = (new_gold_milestone - old_gold_milestone).max(0);
                p.gold += gold_delta;
                p.route_meters_walked = new_route_meters;
                if let Some((tx, ty)) = new_tile {
                    p.prev_tile = Some((p.map_tile_x, p.map_tile_y));
                    p.map_tile_x = tx;
                    p.map_tile_y = ty;
                }
                if let Some(revealed) = new_revealed {
                    p.revealed_tiles = revealed;
                }
                if let Some(facing) = new_facing {
                    p.facing = facing;
                }
                if clear_route {
                    p.planned_route = String::new();
                    p.route_meters_walked = 0.0;
                }

                // Compute interpolation envelope for smooth client animation.
                // Target = current meters + projected advance for next tick, clamped to route end.
                if !p.planned_route.is_empty() && p.is_walking && p.current_speed_kmh > 0.1 {
                    let speed_mps = p.current_speed_kmh as f64 / 3.6;
                    // Project one tick interval (1s), capped at same 20m delta limit
                    let projected = (speed_mps * 1.0).min(20.0);
                    // Compute total route cost to clamp
                    let route_cost: f64 = route_tiles.iter().map(|&(rx, ry)| {
                        let cost = route::tile_cost(world.biome_at(rx, ry), world.has_road_at(rx, ry));
                        if cost == u32::MAX { 0.0 } else { cost as f64 }
                    }).sum();
                    p.interp_meters_target = (p.route_meters_walked + projected).min(route_cost);
                    p.interp_duration_secs = 1.0;
                } else {
                    // Not walking or no route — no interpolation
                    p.interp_meters_target = p.route_meters_walked;
                    p.interp_duration_secs = 0.0;
                }

                // Check for level up
                let old_level = questlib::leveling::level_from_meters((p.total_distance_m - delta).max(0.0) as u64);
                let new_level = questlib::leveling::level_from_meters(p.total_distance_m as u64);
                if new_level > old_level {
                    info!("[{}] leveled up! {} → {}", p.name, old_level, new_level);
                    if let Ok(mut notifs) = shared_notifs.lock() {
                        notifs.push(format!("Level up! You are now level {}!", new_level));
                    }
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
            inventory: player.inventory.iter().map(|s| s.item_id.clone()).collect(),
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
            // Repeatable events (shops, wells, etc.) are permanent POI features.
            // They're not triggered as blocking events — the client handles them.
            if event.repeatable {
                continue;
            }
            if event.transition(EventStatus::Active).is_ok() {
                info!("[{}] Event triggered: {} ({})", player.name, event.name, event.id);

                // Start combat for Boss/RandomEncounter events
                let is_combat = matches!(event.kind, questlib::events::kind::EventKind::Boss { .. }
                    | questlib::events::kind::EventKind::RandomEncounter { .. });
                let difficulty = match &event.kind {
                    questlib::events::kind::EventKind::RandomEncounter { difficulty, .. } => *difficulty,
                    questlib::events::kind::EventKind::Boss { .. } => 8,
                    _ => 3,
                };
                let is_boss = difficulty >= 6;

                if is_combat {
                    if !player.planned_route.is_empty() {
                        // For bosses: wait until ALL online players are at the same POI
                        let poi_pos = (player.map_tile_x, player.map_tile_y);
                        let online_players: Vec<&DevPlayerState> = players.iter().collect();
                        let all_here: Vec<String> = online_players.iter()
                            .filter(|p| p.map_tile_x == poi_pos.0 && p.map_tile_y == poi_pos.1)
                            .map(|p| p.id.clone())
                            .collect();

                        if is_boss && all_here.len() < online_players.len() {
                            // Not everyone here yet — keep event Active, waiting
                            info!("  Boss fight waiting: {}/{} players at ({},{})",
                                all_here.len(), online_players.len(), poi_pos.0, poi_pos.1);
                            event.force_status(EventStatus::Pending);
                            continue;
                        }

                        let catalog = questlib::items::ItemCatalog::from_json(
                            include_str!("../../../adventures/items.json")
                        ).unwrap_or_default();
                        let eq_bonus = questlib::items::equipment_bonuses(&player.equipment, &catalog);
                        server_combat::start_combat(
                            shared_combat,
                            &event.id,
                            &event.kind,
                            player.total_distance_m as u64,
                            eq_bonus,
                            player_id,
                        );
                        // For coop bosses, add all present players to the combat
                        if is_boss {
                            if let Ok(mut combat_lock) = shared_combat.lock() {
                                if let Some(cs) = combat_lock.get_mut(&event.id) {
                                    cs.coop_players = all_here.clone();
                                    info!("  Coop boss fight started: {} players", all_here.len());
                                }
                            }
                        }
                        info!("  Combat started: {}", event.name);
                    } else {
                        event.force_status(EventStatus::Dismissed);
                        info!("  Combat dismissed (no route): {}", event.name);
                    }
                }

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

    // Tick active combats using each player's own walking speed
    let player_speeds: Vec<(String, f32, f32)> = players.iter()
        .map(|p| (p.id.clone(), if p.is_walking { p.current_speed_kmh } else { 0.0 }, p.current_incline))
        .collect();
    let (victories, retreats) = server_combat::tick_all(shared_combat, &player_speeds, 1.0);

    for victory_event_id in &victories {
        info!("Combat victory: {}", victory_event_id);

        // Get the player_id(s) from the combat state before removing
        let (fighter_pid, coop_pids) = {
            let lock = shared_combat.lock().unwrap();
            let c = lock.get(victory_event_id);
            (
                c.map(|c| c.player_id.clone()),
                c.map(|c| c.coop_players.clone()).unwrap_or_default(),
            )
        };

        if victory_event_id.starts_with("monster_") {
            // World monster victory — mark defeated, give loot
            let mut lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Some(pid) = fighter_pid.clone() {
                if let Some(p) = lock.get_mut(&pid) {
                    p.defeated_monsters.push(victory_event_id.clone());
                    // Loot based on monster difficulty
                    let idx: usize = victory_event_id.strip_prefix("monster_").and_then(|s| s.parse().ok()).unwrap_or(0);
                    let difficulty = world.monsters.get(idx).map(|m| m.difficulty).unwrap_or(1);
                    let name = world.monsters.get(idx).map(|m| m.monster_type.display_name()).unwrap_or("Monster");
                    let gold = 30 + (difficulty as i32 * 20);
                    p.gold += gold;
                    // Item drop based on difficulty
                    let catalog = questlib::items::ItemCatalog::from_json(
                        include_str!("../../../adventures/items.json")
                    ).ok();
                    let drop = match difficulty {
                        1 => Some("health_potion"),
                        2 => Some("health_potion"),
                        3 => Some("iron_sword"),
                        4 => Some("chainmail"),
                        5.. => Some("greater_health_potion"),
                        _ => None,
                    };
                    let mut msg = format!("{} defeated! +{} gold", name, gold);
                    if let Some(item) = drop {
                        questlib::items::add_item(&mut p.inventory, item, catalog.as_ref());
                        let item_name = catalog.as_ref().and_then(|c| c.get(item)).map(|d| d.display_name.as_str()).unwrap_or(item);
                        msg.push_str(&format!(", +{}", item_name));
                    }
                    if let Ok(mut notifs) = shared_notifs.lock() {
                        notifs.push(msg);
                    }
                }
            }
        } else if let Some(event) = events_lock.get_mut(victory_event_id) {
            // Quest event victory — apply outcomes to ALL coop participants
            if event.transition(EventStatus::Completed).is_ok() {
                let mut lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
                let participants = if !coop_pids.is_empty() { coop_pids.clone() } else { fighter_pid.clone().into_iter().collect() };
                for pid in &participants {
                    if let (Some(p), Some(fog)) = (lock.get_mut(pid), player_fogs.get_mut(pid)) {
                        for outcome in &event.outcomes {
                            apply_outcome(outcome, p, fog);
                        }
                    }
                }
                // Notifications only once (shared)
                for outcome in &event.outcomes {
                    if let EventOutcome::Notification { text } = outcome {
                        if let Ok(mut notifs) = shared_notifs.lock() {
                            notifs.push(text.clone());
                        }
                    }
                }
            }
        }
        server_combat::remove_combat(shared_combat, victory_event_id);
    }

    // Defeat/Fled: push player back one tile (away from the monster/enemy)
    for retreat_event_id in &retreats {
        info!("Combat retreat: {}", retreat_event_id);

        // Get player_id before removing the combat
        let fighter_pid = {
            let combat_lock = shared_combat.lock().unwrap();
            combat_lock.get(retreat_event_id).map(|c| c.player_id.clone())
        };

        if let Some(event) = events_lock.get_mut(retreat_event_id) {
            event.force_status(EventStatus::Dismissed);
        }

        // Push player back to where they came from
        let mut lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Some(pid) = fighter_pid {
            if let Some(p) = lock.get_mut(&pid) {
                if let Some((prev_x, prev_y)) = p.prev_tile {
                    info!("[{}] retreated back to ({},{}) (previous tile)", p.name, prev_x, prev_y);
                    p.map_tile_x = prev_x;
                    p.map_tile_y = prev_y;
                }
                p.planned_route.clear();
                p.route_meters_walked = 0.0;
            }
        }

        server_combat::remove_combat(shared_combat, retreat_event_id);
    }

    Ok(())
}

/// Create a test player with the given parameters.
#[cfg(test)]
fn test_player(name: &str, tile_x: i32, tile_y: i32, route: &str, distance: f64, walking: bool) -> DevPlayerState {
    DevPlayerState {
        id: format!("test-{name}"),
        name: name.to_string(),
        map_tile_x: tile_x,
        map_tile_y: tile_y,
        planned_route: route.to_string(),
        total_distance_m: distance,
        is_walking: walking,
        route_meters_walked: 0.0,
        ..Default::default()
    }
}

#[cfg(test)]
fn make_test_state(players: Vec<DevPlayerState>) -> (SharedState, SharedEvents, SharedNotifs, SharedCombat) {
    use std::sync::{Arc, Mutex};
    let mut map = HashMap::new();
    for p in players {
        map.insert(p.id.clone(), p);
    }
    (
        Arc::new(Mutex::new(map)),
        Arc::new(Mutex::new(EventCatalog::default())),
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(HashMap::new())),
    )
}

fn apply_outcome(outcome: &EventOutcome, player: &mut DevPlayerState, fog: &mut FogBitfield) {
    match outcome {
        EventOutcome::Gold { amount } => {
            player.gold += amount;
            info!("  +{} gold", amount);
        }
        EventOutcome::Item { name } => {
            let catalog = questlib::items::ItemCatalog::from_json(
                include_str!("../../../adventures/items.json")
            ).ok();
            questlib::items::add_item(&mut player.inventory, name, catalog.as_ref());
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

#[cfg(test)]
mod tests {
    use super::*;
    use questlib::route::encode_route_json;

    fn world() -> WorldMap {
        WorldMap::generate(42)
    }

    /// Find a road route of `len` tiles.
    fn road_route(world: &WorldMap, len: usize) -> Vec<(usize, usize)> {
        for road in &world.roads {
            if road.path.len() >= len {
                return road.path[..len].to_vec();
            }
        }
        panic!("no road long enough");
    }

    /// Get the player state after ticking.
    fn get_player(state: &SharedState, id: &str) -> DevPlayerState {
        state.lock().unwrap().get(id).unwrap().clone()
    }

    /// Run N ticks, incrementing total_distance by delta_per_tick each time.
    fn run_ticks(
        state: &SharedState, world: &WorldMap, events: &SharedEvents, notifs: &SharedNotifs, combat: &SharedCombat,
        fogs: &mut HashMap<String, FogBitfield>, last_dist: &mut HashMap<String, f64>,
        player_id: &str, n: usize, delta_per_tick: f64,
    ) {
        for _ in 0..n {
            {
                let mut lock = state.lock().unwrap();
                if let Some(p) = lock.get_mut(player_id) {
                    p.total_distance_m += delta_per_tick;
                }
            }
            run_tick_dev(state, world, events, notifs, combat, fogs, last_dist, 0.5).unwrap();
        }
    }

    // -- Basic tick tests --

    #[test]
    fn not_walking_does_nothing() {
        let w = world();
        let route = road_route(&w, 5);
        let player = test_player("idle", route[0].0 as i32, route[0].1 as i32,
            &encode_route_json(&route), 100.0, false);
        let pid = player.id.clone();
        let (state, events, notifs, combat) = make_test_state(vec![player]);
        let mut fogs = HashMap::new();
        let mut last_dist = HashMap::new();

        run_tick_dev(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, 0.5).unwrap();

        let p = get_player(&state, &pid);
        assert_eq!(p.gold, 0, "idle player should earn no gold");
        assert_eq!(p.route_meters_walked, 0.0, "idle player should not advance");
    }

    #[test]
    fn walking_no_route_earns_gold() {
        let w = world();
        let player = test_player("noroute", 50, 40, "", 100.0, true);
        let pid = player.id.clone();
        let (state, events, notifs, combat) = make_test_state(vec![player]);
        let mut fogs = HashMap::new();
        let mut last_dist = HashMap::new();

        // First tick inits last_distance, so add distance then tick again
        run_tick_dev(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, 0.5).unwrap();
        {
            let mut lock = state.lock().unwrap();
            lock.get_mut(&pid).unwrap().total_distance_m = 110.0;
        }
        run_tick_dev(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, 0.5).unwrap();

        let p = get_player(&state, &pid);
        assert!(p.gold > 0, "walking without route should still earn gold");
        assert_eq!(p.route_meters_walked, 0.0, "no route means no route meters");
    }

    #[test]
    fn walking_advances_along_route() {
        let w = world();
        let route = road_route(&w, 10);
        let start = route[0];
        let player = test_player("walker", start.0 as i32, start.1 as i32,
            &encode_route_json(&route), 0.0, true);
        let pid = player.id.clone();
        let (state, events, notifs, combat) = make_test_state(vec![player]);
        let mut fogs = HashMap::new();
        let mut last_dist = HashMap::new();

        // Walk 10m per tick for 20 ticks (200m total — enough for several road tiles at 20m each)
        run_ticks(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, &pid, 20, 10.0);

        let p = get_player(&state, &pid);
        // Should have moved from start tile (route may have completed and cleared meters)
        let moved = p.map_tile_x != start.0 as i32 || p.map_tile_y != start.1 as i32;
        assert!(moved, "player should have moved tiles: still at ({},{})", p.map_tile_x, p.map_tile_y);
    }

    #[test]
    fn delta_is_capped() {
        let w = world();
        let route = road_route(&w, 10);
        let start = route[0];
        let player = test_player("speedy", start.0 as i32, start.1 as i32,
            &encode_route_json(&route), 0.0, true);
        let pid = player.id.clone();
        let (state, events, notifs, combat) = make_test_state(vec![player]);
        let mut fogs = HashMap::new();
        let mut last_dist = HashMap::new();

        // Huge jump in distance (cheating or glitch)
        run_ticks(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, &pid, 1, 0.0);
        run_ticks(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, &pid, 1, 500.0);

        let p = get_player(&state, &pid);
        // Delta capped at 20, so route_meters should be <= 20
        assert!(p.route_meters_walked <= 20.0,
            "route_meters should be capped: got {}", p.route_meters_walked);
    }

    #[test]
    fn route_completes_and_clears() {
        let w = world();
        let route = road_route(&w, 3); // short route: 3 road tiles = 60m
        let start = route[0];
        let player = test_player("finisher", start.0 as i32, start.1 as i32,
            &encode_route_json(&route), 0.0, true);
        let pid = player.id.clone();
        let (state, events, notifs, combat) = make_test_state(vec![player]);
        let mut fogs = HashMap::new();
        let mut last_dist = HashMap::new();

        // Walk 10m/tick for 20 ticks (200m — plenty for 60m route)
        run_ticks(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, &pid, 20, 10.0);

        let p = get_player(&state, &pid);
        assert!(p.planned_route.is_empty(), "route should be cleared after completion");
        assert_eq!(p.route_meters_walked, 0.0, "meters should reset after completion");
        // Player should be at or near the last route tile
        let end = route.last().unwrap();
        assert_eq!((p.map_tile_x, p.map_tile_y), (end.0 as i32, end.1 as i32),
            "player should be at route end");
    }

    #[test]
    fn player_never_moves_backward() {
        let w = world();
        let route = road_route(&w, 15);
        let start = route[0];
        let player = test_player("forward", start.0 as i32, start.1 as i32,
            &encode_route_json(&route), 0.0, true);
        let pid = player.id.clone();
        let (state, events, notifs, combat) = make_test_state(vec![player]);
        let mut fogs = HashMap::new();
        let mut last_dist = HashMap::new();

        let mut max_route_idx = 0usize;
        for tick in 0..30 {
            {
                let mut lock = state.lock().unwrap();
                lock.get_mut(&pid).unwrap().total_distance_m += 10.0;
            }
            run_tick_dev(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, 0.5).unwrap();

            let p = get_player(&state, &pid);
            let pos = (p.map_tile_x as usize, p.map_tile_y as usize);
            if let Some(idx) = route.iter().position(|&t| t == pos) {
                assert!(idx >= max_route_idx,
                    "tick {tick}: player went backward on route: {max_route_idx} -> {idx}");
                max_route_idx = idx;
            }

            if p.planned_route.is_empty() { break; }
        }
        assert!(max_route_idx > 0, "player should have advanced at least one tile");
    }

    #[test]
    fn gold_earned_every_tick_while_walking() {
        let w = world();
        let route = road_route(&w, 10);
        let start = route[0];
        let player = test_player("miner", start.0 as i32, start.1 as i32,
            &encode_route_json(&route), 0.0, true);
        let pid = player.id.clone();
        let (state, events, notifs, combat) = make_test_state(vec![player]);
        let mut fogs = HashMap::new();
        let mut last_dist = HashMap::new();

        // Init tick
        run_ticks(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, &pid, 1, 0.0);

        let mut last_gold = 0;
        for _ in 0..5 {
            run_ticks(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, &pid, 1, 15.0);
            let p = get_player(&state, &pid);
            assert!(p.gold > last_gold, "gold should increase each tick: {} -> {}", last_gold, p.gold);
            last_gold = p.gold;
        }
    }

    #[test]
    fn fog_reveals_around_player() {
        let w = world();
        let route = road_route(&w, 5);
        let start = route[0];
        let player = test_player("explorer", start.0 as i32, start.1 as i32,
            &encode_route_json(&route), 0.0, true);
        let pid = player.id.clone();
        let (state, events, notifs, combat) = make_test_state(vec![player]);
        let mut fogs = HashMap::new();
        let mut last_dist = HashMap::new();

        run_ticks(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, &pid, 1, 0.0);

        let fog = fogs.get(&pid).unwrap();
        assert!(fog.is_revealed(start.0, start.1), "start tile should be revealed");
        // Check radius
        if start.0 >= 3 && start.1 >= 3 {
            assert!(fog.is_revealed(start.0 - 3, start.1), "nearby tile should be revealed");
        }
    }

    #[test]
    fn zero_distance_delta_no_progress() {
        let w = world();
        let route = road_route(&w, 5);
        let start = route[0];
        let player = test_player("still", start.0 as i32, start.1 as i32,
            &encode_route_json(&route), 100.0, true);
        let pid = player.id.clone();
        let (state, events, notifs, combat) = make_test_state(vec![player]);
        let mut fogs = HashMap::new();
        let mut last_dist = HashMap::new();

        // Two ticks with same distance — no movement
        run_tick_dev(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, 0.5).unwrap();
        run_tick_dev(&state, &w, &events, &notifs, &combat, &mut fogs, &mut last_dist, 0.5).unwrap();

        let p = get_player(&state, &pid);
        assert_eq!(p.route_meters_walked, 0.0);
        assert_eq!((p.map_tile_x, p.map_tile_y), (start.0 as i32, start.1 as i32));
    }
}
