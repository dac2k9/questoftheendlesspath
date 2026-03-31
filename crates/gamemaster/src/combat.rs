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

/// Tick all active combats. Returns event IDs of combats that ended in Victory.
/// Defeat and Fled combats are removed but do NOT complete the event.
pub fn tick_all(
    shared: &SharedCombat,
    player_speed_kmh: f32,
    player_incline: f32,
    delta_secs: f32,
) -> (Vec<String>, Vec<String>) {
    let mut lock = shared.lock().unwrap();
    let mut victories = Vec::new();
    let mut retreats = Vec::new();

    for (event_id, state) in lock.iter_mut() {
        combat::tick_combat(state, player_speed_kmh, player_incline, delta_secs);
        match state.status {
            combat::CombatStatus::Victory => victories.push(event_id.clone()),
            combat::CombatStatus::Defeat | combat::CombatStatus::Fled => retreats.push(event_id.clone()),
            _ => {}
        }
    }

    (victories, retreats)
}

/// Get the first active combat state.
pub fn get_active_combat(shared: &SharedCombat) -> Option<CombatState> {
    let lock = shared.lock().unwrap();
    lock.values().next().cloned()
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
