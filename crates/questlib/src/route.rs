//! Route advancement logic.
//!
//! Given a planned route (list of tile coordinates) and meters walked,
//! compute the current tile position and whether the route is complete.

use crate::mapgen::{Biome, WorldMap};

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
    for (i, &(x, y)) in route.iter().enumerate() {
        let biome = world.biome_at(x, y);
        let has_road = world.has_road_at(x, y);
        let cost = tile_cost(biome, has_road);
        if cost == u32::MAX {
            // Impassable — stop here
            return (x, y, i, true);
        }

        if remaining < cost as f64 {
            // Still on this tile
            return (x, y, i, false);
        }
        remaining -= cost as f64;
    }

    // Completed the entire route
    let &(x, y) = route.last().unwrap_or(&(0, 0));
    (x, y, route.len().saturating_sub(1), true)
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
    fn position_along_simple_route() {
        let world = WorldMap::generate(42);
        // Use road POI positions which are guaranteed passable
        let start = world.pois.iter()
            .find(|p| p.has_road)
            .map(|p| (p.x, p.y))
            .unwrap_or((50, 40));

        let route = vec![start, (start.0 + 1, start.1), (start.0 + 2, start.1)];

        // At 0 meters, should be at start
        let (x, y, idx, done) = position_along_route(&route, 0.0, &world);
        assert_eq!((x, y), start);
        assert_eq!(idx, 0);
        assert!(!done);

        // After 2000 meters, should have advanced (even worst terrain is 600m)
        let (_, _, idx2, _) = position_along_route(&route, 2000.0, &world);
        assert!(idx2 > 0, "should have advanced past first tile");
    }
}
