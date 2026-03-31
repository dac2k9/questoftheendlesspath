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
) {
    let state = combat::init_combat(event_id, kind, total_distance_m);
    let mut lock = shared.lock().unwrap();
    lock.insert(event_id.to_string(), state);
}

/// Tick all active combats. Returns event IDs of combats that just ended in Victory.
pub fn tick_all(
    shared: &SharedCombat,
    player_speed_kmh: f32,
    delta_secs: f32,
) -> Vec<String> {
    let mut lock = shared.lock().unwrap();
    let mut victories = Vec::new();

    for (event_id, state) in lock.iter_mut() {
        let ended = combat::tick_combat(state, player_speed_kmh, delta_secs);
        if ended && state.status == combat::CombatStatus::Victory {
            victories.push(event_id.clone());
        }
        // Auto-retry on defeat
        if state.status == combat::CombatStatus::Defeat {
            combat::retry_combat(state);
        }
    }

    victories
}

/// Get the first active combat state (for single-player, there's only one at a time).
pub fn get_active_combat(shared: &SharedCombat) -> Option<CombatState> {
    let lock = shared.lock().unwrap();
    lock.values()
        .find(|s| s.status != combat::CombatStatus::Victory)
        .cloned()
}

/// Apply a player action to the combat matching the given event_id.
pub fn apply_action(
    shared: &SharedCombat,
    event_id: &str,
    action: &str,
    incline_pct: f32,
) -> Option<CombatState> {
    let mut lock = shared.lock().unwrap();
    if let Some(state) = lock.get_mut(event_id) {
        combat::apply_player_action(state, action, incline_pct);
        Some(state.clone())
    } else {
        None
    }
}

/// Remove a completed combat.
pub fn remove_combat(shared: &SharedCombat, event_id: &str) {
    let mut lock = shared.lock().unwrap();
    lock.remove(event_id);
}
