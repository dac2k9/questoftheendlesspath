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
    parse_entities_json, Behavior, BehaviorState, ContactAction, Facing, LoopMode,
    MobileEntityDef, MobileEntityState,
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
/// or removed in JSON). Also re-inits any entity whose saved
/// `spawn` no longer matches its def — that happens when the author
/// edits the JSON spawn coords, and without this check the entity
/// stays pinned to its old saved position forever. Idempotent —
/// safe to call on every startup.
pub fn ensure_states(
    defs: &HashMap<String, MobileEntityDef>,
    states: &mut HashMap<String, MobileEntityState>,
) {
    for (id, def) in defs.iter() {
        let needs_reset = match states.get(id) {
            None => true,
            Some(s) => s.spawn != def.spawn,
        };
        if needs_reset {
            states.insert(id.clone(), MobileEntityState::from_def(def));
        }
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
    shared_combat: &crate::combat::SharedCombat,
) {
    // Snapshot which entities are currently engaged in combat so we can
    // freeze their movement for the duration. Without this the wolf
    // (etc.) keeps wandering on the world map even while the player is
    // mid-fight in the combat overlay — visible as the sprite walking
    // off across the map.
    let engaged_ids: std::collections::HashSet<String> = shared_combat
        .lock()
        .ok()
        .map(|lock| {
            lock.keys()
                .filter_map(|k| k.strip_prefix(MOBILE_MONSTER_PREFIX).map(String::from))
                .collect()
        })
        .unwrap_or_default();

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
        // Frozen while a player is fighting this entity. last_step is
        // not advanced, so once combat ends the entity may step once
        // immediately (interval will have elapsed many times over),
        // then resume normal pacing. Acceptable behavior — wolf wakes
        // up and takes its first step right after victory/flee.
        if engaged_ids.contains(id) {
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

/// Pick the next tile for an entity. Returns `None` if the entity has
/// nowhere to go this tick (genuinely stuck — it'll retry next tick).
/// Returns `Some(state.current)` if the entity is intentionally
/// staying put (e.g. patrol with one waypoint, already at it).
fn pick_next_tile(
    def: &MobileEntityDef,
    state: &mut MobileEntityState,
    world: &WorldMap,
    rng_state: &mut u64,
) -> Option<(usize, usize)> {
    match &def.behavior {
        Behavior::Wander { radius } => pick_wander(def, state, world, *radius, rng_state),
        Behavior::Patrol { waypoints, loop_mode } => {
            pick_patrol(state, world, waypoints, *loop_mode)
        }
    }
}

/// Patrol: take one walkable step toward `waypoints[idx]`. On
/// reaching that waypoint, advance idx according to LoopMode (Wrap or
/// Bounce) and target the next.
fn pick_patrol(
    state: &mut MobileEntityState,
    world: &WorldMap,
    waypoints: &[(usize, usize)],
    loop_mode: LoopMode,
) -> Option<(usize, usize)> {
    if waypoints.is_empty() {
        return None;
    }
    // Pull mutable patrol state out of the entity. If the saved state
    // is the wrong variant (e.g. entity definition switched from
    // Wander to Patrol mid-run), reset to a fresh patrol.
    let (mut idx, mut forward) = match state.behavior_state {
        BehaviorState::Patrol { idx, forward } => (idx, forward),
        _ => (0, true),
    };
    if idx >= waypoints.len() {
        idx = 0;
        forward = true;
    }
    // If we're already at the current waypoint, advance the index
    // first so we have a fresh target this tick.
    if state.current == waypoints[idx] {
        let (next_idx, next_forward) = advance_patrol_idx(idx, forward, waypoints.len(), loop_mode);
        idx = next_idx;
        forward = next_forward;
    }
    state.behavior_state = BehaviorState::Patrol { idx, forward };
    let target = waypoints[idx];
    if state.current == target {
        return Some(state.current);
    }
    one_step_toward(state.current, target, world)
}

fn advance_patrol_idx(
    idx: usize,
    forward: bool,
    len: usize,
    loop_mode: LoopMode,
) -> (usize, bool) {
    if len <= 1 {
        return (0, true);
    }
    match loop_mode {
        LoopMode::Wrap => ((idx + 1) % len, true),
        LoopMode::Bounce => {
            if forward {
                if idx + 1 >= len {
                    (idx.saturating_sub(1), false)
                } else {
                    (idx + 1, true)
                }
            } else if idx == 0 {
                (1.min(len - 1), true)
            } else {
                (idx - 1, false)
            }
        }
    }
}

/// Take a single walkable cardinal step from `from` toward `to`.
/// Greedy heuristic — picks the axis with the largest delta first,
/// falling back to the other axis if the preferred neighbor isn't
/// walkable. Returns `None` if both candidate steps are blocked
/// (entity stays put for a tick, retries on the next).
fn one_step_toward(
    from: (usize, usize),
    to: (usize, usize),
    world: &WorldMap,
) -> Option<(usize, usize)> {
    let dx = to.0 as i32 - from.0 as i32;
    let dy = to.1 as i32 - from.1 as i32;
    let mut tries: [(i32, i32); 2] = [(0, 0), (0, 0)];
    if dx.abs() >= dy.abs() {
        if dx != 0 {
            tries[0] = (dx.signum(), 0);
        }
        if dy != 0 {
            tries[1] = (0, dy.signum());
        }
    } else {
        if dy != 0 {
            tries[0] = (0, dy.signum());
        }
        if dx != 0 {
            tries[1] = (dx.signum(), 0);
        }
    }
    for (sx, sy) in &tries {
        if *sx == 0 && *sy == 0 {
            continue;
        }
        let nx = from.0 as i32 + sx;
        let ny = from.1 as i32 + sy;
        if nx < 0
            || ny < 0
            || (nx as usize) >= world.width
            || (ny as usize) >= world.height
        {
            continue;
        }
        let cost = questlib::route::tile_cost(
            world.biome_at(nx as usize, ny as usize),
            world.has_road_at(nx as usize, ny as usize),
        );
        if cost == u32::MAX {
            continue;
        }
        return Some((nx as usize, ny as usize));
    }
    None
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

// ── Contact handling ────────────────────────────────────

/// Synthetic event-id prefix used for combats started by a mobile
/// monster contact. tick.rs's victory handler matches on this prefix
/// to credit the kill back to the entity (mark dead, set respawn).
pub const MOBILE_MONSTER_PREFIX: &str = "mobile_monster:";

pub fn combat_event_id_for(entity_id: &str) -> String {
    format!("{}{}", MOBILE_MONSTER_PREFIX, entity_id)
}

pub fn parse_combat_event_id(event_id: &str) -> Option<&str> {
    event_id.strip_prefix(MOBILE_MONSTER_PREFIX)
}

/// Walk every alive entity, check whether any player is in contact,
/// and start combat / push dialogue notifications. Runs after both
/// player and entity ticks so positions on both sides are fresh.
pub fn check_contacts(
    defs: &HashMap<String, MobileEntityDef>,
    entity_states: &SharedEntityStates,
    state: &crate::devserver::SharedState,
    shared_combat: &crate::combat::SharedCombat,
    shared_notifs: &crate::SharedNotifs,
) {
    // Snapshot overworld player positions so we don't hold the state
    // lock while we mutate entity states + combat.
    let players: Vec<(String, (usize, usize), u64, (i32, i32, i32))> = {
        let Ok(lock) = state.lock() else { return };
        lock.iter()
            .filter(|(_, p)| p.location.interior_id().is_none())
            .map(|(pid, p)| {
                let eq = questlib::items::equipment_bonuses(
                    &p.equipment,
                    crate::item_catalog(),
                    &p.item_upgrades,
                );
                (
                    pid.clone(),
                    (p.map_tile_x as usize, p.map_tile_y as usize),
                    p.total_distance_m as u64,
                    eq,
                )
            })
            .collect()
    };
    if players.is_empty() {
        return;
    }
    let Ok(mut entity_lock) = entity_states.lock() else { return };
    for (eid, def) in defs.iter() {
        let Some(s) = entity_lock.get_mut(eid) else { continue };
        if !s.alive {
            continue;
        }
        match &def.on_contact {
            ContactAction::Combat { difficulty } => {
                let event_id = combat_event_id_for(eid);
                // Spec: an entity is engaged by ONE player at a time.
                // If a combat keyed by this entity's event_id already
                // exists in shared_combat (anyone else fighting it),
                // skip — without this, every tick re-runs start_combat
                // and overwrites the combat state, ping-ponging between
                // players standing on the same tile and never letting
                // the fight resolve.
                let already_engaged = shared_combat
                    .lock()
                    .map(|lock| lock.contains_key(&event_id))
                    .unwrap_or(false);
                if already_engaged {
                    // If a player is actually standing on this entity's
                    // tile but we skip, that's the "stuck combat entry"
                    // failure mode (a previous fight that wasn't cleaned
                    // up). Log it once per second so it's easy to spot
                    // in the deploy logs.
                    for (pid, ppos, _, _) in &players {
                        if *ppos == s.current {
                            tracing::warn!(
                                "[mobile_entity] contact-skip: {} on tile {:?} of {} but stale combat entry blocks (event_id={})",
                                pid, ppos, eid, event_id,
                            );
                        }
                    }
                    continue;
                }
                for (pid, ppos, total_m, eq) in &players {
                    if *ppos != s.current {
                        continue;
                    }
                    if crate::combat::player_in_combat(shared_combat, pid) {
                        tracing::warn!(
                            "[mobile_entity] contact-skip: {} on tile {:?} of {} but player already in another combat",
                            pid, ppos, eid,
                        );
                        continue;
                    }
                    let display = def.name.clone().unwrap_or_else(|| eid.clone());
                    let kind = questlib::events::kind::EventKind::RandomEncounter {
                        enemy_name: display.clone(),
                        description: format!("{} attacks!", display),
                        difficulty: *difficulty,
                    };
                    crate::combat::start_combat(
                        shared_combat,
                        &event_id,
                        &kind,
                        *total_m,
                        *eq,
                        pid,
                    );
                    if let Ok(mut n) = shared_notifs.lock() {
                        crate::push_notif(&mut n, pid, format!("A {} attacks!", display));
                    }
                    tracing::info!("[mobile_entity] combat started: {} vs {}", pid, eid);
                    // Only one player can engage per tick — others on
                    // the same tile see the entity vanish (it's in
                    // combat with someone else) until the fight ends.
                    break;
                }
            }
            ContactAction::Dialogue { .. } | ContactAction::Trade { .. } => {
                let mut current_in_range: Vec<String> = Vec::new();
                for (pid, ppos, _, _) in &players {
                    let dx = (ppos.0 as i32 - s.current.0 as i32).abs();
                    let dy = (ppos.1 as i32 - s.current.1 as i32).abs();
                    if dx.max(dy) <= 1 {
                        current_in_range.push(pid.clone());
                    }
                }
                // First-approach detection: in current set but not in
                // last tick's set → push notification once.
                let display = def.name.clone().unwrap_or_else(|| eid.clone());
                for pid in &current_in_range {
                    if !s.nearby_players.contains(pid) {
                        if let Ok(mut n) = shared_notifs.lock() {
                            crate::push_notif(&mut n, pid, format!("{} is here.", display));
                        }
                    }
                }
                s.nearby_players = current_in_range;
            }
            ContactAction::None => {}
        }
    }
}

/// Mark an entity as dead and schedule its respawn (if configured).
/// Called by tick.rs from the combat-victory handler when it sees
/// an event id with the `mobile_monster:` prefix.
pub fn mark_killed(
    defs: &HashMap<String, MobileEntityDef>,
    states: &SharedEntityStates,
    entity_id: &str,
    now_unix_ms: u64,
) -> Option<MobileEntityDef> {
    let def = defs.get(entity_id)?.clone();
    let mut lock = states.lock().ok()?;
    let s = lock.get_mut(entity_id)?;
    s.alive = false;
    s.nearby_players.clear();
    s.respawn_at_unix_ms = match def.respawn_after_secs {
        Some(secs) => now_unix_ms + secs as u64 * 1000,
        None => 0,
    };
    Some(def)
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

    fn empty_combat() -> crate::combat::SharedCombat {
        Arc::new(Mutex::new(HashMap::new()))
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
            name: None,
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
    fn ensure_states_resets_when_spawn_edited() {
        // Saved state at (5, 5), but the def has been edited to spawn
        // somewhere else (the author moved the entity). ensure_states
        // should detect the mismatch and re-init.
        let mut defs = HashMap::new();
        defs.insert("a".into(), wolf_at((100, 100), 4));
        let mut states = HashMap::new();
        states.insert(
            "a".into(),
            MobileEntityState {
                current: (5, 5),
                spawn: (5, 5), // mirrors what the def used to say
                facing: Facing::Down,
                last_step_unix_ms: 0,
                behavior_state: BehaviorState::Wander,
                alive: true,
                respawn_at_unix_ms: 0,
                nearby_players: Vec::new(),
            },
        );
        ensure_states(&defs, &mut states);
        assert_eq!(states["a"].current, (100, 100));
        assert_eq!(states["a"].spawn, (100, 100));
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
        let combat = empty_combat();

        // First tick: last_step is 0 → interval (10000 ms) elapsed → step.
        tick_entities(&defs, &states, &world, 1_000_000_000, &mut rng, &combat);
        let pos_after_1 = states.lock().unwrap()["a"].current;
        let last_step_1 = states.lock().unwrap()["a"].last_step_unix_ms;
        assert_eq!(last_step_1, 1_000_000_000);

        // Second tick 1 s later: interval not yet elapsed → no step.
        tick_entities(&defs, &states, &world, 1_000_001_000, &mut rng, &combat);
        let pos_after_2 = states.lock().unwrap()["a"].current;
        let last_step_2 = states.lock().unwrap()["a"].last_step_unix_ms;
        assert_eq!(pos_after_1, pos_after_2);
        assert_eq!(last_step_1, last_step_2);

        // Third tick 11 s later: interval elapsed → step.
        tick_entities(&defs, &states, &world, 1_000_012_000, &mut rng, &combat);
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
                spawn: (20, 20),
                facing: Facing::Down,
                last_step_unix_ms: 0,
                behavior_state: BehaviorState::Wander,
                alive: false,
                respawn_at_unix_ms: 1_000,
                nearby_players: Vec::new(),
            },
        );
        let mut rng = 1u64;
        let combat = empty_combat();
        tick_entities(&defs, &states, &world, 2_000, &mut rng, &combat);
        let s = states.lock().unwrap()["a"].clone();
        assert!(s.alive);
        assert_eq!(s.current, (20, 20));
        assert_eq!(s.respawn_at_unix_ms, 0);
    }

    fn patrol_at(spawn: (usize, usize), waypoints: Vec<(usize, usize)>, mode: LoopMode) -> MobileEntityDef {
        MobileEntityDef {
            id: "p1".into(),
            kind: EntityKind::Npc,
            sprite: "skeleton_soldier".into(),
            spawn,
            behavior: Behavior::Patrol { waypoints, loop_mode: mode },
            // 60_000 tpm = 1 tile / ms, so each tick advances exactly
            // once when the test bumps now_unix_ms by 1 per call.
            movement: Movement { speed_tiles_per_min: 60_000 },
            on_contact: ContactAction::None,
            respawn_after_secs: None,
            name: None,
        }
    }

    #[test]
    fn patrol_walks_through_waypoints_wrap() {
        let world = flat_world();
        let mut defs = HashMap::new();
        // Short loop: (50,40) → (52,40) → (52,42) → wrap.
        defs.insert(
            "p1".into(),
            patrol_at((50, 40), vec![(50, 40), (52, 40), (52, 42)], LoopMode::Wrap),
        );
        let states = Arc::new(Mutex::new(HashMap::new()));
        ensure_states(&defs, &mut states.lock().unwrap());
        let mut rng = 1u64;
        let combat = empty_combat();
        let mut visited = std::collections::HashSet::new();
        for i in 0..40 {
            tick_entities(&defs, &states, &world, i + 1, &mut rng, &combat);
            visited.insert(states.lock().unwrap()["p1"].current);
        }
        // All three waypoints should have been touched.
        assert!(visited.contains(&(50, 40)));
        assert!(visited.contains(&(52, 40)));
        assert!(visited.contains(&(52, 42)));
    }

    #[test]
    fn patrol_bounce_reverses_at_endpoints() {
        let world = flat_world();
        let mut defs = HashMap::new();
        defs.insert(
            "p1".into(),
            patrol_at((30, 30), vec![(30, 30), (32, 30)], LoopMode::Bounce),
        );
        let states = Arc::new(Mutex::new(HashMap::new()));
        ensure_states(&defs, &mut states.lock().unwrap());
        let mut rng = 1u64;
        let combat = empty_combat();
        // Step long enough that we should reach (32, 30), then bounce back.
        let mut positions = Vec::new();
        for i in 0..15 {
            tick_entities(&defs, &states, &world, i + 1, &mut rng, &combat);
            positions.push(states.lock().unwrap()["p1"].current);
        }
        // We should hit (32,30) at some point AND come back through (30,30).
        let hit_far = positions.iter().any(|&p| p == (32, 30));
        let returned_home = positions
            .iter()
            .skip_while(|&&p| p != (32, 30))
            .any(|&p| p == (30, 30));
        assert!(hit_far, "never reached far waypoint: {:?}", positions);
        assert!(returned_home, "never returned home: {:?}", positions);
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
        let combat = empty_combat();
        // 100 steps; entity should never drift more than `radius + 1`
        // away (the +1 accounts for the moment it's exactly at the
        // boundary and trying to come back).
        for i in 0..100 {
            tick_entities(&defs, &states, &world, i + 1, &mut rng, &combat);
        }
        let s = states.lock().unwrap()["a"].clone();
        let dx = (s.current.0 as i32 - 50).abs();
        let dy = (s.current.1 as i32 - 40).abs();
        assert!(dx.max(dy) <= 3, "wandered too far: {:?}", s.current);
    }
}
