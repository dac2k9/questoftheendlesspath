use std::collections::BinaryHeap;
use std::cmp::Ordering;

use bevy::prelude::*;

use super::world::{WorldGrid, WORLD_W, WORLD_H, TILE_PX};

/// Route displayed on screen (for path markers).
#[derive(Resource, Default)]
pub struct DisplayRoute {
    pub waypoints: Vec<(usize, usize)>,
}

/// Predicted meters since last server poll (dead reckoning).
#[derive(Resource, Default)]
pub struct PredictedMeters(pub f64);

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
        let terrain = world.get(x, y);
        let cost = terrain.movement_cost();
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

/// Find which tile index in a route corresponds to a given meters walked.
pub fn tile_index_from_meters(
    route: &[(usize, usize)],
    meters: f64,
    world: &WorldGrid,
) -> usize {
    let mut remaining = meters;
    for i in 0..route.len() {
        let (x, y) = route[i];
        let cost = world.get(x, y).movement_cost();
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
    if !world.get(goal.0, goal.1).is_passable() {
        return None;
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
