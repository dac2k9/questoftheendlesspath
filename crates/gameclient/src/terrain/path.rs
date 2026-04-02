use std::collections::BinaryHeap;
use std::cmp::Ordering;

use bevy::prelude::*;

use super::world::{WorldGrid, WORLD_W, WORLD_H, TILE_PX};

/// Route displayed on screen (for path markers).
#[derive(Resource, Default)]
pub struct DisplayRoute {
    pub waypoints: Vec<(usize, usize)>,
    /// True if user just set/cleared route locally — don't overwrite from server until next poll confirms.
    pub locally_modified: bool,
}

/// Server-driven interpolation state. The server tells us where to animate toward,
/// and we lerp smoothly over the given duration. Can never overshoot.
#[derive(Resource, Default)]
pub struct InterpolationState {
    /// Confirmed route_meters at last poll.
    pub start_meters: f64,
    /// Server's interpolation target (max meters we may animate toward).
    pub target_meters: f64,
    /// Seconds elapsed since this interpolation started.
    pub elapsed: f32,
    /// Duration over which to reach target (from server, typically 1.0s).
    pub duration: f32,
}

impl InterpolationState {
    /// Current display meters, lerped between start and target. Never exceeds target.
    pub fn current_meters(&self) -> f64 {
        if self.duration <= 0.0 {
            return self.start_meters;
        }
        let t = (self.elapsed / self.duration).clamp(0.0, 1.0) as f64;
        // Smoothstep for ease-out near target (reduces snap on next poll)
        let t = t * t * (3.0 - 2.0 * t);
        self.start_meters + (self.target_meters - self.start_meters) * t
    }
}

/// Compute world-space position from a route + meters walked.
/// Walks through tiles consuming meter costs, interpolates within the current tile.
pub fn position_from_route_meters(
    route: &[(usize, usize)],
    meters: f64,
    world: &WorldGrid,
) -> Option<Vec2> {
    if route.is_empty() {
        return None;
    }

    let mut remaining = meters;
    for i in 0..route.len() {
        let (x, y) = route[i];
        // Use server-compatible cost to stay in sync with game master
        let cost = world.server_tile_cost(x, y);
        if cost == u32::MAX {
            // Impassable — stop here
            return Some(WorldGrid::tile_to_world(x, y));
        }

        if remaining < cost as f64 {
            // Partway through this tile — interpolate to next
            let frac = remaining / cost as f64;
            let current = WorldGrid::tile_to_world(x, y);

            if i + 1 < route.len() {
                let (nx, ny) = route[i + 1];
                let next = WorldGrid::tile_to_world(nx, ny);
                let interp_x = current.x + (next.x - current.x) * frac as f32;
                let interp_y = current.y + (next.y - current.y) * frac as f32;
                return Some(Vec2::new(interp_x, interp_y));
            } else {
                return Some(current);
            }
        }
        remaining -= cost as f64;
    }

    // Past the end of the route — return last tile
    let (x, y) = route[route.len() - 1];
    Some(WorldGrid::tile_to_world(x, y))
}

/// Compute world-space position and route index from a route + meters walked.
pub fn position_and_index_from_route_meters(
    route: &[(usize, usize)],
    meters: f64,
    world: &WorldGrid,
) -> Option<(Vec2, usize)> {
    if route.is_empty() {
        return None;
    }

    let mut remaining = meters;
    for i in 0..route.len() {
        let (x, y) = route[i];
        let cost = world.server_tile_cost(x, y);
        if cost == u32::MAX {
            return Some((WorldGrid::tile_to_world(x, y), i));
        }

        if remaining < cost as f64 {
            let frac = remaining / cost as f64;
            let current = WorldGrid::tile_to_world(x, y);

            if i + 1 < route.len() {
                let (nx, ny) = route[i + 1];
                let next = WorldGrid::tile_to_world(nx, ny);
                let interp_x = current.x + (next.x - current.x) * frac as f32;
                let interp_y = current.y + (next.y - current.y) * frac as f32;
                return Some((Vec2::new(interp_x, interp_y), i));
            } else {
                return Some((current, i));
            }
        }
        remaining -= cost as f64;
    }

    let (x, y) = route[route.len() - 1];
    Some((WorldGrid::tile_to_world(x, y), route.len().saturating_sub(1)))
}

/// Find which tile index in a route corresponds to a given meters walked.
pub fn tile_index_from_meters(
    route: &[(usize, usize)],
    meters: f64,
    world: &WorldGrid,
) -> usize {
    let mut remaining = meters;
    for i in 0..route.len() {
        let (x, y) = route[i];
        // Use server-compatible cost to stay in sync with game master
        let cost = world.server_tile_cost(x, y);
        if cost == u32::MAX || remaining < cost as f64 {
            return i;
        }
        remaining -= cost as f64;
    }
    route.len().saturating_sub(1)
}

/// A* pathfinding between two tiles, using movement cost as weight.
pub fn find_path(
    world: &WorldGrid,
    start: (usize, usize),
    goal: (usize, usize),
) -> Option<Vec<(usize, usize)>> {
    find_path_with_items(world, start, goal, &[])
}

pub fn find_path_with_items(
    world: &WorldGrid,
    start: (usize, usize),
    goal: (usize, usize),
    inventory_ids: &[String],
) -> Option<Vec<(usize, usize)>> {
    if !world.get(goal.0, goal.1).is_passable() {
        return None;
    }
    // Check if goal biome requires an item we don't have
    let goal_biome = world.map.biome_at(goal.0, goal.1);
    if let Some(req) = goal_biome.required_item() {
        if !inventory_ids.iter().any(|id| id == req) {
            return None;
        }
    }

    let w = world.width;
    let h = world.height;
    let idx = |x: usize, y: usize| y * w + x;

    let mut dist = vec![u32::MAX; w * h];
    let mut prev = vec![None::<(usize, usize)>; w * h];
    let mut heap = BinaryHeap::new();

    dist[idx(start.0, start.1)] = 0;
    heap.push(Node {
        cost: 0,
        heuristic: heuristic(start, goal),
        pos: start,
    });

    while let Some(Node { cost, pos, .. }) = heap.pop() {
        let (x, y) = pos;

        if pos == goal {
            let mut path = vec![goal];
            let mut cur = goal;
            while let Some(p) = prev[idx(cur.0, cur.1)] {
                path.push(p);
                cur = p;
                if cur == start {
                    break;
                }
            }
            path.reverse();
            return Some(path);
        }

        if cost > dist[idx(x, y)] {
            continue;
        }

        for (nx, ny) in neighbors(x, y) {
            if nx >= w || ny >= h {
                continue;
            }
            let terrain = world.get(nx, ny);
            if !terrain.is_passable() {
                continue;
            }
            // Block tiles that require an item the player doesn't have
            let biome = world.map.biome_at(nx, ny);
            if let Some(req) = biome.required_item() {
                if !inventory_ids.iter().any(|id| id == req) {
                    continue;
                }
            }

            let move_cost = terrain.movement_cost();
            let new_cost = cost.saturating_add(move_cost);

            if new_cost < dist[idx(nx, ny)] {
                dist[idx(nx, ny)] = new_cost;
                prev[idx(nx, ny)] = Some(pos);
                heap.push(Node {
                    cost: new_cost,
                    heuristic: heuristic((nx, ny), goal),
                    pos: (nx, ny),
                });
            }
        }
    }

    None
}

fn neighbors(x: usize, y: usize) -> [(usize, usize); 4] {
    [
        (x.wrapping_sub(1), y),
        (x + 1, y),
        (x, y.wrapping_sub(1)),
        (x, y + 1),
    ]
}

fn heuristic(a: (usize, usize), b: (usize, usize)) -> u32 {
    let dx = (a.0 as i32 - b.0 as i32).unsigned_abs();
    let dy = (a.1 as i32 - b.1 as i32).unsigned_abs();
    (dx + dy) * 100
}

#[derive(Eq, PartialEq)]
struct Node {
    cost: u32,
    heuristic: u32,
    pos: (usize, usize),
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        let self_f = self.cost.saturating_add(self.heuristic);
        let other_f = other.cost.saturating_add(other.heuristic);
        other_f.cmp(&self_f)
    }
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
