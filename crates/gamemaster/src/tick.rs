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
    interiors: &crate::interior::SharedInteriors,
    player_fogs: &mut HashMap<String, FogBitfield>,
    player_last_distance: &mut HashMap<String, f64>,
    // player_id → event_id we've already sent "waiting for others" for.
    // Keeps the wait notification from spamming every tick.
    player_boss_wait_notified: &mut HashMap<String, String>,
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

        // Players inside an interior are handled by crate::interior::run_interior_tick
        // in the outer loop. Skip them here entirely so overworld logic (fog,
        // routes, events, combat) doesn't touch their state.
        if player.location.interior_id().is_some() {
            continue;
        }

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

        // When the player isn't walking, drop last_dist tracking. On the next
        // walking tick, it's re-synced to the current snapshot total — the first
        // walking tick advances nothing, preventing bursts when the bridge has
        // accumulated distance between ticks (e.g. during WS connect/buffering).
        if !player.is_walking {
            player_last_distance.remove(player_id);
            continue;
        }

        // First walking tick (after join or after a not-walking period): sync to current.
        if !player_last_distance.contains_key(player_id) {
            player_last_distance.insert(player_id.clone(), player.total_distance_m);
        }

        info!(
            "[{}] tile=({},{}) dist={}m gold={} route_m={:.0}",
            player.name, player.map_tile_x, player.map_tile_y,
            player.total_distance_m, player.gold, player.route_meters_walked
        );

        // Distance delta — f64 throughout for sub-meter precision.
        // Debug walking computes delta from speed directly.
        let raw_delta: f64 = if player.debug_walking {
            (player.current_speed_kmh as f64 / 3.6).min(20.0)
        } else {
            let last_dist = *player_last_distance.get(player_id).unwrap_or(&player.total_distance_m);
            let d = (player.total_distance_m - last_dist).max(0.0);
            let capped = d.min(20.0);
            player_last_distance.insert(player_id.clone(), player.total_distance_m);
            if (d - capped).abs() > 0.01 {
                info!("[{}] delta capped: {:.1}m → {:.1}m", player.name, d, capped);
            }
            capped
        };

        // Apply speed multipliers: passive (boots) × active buffs (potions).
        let catalog = crate::item_catalog();
        let boots_mult = questlib::items::equipment_speed_multiplier(&player.equipment, catalog);
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs()).unwrap_or(0);
        let buff_mult: f32 = player.active_buffs.iter()
            .filter(|b| b.expires_unix > now_unix && b.kind == "speed")
            .map(|b| b.multiplier)
            .product();
        let speed_mult = (boots_mult * buff_mult) as f64;
        let delta = raw_delta * speed_mult;
        if (speed_mult - 1.0).abs() > 0.01 {
            info!("[{}] speed_mult={:.2}x (boots={:.2}, buffs={:.2})", player.name, speed_mult, boots_mult, buff_mult);
        }
        info!("[{}] delta={:.2}m (speed={:.1}km/h)", player.name, delta, player.current_speed_kmh);

        if delta < 0.01 {
            continue;
        }

        // Blocking event check — only block if there's a requires_browser event
        // that THIS player hasn't personally completed. Otherwise one player's
        // dialog would freeze everyone else on the map.
        let has_blocking = events_lock.active_events().iter()
            .any(|e| e.requires_browser && !player.completed_events.contains(&e.id))
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

            // Fog — should always exist (initialized above), but guard defensively.
            let Some(fog) = player_fogs.get_mut(player_id) else { continue };
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
                let catalog = crate::item_catalog();
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
                // Prune expired buffs opportunistically (cheap and keeps the
                // Vec small without needing a separate buff-housekeeping pass).
                p.active_buffs.retain(|b| b.expires_unix > now_unix);

                // Apply chest loot
                if let Some((chest_id, loot)) = chest_loot {
                    p.opened_chests.push(chest_id);
                    p.gold += loot.gold;
                    let catalog = Some(crate::item_catalog());
                    let mut parts = vec![format!("+{} gold", loot.gold)];
                    for item_id in &loot.items {
                        questlib::items::add_item(&mut p.inventory, item_id, catalog);
                        let name = catalog.and_then(|c| c.get(item_id)).map(|d| d.display_name.as_str()).unwrap_or(item_id);
                        parts.push(name.to_string());
                    }
                    if let Ok(mut notifs) = shared_notifs.lock() {
                        crate::push_notif(&mut notifs, &player_id, format!("Opened chest! {}", parts.join(", ")));
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

                // Level up detected client-side (HUD detect_level_up system)
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
            // Equipped items count as "having" them for trigger purposes —
            // wearing the Bog Charm should satisfy has_item("bog_charm")
            // the same way carrying one in inventory does. Without this,
            // equipping a quest-gate item hides the gate from the player.
            inventory: {
                let mut ids: Vec<String> = player.inventory.iter().map(|s| s.item_id.clone()).collect();
                for slot in questlib::items::EquipmentLoadout::all_slots() {
                    if let Some(id) = player.equipment.get_slot(slot) {
                        ids.push(id.to_string());
                    }
                }
                ids
            },
            completed_events: events_lock.completed_ids(),
            rng_roll,
        };

        // Check triggers against ALL events (not just Pending), but skip
        // events this player has already completed personally.
        let triggered_ids: Vec<String> = events_lock.events.iter()
            .filter(|e| {
                // CaveEntrance events are naturally re-triggerable: you should
                // be able to walk back to a cave mouth and re-enter. Every
                // other event type obeys the "completed once, never again for
                // this player" rule.
                let is_cave_entrance = matches!(e.kind, questlib::events::kind::EventKind::CaveEntrance { .. });
                if !is_cave_entrance && player.completed_events.contains(&e.id) { return false; }
                // Skip repeatable (shops etc — handled by client)
                if e.repeatable { return false; }
                // Must be Pending or Completed-by-another-player
                if e.status != EventStatus::Pending && e.status != EventStatus::Completed {
                    return false;
                }
                e.trigger.evaluate(&ctx)
            })
            .map(|e| e.id.clone())
            .collect();

        for event_id in &triggered_ids {
            // CaveEntrance: teleport the player into the interior immediately.
            // No dialog, no combat gate. Optionally consumes an item (torch).
            // Unlike other events, CaveEntrances are naturally re-triggerable:
            // walking back onto the entrance tile should let you re-enter.
            // See the filter in triggered_ids above for the per-player dedup.
            let cave_entry: Option<(String, usize, usize, String, Option<String>)> = {
                if let Some(event) = events_lock.get(event_id) {
                    if let questlib::events::kind::EventKind::CaveEntrance { interior_id, spawn_x, spawn_y, flavor, consume_on_entry } = &event.kind {
                        Some((interior_id.clone(), *spawn_x, *spawn_y, flavor.clone(), consume_on_entry.clone()))
                    } else { None }
                } else { None }
            };
            if let Some((interior_id, spawn_x, spawn_y, flavor, consume)) = cave_entry {
                // Torch check + consumption (if required). If the player lacks
                // the item we don't enter, don't mark completed, and don't
                // flip the event status — they can try again with a torch.
                if let Some(item_id) = consume.as_deref() {
                    let has_item = {
                        let lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
                        lock.get(player_id)
                            .map(|p| p.inventory.iter().any(|s| s.item_id == item_id))
                            .unwrap_or(false)
                    };
                    if !has_item {
                        if let Ok(mut n) = shared_notifs.lock() {
                            crate::push_notif(&mut n, player_id,
                                format!("You need a {} to enter the darkness.",
                                    crate::item_catalog().get(item_id)
                                        .map(|d| d.display_name.clone())
                                        .unwrap_or_else(|| item_id.to_string())));
                        }
                        info!("[{}] cave entry blocked: missing {}", player.name, item_id);
                        continue;
                    }
                    // Consume one of the required item.
                    if let Ok(mut lock) = state.lock() {
                        if let Some(p) = lock.get_mut(player_id) {
                            questlib::items::remove_item(&mut p.inventory, item_id);
                        }
                    }
                }

                use crate::interior::enter_interior;
                let _ = enter_interior(interiors, state, player_id, &interior_id, (spawn_x, spawn_y));
                // Record personal completion for unlock checks on portals that
                // reference this event id. (First entry adds it; subsequent
                // re-entries are no-ops thanks to the `contains` guard.)
                if let Some(event) = events_lock.get_mut(event_id) {
                    let _ = event.transition(EventStatus::Completed);
                }
                if let Ok(mut lock) = state.lock() {
                    if let Some(p) = lock.get_mut(player_id) {
                        if !p.completed_events.contains(event_id) {
                            p.completed_events.push(event_id.clone());
                        }
                    }
                }
                if !flavor.is_empty() {
                    if let Ok(mut n) = shared_notifs.lock() {
                        crate::push_notif(&mut n, player_id, flavor);
                    }
                }
                info!("[{}] entered cave '{}' via event {}", player.name, interior_id, event_id);
                continue;
            }

            // Pre-check event kind so we can gate bosses BEFORE flipping state.
            // Previously we set Active → checked gate → set back to Pending if
            // waiting. That oscillated Active ↔ Pending each tick, which was
            // briefly observable by other players' /events/active polls.
            let (is_combat, is_boss) = {
                let Some(event) = events_lock.get(event_id) else { continue };
                let is_combat = matches!(event.kind, questlib::events::kind::EventKind::Boss { .. }
                    | questlib::events::kind::EventKind::RandomEncounter { .. });
                let difficulty = match &event.kind {
                    questlib::events::kind::EventKind::RandomEncounter { difficulty, .. } => *difficulty,
                    questlib::events::kind::EventKind::Boss { .. } => 8,
                    _ => 3,
                };
                (is_combat, difficulty >= 6)
            };

            // Gate: boss fights wait for everyone. Evaluated BEFORE force_status,
            // so the event stays Pending across the wait rather than oscillating.
            // Player is NOT blocked during the wait — they can click anywhere to
            // walk away. We just refuse to start the fight yet.
            let coop_player_ids: Vec<String> = if is_boss && is_combat {
                if player.planned_route.is_empty() {
                    Vec::new() // unreachable path; treated like a non-boss skip below
                } else {
                    let poi_pos = (player.map_tile_x, player.map_tile_y);
                    let online_count = players.len();
                    let here: Vec<String> = players.iter()
                        .filter(|p| p.map_tile_x == poi_pos.0 && p.map_tile_y == poi_pos.1)
                        .map(|p| p.id.clone())
                        .collect();
                    if here.len() < online_count {
                        let missing: Vec<&str> = players.iter()
                            .filter(|p| p.map_tile_x != poi_pos.0 || p.map_tile_y != poi_pos.1)
                            .map(|p| p.name.as_str())
                            .collect();
                        info!("  Boss fight waiting: {}/{} players at ({},{}) — missing: {:?}",
                            here.len(), online_count, poi_pos.0, poi_pos.1, missing);
                        // Notify the arriving player once per event-arrival so they
                        // know why the fight isn't starting. Subsequent ticks on the
                        // same tile are silent.
                        let notified = player_boss_wait_notified.get(player_id);
                        if notified != Some(event_id) {
                            let boss_name = events_lock.get(event_id)
                                .map(|e| e.name.clone()).unwrap_or_default();
                            let msg = if missing.len() == 1 {
                                format!("{} awaits. Waiting for {} to arrive...", boss_name, missing[0])
                            } else {
                                format!("{} awaits. Waiting for {} more players...", boss_name, missing.len())
                            };
                            if let Ok(mut n) = shared_notifs.lock() {
                                crate::push_notif(&mut n, player_id, msg);
                            }
                            player_boss_wait_notified.insert(player_id.clone(), event_id.clone());
                        }
                        continue; // re-check next tick; event stays Pending globally
                    }
                    here
                }
            } else { Vec::new() };

            // Leaving the waiting state (wait gate passed, or this event isn't a
            // boss waiting gate at all) clears our "already notified" record so
            // a future arrival triggers a fresh notification.
            player_boss_wait_notified.remove(player_id);

            let Some(event) = events_lock.get_mut(event_id) else { continue };
            event.force_status(EventStatus::Active);
            {
                info!("[{}] Event triggered: {} ({})", player.name, event.name, event.id);

                if is_combat {
                    if !player.planned_route.is_empty() {
                        let catalog = crate::item_catalog();
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
                        if is_boss && !coop_player_ids.is_empty() {
                            if let Ok(mut combat_lock) = shared_combat.lock() {
                                if let Some(cs) = combat_lock.get_mut(&event.id) {
                                    cs.coop_players = coop_player_ids.clone();
                                    info!("  Coop boss fight started: {} players", coop_player_ids.len());
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
                        let Some(fog) = player_fogs.get_mut(player_id) else { continue };
                        let mut lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
                        if let Some(p) = lock.get_mut(player_id) {
                            p.completed_events.push(event_id.clone());
                            for outcome in &event.outcomes {
                                apply_outcome(outcome, p, fog);
                                if let EventOutcome::Notification { text } = outcome {
                                    if let Ok(mut notifs) = shared_notifs.lock() {
                                        crate::push_notif(&mut notifs, &player_id, text.clone());
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
                    let catalog = Some(crate::item_catalog());
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
                        questlib::items::add_item(&mut p.inventory, item, catalog);
                        let item_name = catalog.and_then(|c| c.get(item)).map(|d| d.display_name.as_str()).unwrap_or(item);
                        msg.push_str(&format!(", +{}", item_name));
                    }
                    if let Ok(mut notifs) = shared_notifs.lock() {
                        crate::push_notif(&mut notifs, &pid, msg);
                    }
                }
            }
        } else if let Some((interior_id, monster_idx)) = questlib::interior::parse_monster_combat_event_id(victory_event_id) {
            // Interior monster victory — same loot rules as overworld.
            let mut lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Some(pid) = fighter_pid.clone() {
                if let Some(p) = lock.get_mut(&pid) {
                    let defeated_key = questlib::interior::monster_key(interior_id, monster_idx);
                    p.defeated_monsters.push(defeated_key);
                    let (difficulty, name) = interiors.get(interior_id)
                        .and_then(|interior| interior.monsters.get(monster_idx))
                        .map(|m| (m.difficulty, m.monster_type.display_name().to_string()))
                        .unwrap_or((1, "Monster".to_string()));
                    let gold = 30 + (difficulty as i32 * 20);
                    p.gold += gold;
                    let catalog = Some(crate::item_catalog());
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
                        questlib::items::add_item(&mut p.inventory, item, catalog);
                        let item_name = catalog.and_then(|c| c.get(item)).map(|d| d.display_name.as_str()).unwrap_or(item);
                        msg.push_str(&format!(", +{}", item_name));
                    }
                    if let Ok(mut notifs) = shared_notifs.lock() {
                        crate::push_notif(&mut notifs, &pid, msg);
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
                        p.completed_events.push(victory_event_id.clone());
                        for outcome in &event.outcomes {
                            apply_outcome(outcome, p, fog);
                        }
                    }
                }
                // Notifications to all participants
                for outcome in &event.outcomes {
                    if let EventOutcome::Notification { text } = outcome {
                        if let Ok(mut notifs) = shared_notifs.lock() {
                            for pid in &participants {
                                crate::push_notif(&mut notifs, &pid, text.clone());
                            }
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
        Arc::new(Mutex::new(HashMap::new())),
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
            let catalog = Some(crate::item_catalog());
            questlib::items::add_item(&mut player.inventory, name, catalog);
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

    /// Empty interiors registry for tick tests — tests don't need caves.
    fn test_interiors() -> crate::interior::SharedInteriors {
        std::sync::Arc::new(HashMap::new())
    }

    /// Run N ticks, incrementing total_distance by delta_per_tick each time.
    fn run_ticks(
        state: &SharedState, world: &WorldMap, events: &SharedEvents, notifs: &SharedNotifs, combat: &SharedCombat,
        fogs: &mut HashMap<String, FogBitfield>, last_dist: &mut HashMap<String, f64>,
        player_id: &str, n: usize, delta_per_tick: f64,
    ) {
        let interiors = test_interiors();
        let mut boss_wait = HashMap::new();
        for _ in 0..n {
            {
                let mut lock = state.lock().unwrap();
                if let Some(p) = lock.get_mut(player_id) {
                    p.total_distance_m += delta_per_tick;
                }
            }
            run_tick_dev(state, world, events, notifs, combat, &interiors, fogs, last_dist, &mut boss_wait, 0.5).unwrap();
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

        run_tick_dev(&state, &w, &events, &notifs, &combat, &test_interiors(), &mut fogs, &mut last_dist, &mut HashMap::new(), 0.5).unwrap();

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
        run_tick_dev(&state, &w, &events, &notifs, &combat, &test_interiors(), &mut fogs, &mut last_dist, &mut HashMap::new(), 0.5).unwrap();
        {
            let mut lock = state.lock().unwrap();
            lock.get_mut(&pid).unwrap().total_distance_m = 110.0;
        }
        run_tick_dev(&state, &w, &events, &notifs, &combat, &test_interiors(), &mut fogs, &mut last_dist, &mut HashMap::new(), 0.5).unwrap();

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
            run_tick_dev(&state, &w, &events, &notifs, &combat, &test_interiors(), &mut fogs, &mut last_dist, &mut HashMap::new(), 0.5).unwrap();

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
        run_tick_dev(&state, &w, &events, &notifs, &combat, &test_interiors(), &mut fogs, &mut last_dist, &mut HashMap::new(), 0.5).unwrap();
        run_tick_dev(&state, &w, &events, &notifs, &combat, &test_interiors(), &mut fogs, &mut last_dist, &mut HashMap::new(), 0.5).unwrap();

        let p = get_player(&state, &pid);
        assert_eq!(p.route_meters_walked, 0.0);
        assert_eq!((p.map_tile_x, p.map_tile_y), (start.0 as i32, start.1 as i32));
    }
}
