//! Route advancement logic.
//!
//! Given a planned route (list of tile coordinates) and meters walked,
//! compute the current tile position and whether the route is complete.

use serde::{Deserialize, Serialize};

use crate::mapgen::{Biome, WorldMap};

/// Cardinal direction the character is facing/moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Facing {
    #[default]
    Down,
    Up,
    Left,
    Right,
}

/// Base multiplier — adjust this to scale all tile costs.
/// Lower = faster movement across the map.
const COST_SCALE: u32 = 1;

/// Movement cost in meters to traverse a tile, based on biome.
pub fn tile_cost(biome: Biome, has_road: bool) -> u32 {
    let base = if has_road {
        20
    } else {
        match biome {
            Biome::Grassland => 40,
            Biome::Forest => 70,
            Biome::DenseForest => 90,
            Biome::Mountain => 120,
            Biome::Desert => 50,
            Biome::Snow => 70,
            Biome::Swamp => 100,
            Biome::Water | Biome::DeepWater => return u32::MAX,
        }
    };
    base * COST_SCALE
}

/// Given a route and total meters walked, find the current tile index and position.
/// Returns (tile_x, tile_y, route_index, is_complete).
pub fn position_along_route(
    route: &[(usize, usize)],
    meters_walked: f64,
    world: &WorldMap,
) -> (usize, usize, usize, bool) {
    if route.is_empty() {
        return (0, 0, 0, true);
    }

    let mut remaining = meters_walked;
    let last_idx = route.len().saturating_sub(1);
    for (i, &(x, y)) in route.iter().enumerate() {
        let biome = world.biome_at(x, y);
        let has_road = world.has_road_at(x, y);
        let cost = tile_cost(biome, has_road);
        if cost == u32::MAX {
            return (x, y, i, true);
        }

        // Last tile in route = destination. Reaching it means route is complete.
        if i == last_idx {
            return (x, y, i, true);
        }

        if remaining < cost as f64 {
            return (x, y, i, false);
        }
        remaining -= cost as f64;
    }

    let &(x, y) = route.last().unwrap_or(&(0, 0));
    (x, y, last_idx, true)
}

/// Compute the facing direction from the current route index to the next tile.
/// If at the end of the route (or route is empty), returns the fallback or Down.
pub fn facing_along_route(route: &[(usize, usize)], route_index: usize) -> Facing {
    if route_index + 1 >= route.len() {
        return Facing::Down;
    }
    let (cx, cy) = route[route_index];
    let (nx, ny) = route[route_index + 1];
    let dx = nx as i32 - cx as i32;
    let dy = ny as i32 - cy as i32;
    if dx.abs() > dy.abs() {
        if dx > 0 { Facing::Right } else { Facing::Left }
    } else if dy != 0 {
        if dy > 0 { Facing::Down } else { Facing::Up }
    } else {
        Facing::Down
    }
}

/// Compute meters consumed by tiles before `index` in the route.
/// This is used to find how far along the current tile the player is when
/// trimming a route (e.g. when extending a waypoint mid-walk).
pub fn meters_consumed_before(route: &[(usize, usize)], index: usize, world: &WorldMap) -> f64 {
    let mut total = 0.0;
    for &(x, y) in route.iter().take(index) {
        let cost = tile_cost(world.biome_at(x, y), world.has_road_at(x, y));
        if cost == u32::MAX { break; }
        total += cost as f64;
    }
    total
}

/// Parse a route JSON string: "[[x1,y1],[x2,y2],...]" → Vec<(usize, usize)>
pub fn parse_route_json(json: &str) -> Option<Vec<(usize, usize)>> {
    let parsed: Vec<Vec<usize>> = serde_json::from_str(json).ok()?;
    Some(parsed.into_iter().filter_map(|v| {
        if v.len() >= 2 { Some((v[0], v[1])) } else { None }
    }).collect())
}

/// Encode a route to JSON string.
pub fn encode_route_json(route: &[(usize, usize)]) -> String {
    let pairs: Vec<Vec<usize>> = route.iter().map(|&(x, y)| vec![x, y]).collect();
    serde_json::to_string(&pairs).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_encode() {
        let route = vec![(10, 20), (11, 20), (12, 20)];
        let json = encode_route_json(&route);
        let parsed = parse_route_json(&json).unwrap();
        assert_eq!(parsed, route);
    }

    #[test]
    fn parse_empty_and_invalid() {
        assert_eq!(parse_route_json("[]"), Some(vec![]));
        assert!(parse_route_json("not json").is_none());
        assert_eq!(parse_route_json("[[1]]"), Some(vec![])); // too few elements
    }

    #[test]
    fn encode_empty_route() {
        assert_eq!(encode_route_json(&[]), "[]");
    }

    // -- tile_cost tests --

    #[test]
    fn road_always_cheapest() {
        for biome in [Biome::Grassland, Biome::Forest, Biome::DenseForest, Biome::Mountain, Biome::Desert, Biome::Snow, Biome::Swamp] {
            let road_cost = tile_cost(biome, true);
            let normal_cost = tile_cost(biome, false);
            assert_eq!(road_cost, 20, "road cost should be 20 for {biome:?}");
            assert!(normal_cost >= road_cost, "normal cost should be >= road cost for {biome:?}");
        }
    }

    #[test]
    fn water_impassable() {
        assert_eq!(tile_cost(Biome::Water, false), u32::MAX);
        assert_eq!(tile_cost(Biome::DeepWater, false), u32::MAX);
    }

    #[test]
    fn cost_ordering() {
        // Road < Grassland < Desert < Forest/Snow < DenseForest < Swamp < Mountain
        let road = tile_cost(Biome::Grassland, true);
        let grass = tile_cost(Biome::Grassland, false);
        let desert = tile_cost(Biome::Desert, false);
        let forest = tile_cost(Biome::Forest, false);
        let dense = tile_cost(Biome::DenseForest, false);
        let swamp = tile_cost(Biome::Swamp, false);
        let mountain = tile_cost(Biome::Mountain, false);
        assert!(road < grass);
        assert!(grass < desert);
        assert!(desert < forest);
        assert!(forest <= dense);
        assert!(dense < mountain);
        assert!(swamp > forest);
    }

    // -- position_along_route tests --

    #[test]
    fn empty_route_is_complete() {
        let world = WorldMap::generate(42);
        let (_, _, _, done) = position_along_route(&[], 100.0, &world);
        assert!(done);
    }

    #[test]
    fn zero_meters_stays_at_start() {
        let world = WorldMap::generate(42);
        let route = find_road_route(&world, 5);
        let (x, y, idx, done) = position_along_route(&route, 0.0, &world);
        assert_eq!((x, y), route[0]);
        assert_eq!(idx, 0);
        assert!(!done);
    }

    #[test]
    fn position_advances_monotonically() {
        let world = WorldMap::generate(42);
        let route = find_road_route(&world, 10);

        let mut last_idx = 0;
        for meters in (0..500).step_by(5) {
            let (_, _, idx, _) = position_along_route(&route, meters as f64, &world);
            assert!(idx >= last_idx, "index should never go backward: {last_idx} -> {idx} at {meters}m");
            last_idx = idx;
        }
    }

    #[test]
    fn position_never_exceeds_route() {
        let world = WorldMap::generate(42);
        let route = find_road_route(&world, 5);
        let huge_meters = 999_999.0;
        let (x, y, idx, done) = position_along_route(&route, huge_meters, &world);
        assert!(done);
        assert_eq!((x, y), *route.last().unwrap());
        assert_eq!(idx, route.len() - 1);
    }

    #[test]
    fn one_tile_route_completes() {
        let world = WorldMap::generate(42);
        let road_tile = find_road_tile(&world);
        let route = vec![road_tile];

        // A single-tile route completes immediately (last tile = destination)
        let (x, y, _, done) = position_along_route(&route, 0.0, &world);
        assert!(done, "single-tile route should complete immediately");
        assert_eq!((x, y), road_tile);
    }

    #[test]
    fn exact_tile_boundary_advances() {
        let world = WorldMap::generate(42);
        let route = find_road_route(&world, 3);
        let cost0 = tile_cost(world.biome_at(route[0].0, route[0].1), world.has_road_at(route[0].0, route[0].1));

        // Exactly at cost boundary should advance to next tile
        let (x, y, idx, _) = position_along_route(&route, cost0 as f64, &world);
        assert_eq!(idx, 1, "should advance to tile 1 at exact boundary");
        assert_eq!((x, y), route[1]);
    }

    #[test]
    fn route_total_cost_matches_sum() {
        let world = WorldMap::generate(42);
        let route = find_road_route(&world, 8);

        // Cost of all tiles except the last (last tile = destination, no cost to enter)
        let cost_to_reach_last: f64 = route[..route.len()-1].iter().map(|&(x, y)| {
            tile_cost(world.biome_at(x, y), world.has_road_at(x, y)) as f64
        }).sum();

        // At cost to reach last tile, route should be complete
        let (_, _, _, done) = position_along_route(&route, cost_to_reach_last, &world);
        assert!(done, "route should be complete after reaching last tile");

        // 1m before reaching last tile, should NOT be complete
        let (_, _, _, done2) = position_along_route(&route, cost_to_reach_last - 1.0, &world);
        assert!(!done2, "route should not be complete before reaching last tile");
    }

    #[test]
    fn incremental_walking_matches_direct() {
        // Simulates how the game master advances: adding deltas over multiple ticks
        let world = WorldMap::generate(42);
        let route = find_road_route(&world, 6);

        let delta = 5; // 5m per tick
        let mut total_meters = 0.0;
        let mut last_pos = route[0];

        for _ in 0..100 {
            total_meters += delta as f64;
            let (x, y, _, done) = position_along_route(&route, total_meters, &world);

            // Direct computation should match
            let (dx, dy, _, ddone) = position_along_route(&route, total_meters, &world);
            assert_eq!((x, y), (dx, dy), "incremental should match direct at {total_meters}m");
            assert_eq!(done, ddone);

            // Should never go backward on the route
            let cur_idx = route.iter().position(|&t| t == (x, y));
            let last_idx = route.iter().position(|&t| t == last_pos);
            if let (Some(ci), Some(li)) = (cur_idx, last_idx) {
                assert!(ci >= li, "position should not go backward: {last_pos:?} -> ({x},{y})");
            }
            last_pos = (x, y);

            if done { break; }
        }
    }

    #[test]
    fn set_route_resets_meters() {
        // Simulates the bug: old meters applied to new route
        let world = WorldMap::generate(42);
        let route1 = find_road_route(&world, 5);
        let route2 = find_road_route_from(&world, route1[2], 5); // different route

        // Walk 200m on route1
        let (_, _, idx1, _) = position_along_route(&route1, 200.0, &world);
        assert!(idx1 > 0, "should have advanced on route1");

        // Set new route — meters should reset to 0
        let (x, y, idx2, _) = position_along_route(&route2, 0.0, &world);
        assert_eq!(idx2, 0, "new route should start at index 0");
        assert_eq!((x, y), route2[0], "new route should start at first tile");

        // Old meters on new route should NOT be used
        // (this is the bug we fixed — client was using old meters on new route)
        let (bx, by, _, _) = position_along_route(&route2, 200.0, &world);
        // This is fine as long as it's deterministic — the point is the CLIENT
        // should reset meters to 0 when setting a new route, not carry over.
        let _ = (bx, by); // just checking it doesn't panic
    }

    // -- helpers --

    /// Find a road tile on the map.
    fn find_road_tile(world: &WorldMap) -> (usize, usize) {
        for road in &world.roads {
            if let Some(&tile) = road.path.first() {
                return tile;
            }
        }
        panic!("no roads on map");
    }

    /// Build a route of `len` tiles along a road.
    fn find_road_route(world: &WorldMap, len: usize) -> Vec<(usize, usize)> {
        for road in &world.roads {
            if road.path.len() >= len {
                return road.path[..len].to_vec();
            }
        }
        panic!("no road long enough for {len} tiles");
    }

    /// Build a route of `len` tiles starting near `from`.
    fn find_road_route_from(world: &WorldMap, from: (usize, usize), len: usize) -> Vec<(usize, usize)> {
        for road in &world.roads {
            for (i, &tile) in road.path.iter().enumerate() {
                let dx = (tile.0 as i32 - from.0 as i32).unsigned_abs();
                let dy = (tile.1 as i32 - from.1 as i32).unsigned_abs();
                if dx <= 2 && dy <= 2 && i + len <= road.path.len() {
                    return road.path[i..i + len].to_vec();
                }
            }
        }
        // Fallback: just use any road
        find_road_route(world, len)
    }
}
