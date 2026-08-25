//! Server-side combat manager.
//!
//! Stores active combat states and ticks them forward each game loop iteration.
//!
//! ## Per-player combat sessions
//!
//! `shared_combat` is keyed by a *session key*, NOT by the bare
//! `event_id`. A solo fight is keyed `{event_id}\x1f{player_id}` so two
//! players who independently trigger the same event (the same boss POI,
//! the same world monster) get INDEPENDENT combat states instead of
//! clobbering each other's `CombatState` on insert. That clobbering was
//! the root of a whole family of live bugs: the Frost Queen that
//! "wouldn't start", wolves cross-feeding one player's fight into
//! another's, etc.
//!
//! A co-op fight (a boss with `requires_coop`, multiple present players)
//! is keyed by the bare `event_id` so every participant ticks the SAME
//! session — that's the existing co-op behavior, preserved. The
//! `coop_players` vec on `CombatState` lists the participants.
//!
//! Routing (victory rewards, event completion) reads `cs.event_id` from
//! the state, never the map key — the key may carry a player suffix.
//!
//! Future direction (noted, not built): generalize this into an explicit
//! "fight session" type that holds 1..N players and an open/closed flag,
//! so mid-fight joins and richer co-op rules have a home. Today's
//! solo-vs-coop key split is the minimum that makes solo fights correct
//! without regressing co-op.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use questlib::combat::{self, CombatState};
use questlib::events::kind::EventKind;

pub type SharedCombat = Arc<Mutex<HashMap<String, CombatState>>>;

/// ASCII Unit Separator — can't appear in an event_id or player_id, so
/// `{event_id}` (co-op) and `{event_id}\x1f{player_id}` (solo) never
/// collide.
const SESSION_SEP: char = '\u{1f}';

/// Build the map key for a combat session. See module docs.
fn session_key(event_id: &str, player_id: &str, coop: bool) -> String {
    if coop {
        event_id.to_string()
    } else {
        format!("{event_id}{SESSION_SEP}{player_id}")
    }
}

/// Initialize combat for an event and add it to the shared map.
///
/// `coop_players` lists every participant for a co-op fight; pass an
/// empty slice for a solo fight. When more than one participant is
/// given the session is shared (keyed by the bare event_id) and
/// `coop_players` is recorded on the state; otherwise it's a per-player
/// solo session. Returns the session key it inserted under (callers
/// that need to mutate the freshly-created state should use it).
pub fn start_combat(
    shared: &SharedCombat,
    event_id: &str,
    kind: &EventKind,
    total_distance_m: u64,
    equipment_bonuses: (i32, i32, i32),
    player_id: &str,
    coop_players: &[String],
) -> String {
    let coop = coop_players.len() > 1;
    let mut state = combat::init_combat(event_id, kind, total_distance_m, equipment_bonuses, player_id);
    if coop {
        state.coop_players = coop_players.to_vec();
    }
    let key = session_key(event_id, player_id, coop);
    let mut lock = shared.lock().unwrap();
    lock.insert(key.clone(), state);
    key
}

/// Tick all active combats using per-player speeds. `player_speeds` is
/// (player_id, speed_kmh, incline_pct). Returns (victory_keys,
/// retreat_keys) — the SESSION KEYS, which callers pass to
/// `remove_combat` and look up to read `cs.event_id` for routing.
///
/// `shared` holds combat sessions for EVERY adventure bundle — it's one
/// global map, not scoped per-bundle. `run_tick_dev` calls this once per
/// bundle per cycle, each time with only that bundle's own event/entity
/// catalogs available for resolving a victory. `in_bundle` restricts
/// ticking (and thus victory/retreat resolution) to sessions whose
/// fighter belongs to the CURRENT bundle — otherwise, whichever bundle's
/// call happened to land the killing blow would resolve it against its
/// own catalog, silently dropping gold/items/completion for a fight that
/// belongs to a different adventure (event_id / entity_id not found
/// there). A session skipped here is left untouched for its own
/// bundle's next call to pick up.
pub fn tick_all(
    shared: &SharedCombat,
    player_speeds: &[(String, f32, f32)],
    delta_secs: f32,
    in_bundle: &HashSet<String>,
) -> (Vec<String>, Vec<String>) {
    let mut lock = shared.lock().unwrap();
    let mut victories = Vec::new();
    let mut retreats = Vec::new();

    for (session_key, state) in lock.iter_mut() {
        if !in_bundle.contains(&state.player_id) {
            continue;
        }
        // Combine walking speeds from all coop players (sum for faster fights)
        let (speed, incline) = if state.coop_players.len() > 1 {
            let total_speed: f32 = state.coop_players.iter()
                .filter_map(|pid| player_speeds.iter().find(|(id, _, _)| id == pid))
                .map(|(_, s, _)| *s)
                .sum();
            let max_incline: f32 = state.coop_players.iter()
                .filter_map(|pid| player_speeds.iter().find(|(id, _, _)| id == pid))
                .map(|(_, _, i)| *i)
                .fold(0.0_f32, f32::max);
            (total_speed, max_incline)
        } else {
            player_speeds.iter()
                .find(|(pid, _, _)| pid == &state.player_id)
                .map(|(_, s, i)| (*s, *i))
                .unwrap_or((0.0, 0.0))
        };
        combat::tick_combat(state, speed, incline, delta_secs);
        match state.status {
            combat::CombatStatus::Victory => victories.push(session_key.clone()),
            combat::CombatStatus::Defeat | combat::CombatStatus::Fled => retreats.push(session_key.clone()),
            _ => {}
        }
    }

    (victories, retreats)
}

/// Get the active combat for a specific player (solo or coop), if any.
pub fn get_combat_for_player(shared: &SharedCombat, player_id: &str) -> Option<CombatState> {
    let lock = shared.lock().unwrap();
    lock.values()
        .find(|c| c.player_id == player_id || c.coop_players.iter().any(|p| p == player_id))
        .cloned()
}

/// Check if this player is currently in any combat (solo or coop).
pub fn player_in_combat(shared: &SharedCombat, player_id: &str) -> bool {
    let lock = shared.lock().unwrap();
    lock.values().any(|c| c.player_id == player_id || c.coop_players.iter().any(|p| p == player_id))
}

/// Is there any live combat session for this CONTENT event_id, by any
/// player? Value-based (not a key lookup) because solo sessions carry a
/// player suffix in the key. Used by the mobile-entity "one player per
/// entity" rule so a second player can't start a fresh fight against a
/// monster someone else is already engaging.
pub fn event_combat_active(shared: &SharedCombat, event_id: &str) -> bool {
    let lock = shared.lock().unwrap();
    lock.values().any(|c| c.event_id == event_id)
}

/// Find the session key for the combat this player is in, if any.
fn session_key_for_player(lock: &HashMap<String, CombatState>, player_id: &str) -> Option<String> {
    lock.iter()
        .find(|(_, c)| c.player_id == player_id || c.coop_players.iter().any(|p| p == player_id))
        .map(|(k, _)| k.clone())
}

/// Player runs away from their own combat. Resolves the player → session
/// key internally so callers needn't know the key shape. (Replaces the
/// old `flee(event_id)` — callers only know the player, and the key now
/// carries a player suffix for solo fights.)
pub fn flee_for_player(shared: &SharedCombat, player_id: &str) -> Option<CombatState> {
    let mut lock = shared.lock().unwrap();
    let key = session_key_for_player(&lock, player_id)?;
    let state = lock.get_mut(&key)?;
    combat::flee_combat(state);
    Some(state.clone())
}

/// Remove a combat session by its key (after victory / defeat / flee has
/// been shown to the client).
pub fn remove_combat(shared: &SharedCombat, session_key: &str) {
    let mut lock = shared.lock().unwrap();
    lock.remove(session_key);
}

/// Remove every session matching a CONTENT event_id (any player). Admin
/// recovery hatch — `clear_combat` with an event_id needs to match
/// solo sessions whose key carries a player suffix. Returns removed keys.
pub fn clear_combat_for_event(shared: &SharedCombat, event_id: &str) -> Vec<String> {
    let mut lock = shared.lock().unwrap();
    let keys: Vec<String> = lock.iter()
        .filter(|(k, c)| k.as_str() == event_id || c.event_id == event_id)
        .map(|(k, _)| k.clone())
        .collect();
    for k in &keys { lock.remove(k); }
    keys
}
