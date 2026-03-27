use std::collections::BinaryHeap;
use std::cmp::Ordering;

use bevy::prelude::*;

use super::world::{WorldGrid, WORLD_W, WORLD_H};

/// The player's planned route — a sequence of tile coordinates.
#[derive(Resource, Default)]
pub struct PlannedRoute {
    /// Ordered list of (x, y) tile coordinates from current position to destination.
    pub waypoints: Vec<(usize, usize)>,
    /// How many meters the player has walked into the current route.
    pub meters_walked: f32,
    /// Total meters required for the full route.
    pub total_meters: f32,
    /// Index of the current waypoint the player is walking toward.
    pub current_index: usize,
}

impl PlannedRoute {
    /// Get the current tile position based on meters walked.
    pub fn current_tile(&self) -> Option<(usize, usize)> {
        if self.waypoints.is_empty() {
            return None;
        }
        let idx = self.current_index.min(self.waypoints.len() - 1);
        Some(self.waypoints[idx])
    }

    /// Advance the route by the given meters. Returns true if the route is complete.
    pub fn advance(&mut self, meters: f32, world: &WorldGrid) -> bool {
        if self.waypoints.is_empty() || self.current_index >= self.waypoints.len() {
            return true;
        }

        self.meters_walked += meters;

        // Walk through waypoints consuming distance
        let mut remaining = self.meters_walked;
        let mut idx = 0;
        for i in 0..self.waypoints.len() {
            let (x, y) = self.waypoints[i];
            let cost = world.get(x, y).movement_cost() as f32;
            if cost >= u32::MAX as f32 {
                break;
            }
            if remaining < cost {
                idx = i;
                break;
            }
            remaining -= cost;
            idx = i + 1;
        }

        self.current_index = idx;
        self.current_index >= self.waypoints.len()
    }

    /// Recalculate total meters from waypoints.
    pub fn recalculate_total(&mut self, world: &WorldGrid) {
        self.total_meters = self
            .waypoints
            .iter()
            .map(|&(x, y)| {
                let cost = world.get(x, y).movement_cost();
                if cost == u32::MAX { 0.0 } else { cost as f32 }
            })
            .sum();
    }
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
            // Reconstruct path
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

    None // No path found
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
    // Manhattan distance * minimum tile cost (100m for road)
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
        // Reverse for min-heap
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
