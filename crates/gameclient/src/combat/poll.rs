//! Polls /combat for combat state and sends /combat/action.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;

use super::CombatUiState;
use crate::terrain::tilemap::MyPlayerState;

const POLL_INTERVAL: f32 = 1.0;

pub fn poll_combat_state(
    time: Res<Time>,
    state: Res<MyPlayerState>,
    mut combat: ResMut<CombatUiState>,
) {
    combat.poll_timer += time.delta_secs();

    // Check for fetched combat state (clone Arc to avoid borrow conflict)
    let fetched_ref = combat.fetched.clone();
    if let Ok(mut lock) = fetched_ref.lock() {
        if let Some(server_state) = lock.take() {
            combat.local_player_charge = server_state.player_charge;
            combat.local_enemy_charge = server_state.enemy_charge;
            combat.state = Some(server_state);
            combat.active = true;
            combat.action_pending = false;
        }
    }

    // Check if server cleared combat
    let cleared_ref = combat.server_cleared.clone();
    if let Ok(mut cleared) = cleared_ref.lock() {
        if *cleared {
            *cleared = false;
            combat.active = false;
            combat.state = None;
            combat.local_player_charge = 0.0;
            combat.local_enemy_charge = 0.0;
        }
    }

    // Poll server at interval
    if combat.poll_timer >= POLL_INTERVAL {
        combat.poll_timer = 0.0;

        let fetched = combat.fetched.clone();
        let server_cleared = combat.server_cleared.clone();
        let was_active = combat.active;
        wasm_bindgen_futures::spawn_local(async move {
            let client = reqwest::Client::new();
            if let Ok(resp) = client.get("http://localhost:3001/combat").send().await {
                if let Ok(text) = resp.text().await {
                    if text == "null" || text.is_empty() {
                        if was_active {
                            if let Ok(mut c) = server_cleared.lock() { *c = true; }
                        }
                        return;
                    }
                    if let Ok(state) = serde_json::from_str::<questlib::combat::CombatState>(&text) {
                        if let Ok(mut lock) = fetched.lock() {
                            *lock = Some(state);
                        }
                    }
                }
            }
        });
    }

    // Advance local charge prediction between polls
    let should_predict = combat.active && !combat.action_pending
        && combat.state.as_ref().is_some_and(|cs| cs.status == questlib::combat::CombatStatus::Fighting);
    let difficulty = combat.state.as_ref().map(|cs| cs.difficulty).unwrap_or(1);
    if should_predict {
        let dt = time.delta_secs();
        combat.local_player_charge += questlib::combat::player_charge_rate(state.speed_kmh) * dt;
        combat.local_player_charge = combat.local_player_charge.min(1.0);
        combat.local_enemy_charge += questlib::combat::enemy_charge_rate(difficulty) * dt;
        combat.local_enemy_charge = combat.local_enemy_charge.min(1.0);
    }
}

/// Send flee request to the server.
pub fn send_flee(fetched: Arc<Mutex<Option<questlib::combat::CombatState>>>) {
    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        if let Ok(resp) = client.post("http://localhost:3001/combat/flee")
            .send()
            .await
        {
            if let Ok(text) = resp.text().await {
                if let Ok(state) = serde_json::from_str::<questlib::combat::CombatState>(&text) {
                    if let Ok(mut lock) = fetched.lock() {
                        *lock = Some(state);
                    }
                }
            }
        }
    });
}
