//! Mobile-entity tick loop and JSON loader.
//!
//! Authored definitions (`MobileEntityDef`) load once from JSON and
//! are immutable for the process lifetime. Runtime state
//! (`MobileEntityState`) is mutated each tick and saved to disk.
//!
//! The tick advances each alive entity at its own pace
//! (`speed_tiles_per_min`), picks a next tile per the entity's
//! `Behavior`, and updates current tile + facing. MVP only
//! implements `Wander`; `Patrol` is wired but a no-op until step 5.
//!
//! See `adventures/MOBILE_ENTITIES.md` for the design.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use questlib::mapgen::WorldMap;
use questlib::mobile_entity::{
    parse_entities_json, Behavior, BehaviorState, Facing, MobileEntityDef,
    MobileEntityState,
};

/// Authored entity definitions, keyed by id. Read-only after load.
pub type SharedEntityDefs = Arc<HashMap<String, MobileEntityDef>>;

/// Mutable runtime state per entity, keyed by id. Saved to disk.
pub type SharedEntityStates = Arc<Mutex<HashMap<String, MobileEntityState>>>;

/// Load and parse an entities JSON file. Missing file → empty map
/// (so adventures without mobile entities are valid).
pub fn load_entities(path: &str) -> Result<HashMap<String, MobileEntityDef>> {
    if !Path::new(path).exists() {
        return Ok(HashMap::new());
    }
    let json = std::fs::read_to_string(path).context("read entities file")?;
    let list = parse_entities_json(&json).context("parse entities json")?;
    let mut map = HashMap::with_capacity(list.len());
    for def in list {
        map.insert(def.id.clone(), def);
    }
    Ok(map)
}

/// Ensure every authored entity has matching runtime state, and drop
/// runtime state for entities whose definitions disappeared (renamed
/// or removed in JSON). Idempotent — safe to call on every startup.
pub fn ensure_states(
    defs: &HashMap<String, MobileEntityDef>,
    states: &mut HashMap<String, MobileEntityState>,
) {
    for (id, def) in defs.iter() {
        states.entry(id.clone()).or_insert_with(|| MobileEntityState::from_def(def));
    }
    states.retain(|id, _| defs.contains_key(id));
}

/// Advance the world's mobile entities by one server tick.
///
/// For each entity:
///   - **Dead and respawn-due**: revive at `def.spawn`, reset
///     behavior state, refresh `last_step`.
///   - **Alive and step-due**: pick a next tile per the entity's
///     `Behavior`, update `current` / `facing` / `last_step`.
///
/// Step pacing comes from `def.movement.speed_tiles_per_min`:
/// `step_interval_ms = 60_000 / speed`. An entity ticked between
/// step-intervals just sits there.
pub fn tick_entities(
    defs: &HashMap<String, MobileEntityDef>,
    states: &SharedEntityStates,
    world: &WorldMap,
    now_unix_ms: u64,
    rng_state: &mut u64,
) {
    let mut lock = match states.lock() {
        Ok(l) => l,
        Err(_) => return,
    };
    for (id, def) in defs.iter() {
        let s = match lock.get_mut(id) {
            Some(s) => s,
            None => continue,
        };
        // Respawn handling.
        if !s.alive {
            if s.respawn_at_unix_ms != 0 && now_unix_ms >= s.respawn_at_unix_ms {
                s.alive = true;
                s.current = def.spawn;
                s.respawn_at_unix_ms = 0;
                s.last_step_unix_ms = now_unix_ms;
                s.behavior_state = BehaviorState::for_behavior(&def.behavior);
                s.facing = Facing::Down;
            }
            continue;
        }
        // Step pacing.
        let speed = def.movement.speed_tiles_per_min.max(1) as u64;
        let interval_ms = 60_000 / speed;
        if now_unix_ms.saturating_sub(s.last_step_unix_ms) < interval_ms {
            continue;
        }
        // Pick + apply.
        if let Some(next) = pick_next_tile(def, s, world, rng_state) {
            if next != s.current {
                s.facing = direction_to_facing(s.current, next);
                s.current = next;
            }
            s.last_step_unix_ms = now_unix_ms;
        }
    }
}

fn pick_next_tile(
    def: &MobileEntityDef,
    state: &MobileEntityState,
    world: &WorldMap,
    rng_state: &mut u64,
) -> Option<(usize, usize)> {
    match &def.behavior {
        Behavior::Wander { radius } => pick_wander(def, state, world, *radius, rng_state),
        // Step 5 implements this. Returning current keeps the entity
        // stationary in the meantime — fine for testing the rest of
        // the pipeline.
        Behavior::Patrol { .. } => Some(state.current),
    }
}

/// Wander: pick a random walkable cardinal neighbor. If currently
/// outside the home radius, restrict candidates to ones that bring
/// us closer to spawn — the entity gravitates back rather than
/// drifting forever.
fn pick_wander(
    def: &MobileEntityDef,
    state: &MobileEntityState,
    world: &WorldMap,
    radius: u32,
    rng_state: &mut u64,
) -> Option<(usize, usize)> {
    let (cx, cy) = (state.current.0 as i32, state.current.1 as i32);
    let (sx, sy) = (def.spawn.0 as i32, def.spawn.1 as i32);
    let dist_now = (cx - sx).abs().max((cy - sy).abs()) as u32;
    let outside = dist_now >= radius;

    let candidates: Vec<(i32, i32)> = [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)]
        .iter()
        .filter_map(|&(dx, dy)| {
            let nx = cx + dx;
            let ny = cy + dy;
            if nx < 0 || ny < 0
                || (nx as usize) >= world.width
                || (ny as usize) >= world.height
            {
                return None;
            }
            let cost = questlib::route::tile_cost(
                world.biome_at(nx as usize, ny as usize),
                world.has_road_at(nx as usize, ny as usize),
            );
            if cost == u32::MAX {
                return None;
            }
            if outside {
                let new_dist = (nx - sx).abs().max((ny - sy).abs()) as u32;
                if new_dist >= dist_now {
                    return None;
                }
            }
            Some((nx, ny))
        })
        .collect();

    if candidates.is_empty() {
        // Stuck (e.g. island in water). Stay put — fine for prototype.
        return None;
    }
    let idx = (next_rng(rng_state) as usize) % candidates.len();
    let (nx, ny) = candidates[idx];
    Some((nx as usize, ny as usize))
}

/// LCG step matching tick.rs's RNG so the two streams behave alike.
fn next_rng(state: &mut u64) -> u32 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
    (*state >> 33) as u32
}

fn direction_to_facing(from: (usize, usize), to: (usize, usize)) -> Facing {
    let dx = to.0 as i32 - from.0 as i32;
    let dy = to.1 as i32 - from.1 as i32;
    if dx.abs() > dy.abs() {
        if dx > 0 { Facing::Right } else { Facing::Left }
    } else if dy > 0 {
        Facing::Down
    } else {
        Facing::Up
    }
}

// ── Tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use questlib::mobile_entity::{ContactAction, EntityKind, Movement};

    fn flat_world() -> WorldMap {
        // Tiny grass world — every tile walkable.
        WorldMap::generate(99)
    }

    fn wolf_at(spawn: (usize, usize), radius: u32) -> MobileEntityDef {
        MobileEntityDef {
            id: "test_wolf".into(),
            kind: EntityKind::Monster,
            sprite: "wolf".into(),
            spawn,
            behavior: Behavior::Wander { radius },
            movement: Movement { speed_tiles_per_min: 60 }, // 1 tile/sec
            on_contact: ContactAction::Combat { difficulty: 1 },
            respawn_after_secs: None,
        }
    }

    #[test]
    fn ensure_states_creates_missing() {
        let mut defs = HashMap::new();
        defs.insert("a".into(), wolf_at((10, 10), 4));
        let mut states = HashMap::new();
        ensure_states(&defs, &mut states);
        assert!(states.contains_key("a"));
        assert_eq!(states["a"].current, (10, 10));
    }

    #[test]
    fn ensure_states_prunes_orphans() {
        let defs: HashMap<String, MobileEntityDef> = HashMap::new();
        let mut states = HashMap::new();
        states.insert("orphan".into(), MobileEntityState::from_def(&wolf_at((0, 0), 1)));
        ensure_states(&defs, &mut states);
        assert!(!states.contains_key("orphan"));
    }

    #[test]
    fn tick_respects_step_interval() {
        let world = flat_world();
        let mut defs = HashMap::new();
        // 6 tiles/min = 1 step every 10 s.
        let mut wolf = wolf_at((10, 10), 4);
        wolf.movement.speed_tiles_per_min = 6;
        defs.insert("a".into(), wolf);
        let states = Arc::new(Mutex::new(HashMap::new()));
        ensure_states(&defs, &mut states.lock().unwrap());
        let mut rng = 1u64;

        // First tick: last_step is 0 → interval (10000 ms) elapsed → step.
        tick_entities(&defs, &states, &world, 1_000_000_000, &mut rng);
        let pos_after_1 = states.lock().unwrap()["a"].current;
        let last_step_1 = states.lock().unwrap()["a"].last_step_unix_ms;
        assert_eq!(last_step_1, 1_000_000_000);

        // Second tick 1 s later: interval not yet elapsed → no step.
        tick_entities(&defs, &states, &world, 1_000_001_000, &mut rng);
        let pos_after_2 = states.lock().unwrap()["a"].current;
        let last_step_2 = states.lock().unwrap()["a"].last_step_unix_ms;
        assert_eq!(pos_after_1, pos_after_2);
        assert_eq!(last_step_1, last_step_2);

        // Third tick 11 s later: interval elapsed → step.
        tick_entities(&defs, &states, &world, 1_000_012_000, &mut rng);
        let last_step_3 = states.lock().unwrap()["a"].last_step_unix_ms;
        assert_eq!(last_step_3, 1_000_012_000);
    }

    #[test]
    fn dead_entity_respawns() {
        let world = flat_world();
        let mut defs = HashMap::new();
        let mut wolf = wolf_at((20, 20), 3);
        wolf.respawn_after_secs = Some(60);
        defs.insert("a".into(), wolf);
        let states = Arc::new(Mutex::new(HashMap::new()));
        // Manually mark dead with an expired respawn timer.
        states.lock().unwrap().insert(
            "a".into(),
            MobileEntityState {
                current: (5, 5),
                facing: Facing::Down,
                last_step_unix_ms: 0,
                behavior_state: BehaviorState::Wander,
                alive: false,
                respawn_at_unix_ms: 1_000,
            },
        );
        let mut rng = 1u64;
        tick_entities(&defs, &states, &world, 2_000, &mut rng);
        let s = states.lock().unwrap()["a"].clone();
        assert!(s.alive);
        assert_eq!(s.current, (20, 20));
        assert_eq!(s.respawn_at_unix_ms, 0);
    }

    #[test]
    fn wander_stays_within_radius_over_many_ticks() {
        let world = flat_world();
        let mut defs = HashMap::new();
        let mut wolf = wolf_at((50, 40), 2);
        wolf.movement.speed_tiles_per_min = 6_000; // step every ms — fast iteration
        defs.insert("a".into(), wolf);
        let states = Arc::new(Mutex::new(HashMap::new()));
        ensure_states(&defs, &mut states.lock().unwrap());
        let mut rng = 42u64;
        // 100 steps; entity should never drift more than `radius + 1`
        // away (the +1 accounts for the moment it's exactly at the
        // boundary and trying to come back).
        for i in 0..100 {
            tick_entities(&defs, &states, &world, i + 1, &mut rng);
        }
        let s = states.lock().unwrap()["a"].clone();
        let dx = (s.current.0 as i32 - 50).abs();
        let dy = (s.current.1 as i32 - 40).abs();
        assert!(dx.max(dy) <= 3, "wandered too far: {:?}", s.current);
    }
}
