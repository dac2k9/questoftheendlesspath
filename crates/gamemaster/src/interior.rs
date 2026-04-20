//! Runtime for interior spaces (caves / dungeons / castles).
//!
//! Design goals for Phase 1:
//! - Keep all interior-specific runtime in this module. The overworld tick
//!   loop calls into here and otherwise stays unchanged.
//! - Data model lives in questlib::interior; this module owns loading,
//!   per-tick movement inside an interior, and the enter/exit endpoints.
//! - No combat, monsters, or events yet — just movement, fog, chests, and
//!   portal transitions.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use questlib::fog::FogBitfield;
use questlib::interior::{chest_key, InteriorMap, Location, PortalDest};
use questlib::route;
use tracing::info;

use crate::devserver::{DevPlayerState, SharedState};
use crate::SharedNotifs;

/// Shared collection of interior maps, indexed by id.
pub type SharedInteriors = Arc<HashMap<String, InteriorMap>>;

/// Load all `adventures/interiors/*.json` files. Missing directory or empty
/// directory is fine — the game just won't have any interiors wired up.
pub fn load_interiors(dir: &str) -> Result<HashMap<String, InteriorMap>> {
    let mut out = HashMap::new();
    let path = std::path::Path::new(dir);
    if !path.exists() {
        info!("No interiors directory at {} — caves disabled", dir);
        return Ok(out);
    }
    for entry in std::fs::read_dir(path).context("read interiors dir")? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => { tracing::warn!("skipping bad entry: {e}"); continue; }
        };
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") { continue; }
        let json = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => { tracing::warn!("skipping {}: {e}", p.display()); continue; }
        };
        let interior: InteriorMap = match serde_json::from_str(&json) {
            Ok(m) => m,
            Err(e) => { tracing::warn!("parse {}: {e}", p.display()); continue; }
        };
        if let Err(e) = interior.validate() {
            tracing::warn!("validate {}: {e}", interior.id);
            continue;
        }
        info!("Loaded interior '{}' ({}x{}, {} portals, {} chests)",
            interior.id, interior.width, interior.height,
            interior.portals.len(), interior.chests.len());
        out.insert(interior.id.clone(), interior);
    }
    Ok(out)
}

// ── Per-tick movement inside an interior ───────────

/// Tick a single player who is inside an interior. Mirrors a tiny subset of
/// the overworld tick: walker-derived delta → route advancement → fog reveal
/// → chest open. No events, monsters, or combat in Phase 1.
///
/// Returns Ok(()) even if nothing happens. Silently skips the tick for
/// players with a stale or missing location.
pub fn run_interior_tick(
    interiors: &SharedInteriors,
    state: &SharedState,
    shared_notifs: &SharedNotifs,
    player_last_distance: &mut HashMap<String, f64>,
    interior_fogs: &mut HashMap<(String, String), FogBitfield>,
    player_id: &str,
) -> Result<()> {
    // Snapshot player. If their location isn't an interior we know about, bail.
    let player = {
        let lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        match lock.get(player_id) { Some(p) => p.clone(), None => return Ok(()) }
    };

    let interior_id = match player.location.interior_id() {
        Some(id) => id.to_string(),
        None => return Ok(()),
    };
    let Some(interior) = interiors.get(&interior_id) else {
        tracing::warn!("[{}] in unknown interior '{}' — keeping idle", player.name, interior_id);
        return Ok(());
    };

    // Not walking → just keep last_distance fresh and return. Same semantics
    // as the overworld tick so a pause on the treadmill doesn't create a
    // burst once the player starts walking again.
    if !player.is_walking {
        player_last_distance.remove(player_id);
        return Ok(());
    }

    // Compute delta from total_distance_m, same as overworld.
    if !player_last_distance.contains_key(player_id) {
        player_last_distance.insert(player_id.to_string(), player.total_distance_m);
    }
    let last_dist = *player_last_distance.get(player_id).unwrap_or(&player.total_distance_m);
    let raw_delta = (player.total_distance_m - last_dist).max(0.0).min(20.0);
    player_last_distance.insert(player_id.to_string(), player.total_distance_m);

    // Apply speed multipliers (boots + potions) — interiors are just as much
    // movement as overworld, so buffs should carry.
    let catalog = crate::item_catalog();
    let boots_mult = questlib::items::equipment_speed_multiplier(&player.equipment, catalog);
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let buff_mult: f32 = player.active_buffs.iter()
        .filter(|b| b.expires_unix > now_unix && b.kind == "speed")
        .map(|b| b.multiplier).product();
    let delta = raw_delta * (boots_mult * buff_mult) as f64;

    if delta < 0.01 { return Ok(()); }

    // Parse route (same JSON shape; tiles are interior coords).
    let route_tiles = if !player.planned_route.is_empty() {
        route::parse_route_json(&player.planned_route).unwrap_or_default()
    } else { Vec::new() };
    if route_tiles.is_empty() { return Ok(()); }

    // Advance by `delta` meters along the route, stopping at walls.
    let mut route_m = player.route_meters_walked + delta;
    let cost_per_tile = interior.floor_cost_m as f64;
    let mut cur_idx = route_tiles.iter()
        .position(|&t| t.0 == player.map_tile_x as usize && t.1 == player.map_tile_y as usize)
        .unwrap_or(0);
    // Pure steps: chew through tiles while route_m > cost_per_tile
    while route_m >= cost_per_tile && cur_idx + 1 < route_tiles.len() {
        let next = route_tiles[cur_idx + 1];
        if !interior.is_walkable(next.0, next.1) { break; }
        cur_idx += 1;
        route_m -= cost_per_tile;
    }
    let new_tile = route_tiles.get(cur_idx).copied();

    // Fog: reveal around the new tile.
    let fog_key = (player_id.to_string(), interior_id.clone());
    let fog = interior_fogs.entry(fog_key.clone()).or_insert_with(|| {
        // Restore from saved state if present.
        player.interior_fog.get(&interior_id)
            .and_then(|b64| FogBitfield::from_base64_sized(b64, interior.width, interior.height))
            .unwrap_or_else(|| FogBitfield::new_sized(interior.width, interior.height))
    });
    let (tx, ty) = new_tile.unwrap_or((player.map_tile_x as usize, player.map_tile_y as usize));
    let fog_changed = fog.reveal_radius(tx, ty, 4);

    // Chest: opening at the current tile.
    let opened_chest = interior.chest_at(tx, ty)
        .filter(|idx| !player.opened_chests.contains(&chest_key(&interior_id, *idx)));

    // Portal: if the player stepped onto a portal tile this tick, auto-use it.
    // Writeback happens FIRST (so new position is recorded), then use_portal
    // transitions the location. Skip if the player didn't actually move this
    // tick — otherwise they'd re-trigger the portal every idle tick.
    let stepped_onto_portal =
        new_tile.map_or(false, |(x, y)| x as i32 != player.map_tile_x || y as i32 != player.map_tile_y)
        && interior.portal_at(tx, ty).is_some();

    // Writeback.
    {
        let mut lock = state.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Some(p) = lock.get_mut(player_id) {
            // Position + route progress
            if let Some((x, y)) = new_tile {
                p.map_tile_x = x as i32;
                p.map_tile_y = y as i32;
            }
            p.route_meters_walked = route_m;
            // Fog persistence
            if fog_changed {
                p.interior_fog.insert(interior_id.clone(), fog.to_base64());
            }
            // Chest reward (flat gold for Phase 1; Phase 2 adds loot tables).
            if let Some(chest_idx) = opened_chest {
                let key = chest_key(&interior_id, chest_idx);
                p.opened_chests.push(key);
                let gold_reward = 50;
                p.gold += gold_reward;
                if let Ok(mut n) = shared_notifs.lock() {
                    crate::push_notif(&mut n, player_id, format!("Opened a hidden chest! +{} gold", gold_reward));
                }
            }
        }
    }

    // Auto-use portal after the position is recorded.
    if stepped_onto_portal {
        let _ = use_portal(interiors, state, player_id);
    }
    Ok(())
}

// ── Enter / exit HTTP logic ────────────────────────

/// Result of an enter/exit request, ready to be serialized to the HTTP client.
#[derive(Debug)]
pub enum PortalTransitionResult {
    /// Success. Player was moved into the interior / back to the overworld.
    Moved { new_location: Location, tile: (i32, i32) },
    /// Player isn't standing on a portal tile.
    NotOnPortal,
    /// Interior id isn't loaded.
    UnknownInterior,
    /// Player_id not in state.
    UnknownPlayer,
}

/// Enter the portal the player is currently standing on. The player must
/// already be on the overworld at a tile that matches a portal entry defined
/// in some loaded interior (for now: any portal with
/// `PortalDest::Overworld { x, y }` — that's the "entrance" from the
/// overworld side, we just reverse it).
///
/// For Phase 1, we accept an explicit interior_id to enter + spawn coords,
/// which keeps the enter-from-overworld story simple (the admin or the
/// client tells us what cave we're walking into). Phase 2 attaches this to
/// a POI event.
pub fn enter_interior(
    interiors: &SharedInteriors,
    state: &SharedState,
    player_id: &str,
    interior_id: &str,
    spawn_tile: (usize, usize),
) -> PortalTransitionResult {
    let Some(interior) = interiors.get(interior_id) else {
        return PortalTransitionResult::UnknownInterior;
    };
    if !interior.is_walkable(spawn_tile.0, spawn_tile.1) {
        return PortalTransitionResult::NotOnPortal;
    }
    let mut lock = match state.lock() {
        Ok(l) => l,
        Err(e) => { tracing::warn!("enter_interior: mutex: {e}"); return PortalTransitionResult::UnknownPlayer; }
    };
    let Some(p) = lock.get_mut(player_id) else { return PortalTransitionResult::UnknownPlayer };

    // Save the tile we came FROM as the return, not the POI tile we're on.
    // If we returned to the POI tile, we'd immediately re-trigger the cave
    // entrance event. Fall back to current tile if we have no prev_tile yet.
    p.overworld_return = Some(p.prev_tile.unwrap_or((p.map_tile_x, p.map_tile_y)));
    p.location = Location::Interior { id: interior_id.to_string() };
    p.map_tile_x = spawn_tile.0 as i32;
    p.map_tile_y = spawn_tile.1 as i32;
    p.planned_route = String::new();
    p.route_meters_walked = 0.0;
    info!("[{}] entered interior {} @ ({},{})", p.name, interior_id, spawn_tile.0, spawn_tile.1);
    PortalTransitionResult::Moved {
        new_location: p.location.clone(),
        tile: (p.map_tile_x, p.map_tile_y),
    }
}

/// Take the portal at the player's current interior tile, or fall back to
/// `overworld_return` if they're somehow off-portal (defensive).
pub fn use_portal(
    interiors: &SharedInteriors,
    state: &SharedState,
    player_id: &str,
) -> PortalTransitionResult {
    let mut lock = match state.lock() {
        Ok(l) => l,
        Err(e) => { tracing::warn!("use_portal: mutex: {e}"); return PortalTransitionResult::UnknownPlayer; }
    };
    let Some(p) = lock.get_mut(player_id) else { return PortalTransitionResult::UnknownPlayer };
    let Some(interior_id) = p.location.interior_id().map(|s| s.to_string()) else {
        return PortalTransitionResult::NotOnPortal;
    };
    let Some(interior) = interiors.get(&interior_id) else {
        return PortalTransitionResult::UnknownInterior;
    };

    let portal_idx = interior.portal_at(p.map_tile_x as usize, p.map_tile_y as usize);
    let dest = match portal_idx.and_then(|i| interior.portals.get(i)) {
        Some(portal) => portal.destination.clone(),
        None => {
            // Fallback: pretend they took the exit-to-overworld portal.
            let (x, y) = p.overworld_return.unwrap_or((50, 40));
            PortalDest::Overworld { x, y }
        }
    };

    // Resolve OverworldReturn to a concrete overworld coord now, so the
    // rest of the match is uniform.
    let dest = match dest {
        PortalDest::OverworldReturn => {
            let (x, y) = p.overworld_return.unwrap_or((50, 40));
            PortalDest::Overworld { x, y }
        }
        other => other,
    };

    match dest {
        PortalDest::Overworld { x, y } => {
            p.location = Location::Overworld;
            p.map_tile_x = x;
            p.map_tile_y = y;
            p.overworld_return = None;
            p.planned_route = String::new();
            p.route_meters_walked = 0.0;
            info!("[{}] exited {} back to overworld ({},{})", p.name, interior_id, x, y);
            PortalTransitionResult::Moved {
                new_location: Location::Overworld, tile: (x, y),
            }
        }
        PortalDest::OverworldReturn => unreachable!("resolved above"),
        PortalDest::Interior { id, x, y } => {
            // No check that target interior exists — accept it; next tick's
            // run_interior_tick will no-op if unknown, but we keep the player
            // in a consistent "in interior X" state.
            p.location = Location::Interior { id: id.clone() };
            p.map_tile_x = x as i32;
            p.map_tile_y = y as i32;
            p.planned_route = String::new();
            p.route_meters_walked = 0.0;
            info!("[{}] traversed portal from {} to {} @ ({},{})", p.name, interior_id, id, x, y);
            PortalTransitionResult::Moved {
                new_location: Location::Interior { id }, tile: (x as i32, y as i32),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use questlib::interior::{InteriorTile, Portal};

    fn sample() -> InteriorMap {
        let tiles = vec![
            InteriorTile::Wall,  InteriorTile::Floor, InteriorTile::Wall,
            InteriorTile::Floor, InteriorTile::Floor, InteriorTile::Floor,
            InteriorTile::Wall,  InteriorTile::Floor, InteriorTile::Wall,
        ];
        InteriorMap {
            id: "cave".into(), name: "Cave".into(),
            width: 3, height: 3, tiles,
            portals: vec![Portal { x: 1, y: 0, destination: PortalDest::Overworld { x: 5, y: 5 }, label: "out".into() }],
            chests: vec![(1, 2)],
            floor_cost_m: 40,
        }
    }

    fn shared_state() -> SharedState {
        let mut p = DevPlayerState::default();
        p.id = "p1".into();
        p.name = "TestPlayer".into();
        p.map_tile_x = 10;
        p.map_tile_y = 10;
        let mut m = HashMap::new();
        m.insert(p.id.clone(), p);
        Arc::new(Mutex::new(m))
    }

    #[test]
    fn enter_then_exit_round_trip() {
        let mut interiors_map = HashMap::new();
        interiors_map.insert("cave".to_string(), sample());
        let interiors: SharedInteriors = Arc::new(interiors_map);

        let state = shared_state();
        let res = enter_interior(&interiors, &state, "p1", "cave", (1, 1));
        assert!(matches!(res, PortalTransitionResult::Moved { .. }));
        {
            let lock = state.lock().unwrap();
            let p = lock.get("p1").unwrap();
            assert_eq!(p.location.interior_id(), Some("cave"));
            assert_eq!(p.map_tile_x, 1);
            assert_eq!(p.map_tile_y, 1);
            assert_eq!(p.overworld_return, Some((10, 10)));
        }

        // Walk the player onto the portal tile, then use_portal
        {
            let mut lock = state.lock().unwrap();
            let p = lock.get_mut("p1").unwrap();
            p.map_tile_x = 1; p.map_tile_y = 0; // portal
        }
        let res = use_portal(&interiors, &state, "p1");
        assert!(matches!(res, PortalTransitionResult::Moved { .. }));
        let lock = state.lock().unwrap();
        let p = lock.get("p1").unwrap();
        assert_eq!(p.location, Location::Overworld);
        assert_eq!(p.map_tile_x, 5); // portal destination from the cave JSON
        assert_eq!(p.map_tile_y, 5);
    }

    #[test]
    fn enter_unknown_interior() {
        let interiors: SharedInteriors = Arc::new(HashMap::new());
        let state = shared_state();
        assert!(matches!(
            enter_interior(&interiors, &state, "p1", "nope", (0, 0)),
            PortalTransitionResult::UnknownInterior
        ));
    }

    #[test]
    fn enter_on_wall_rejected() {
        let mut m = HashMap::new();
        m.insert("cave".to_string(), sample());
        let interiors: SharedInteriors = Arc::new(m);
        let state = shared_state();
        // (0,0) is a wall in sample()
        assert!(matches!(
            enter_interior(&interiors, &state, "p1", "cave", (0, 0)),
            PortalTransitionResult::NotOnPortal
        ));
    }
}
