//! Server-side combat manager.
//!
//! Stores active combat states and ticks them forward each game loop iteration.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use questlib::combat::{self, CombatState};
use questlib::events::kind::EventKind;

pub type SharedCombat = Arc<Mutex<HashMap<String, CombatState>>>;

/// Initialize combat for an event. Adds to the shared combat map.
pub fn start_combat(
    shared: &SharedCombat,
    event_id: &str,
    kind: &EventKind,
    total_distance_m: u64,
    equipment_bonuses: (i32, i32, i32),
    player_id: &str,
) {
    let state = combat::init_combat(event_id, kind, total_distance_m, equipment_bonuses, player_id);
    let mut lock = shared.lock().unwrap();
    lock.insert(event_id.to_string(), state);
}

/// Tick all active combats using per-player speeds. `player_speeds` is
/// (player_id, speed_kmh, incline_pct).
pub fn tick_all(
    shared: &SharedCombat,
    player_speeds: &[(String, f32, f32)],
    delta_secs: f32,
) -> (Vec<String>, Vec<String>) {
    let mut lock = shared.lock().unwrap();
    let mut victories = Vec::new();
    let mut retreats = Vec::new();

    for (event_id, state) in lock.iter_mut() {
        let (speed, incline) = player_speeds.iter()
            .find(|(pid, _, _)| pid == &state.player_id)
            .map(|(_, s, i)| (*s, *i))
            .unwrap_or((0.0, 0.0));
        combat::tick_combat(state, speed, incline, delta_secs);
        match state.status {
            combat::CombatStatus::Victory => victories.push(event_id.clone()),
            combat::CombatStatus::Defeat | combat::CombatStatus::Fled => retreats.push(event_id.clone()),
            _ => {}
        }
    }

    (victories, retreats)
}

/// Get the active combat for a specific player, if any.
pub fn get_combat_for_player(shared: &SharedCombat, player_id: &str) -> Option<CombatState> {
    let lock = shared.lock().unwrap();
    lock.values().find(|c| c.player_id == player_id).cloned()
}

/// Check if this player is currently in combat.
pub fn player_in_combat(shared: &SharedCombat, player_id: &str) -> bool {
    let lock = shared.lock().unwrap();
    lock.values().any(|c| c.player_id == player_id)
}

/// Player runs away from combat.
pub fn flee(shared: &SharedCombat, event_id: &str) -> Option<CombatState> {
    let mut lock = shared.lock().unwrap();
    if let Some(state) = lock.get_mut(event_id) {
        combat::flee_combat(state);
        Some(state.clone())
    } else {
        None
    }
}

/// Remove a combat (after victory, defeat, or flee has been shown to client).
pub fn remove_combat(shared: &SharedCombat, event_id: &str) {
    let mut lock = shared.lock().unwrap();
    lock.remove(event_id);
}
