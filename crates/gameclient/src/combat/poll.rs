//! Polls /combat for combat state and sends /combat/action.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;

use super::CombatUiState;
use crate::terrain::tilemap::MyPlayerState;

const POLL_INTERVAL: f32 = 1.0;

pub fn poll_combat_state(
    time: Res<Time>,
    state: Res<MyPlayerState>,
    session: Res<crate::GameSession>,
    mut combat: ResMut<CombatUiState>,
) {
    combat.poll_timer += time.delta_secs();

    // Check for fetched combat state (clone Arc to avoid borrow conflict)
    let fetched_ref = combat.fetched.clone();
    if let Ok(mut lock) = fetched_ref.lock() {
        if let Some(server_state) = lock.take() {
            // Re-anchor the dead-reckoned bars to this server sample.
            // The displayed charge is recomputed every frame as
            // `server_charge + rate * charge_age`, so anchoring here +
            // resetting charge_age to 0 means the bar can't drift away
            // from the server's truth over a long fight. Crucially, the
            // forward prediction (below) compensates for the ~1 s the
            // server sample lags real time, so re-anchoring does NOT
            // snap the bar backward the way a naive `local = server`
            // did — the value we display the instant after a poll is
            // very close to what we displayed the instant before it,
            // because both advance at the same rate.
            combat.server_player_charge = server_state.player_charge;
            combat.server_enemy_charge = server_state.enemy_charge;
            combat.charge_age = 0.0;
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
            combat.server_player_charge = 0.0;
            combat.server_enemy_charge = 0.0;
            combat.charge_age = 0.0;
        }
    }

    // Poll server at interval
    if combat.poll_timer >= POLL_INTERVAL {
        combat.poll_timer = 0.0;

        let fetched = combat.fetched.clone();
        let server_cleared = combat.server_cleared.clone();
        let was_active = combat.active;
        let player_id = session.player_id.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let client = reqwest::Client::new();
            let url = crate::api_url(&format!("/combat?player_id={}", player_id));
            if let Ok(resp) = client.get(&url).send().await {
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

    // Dead-reckon the displayed bars from the last server sample.
    // local = (server_charge + rate * charge_age).min(1.0). When the
    // fight is paused/over we freeze the age so the bars hold steady.
    let fighting = combat.active && !combat.action_pending
        && combat.state.as_ref().is_some_and(|cs| cs.status == questlib::combat::CombatStatus::Fighting);
    let difficulty = combat.state.as_ref().map(|cs| cs.difficulty).unwrap_or(1);
    if fighting {
        combat.charge_age += time.delta_secs();
        let p_rate = questlib::combat::player_charge_rate(state.speed_kmh);
        let e_rate = questlib::combat::enemy_charge_rate(difficulty);
        combat.local_player_charge = (combat.server_player_charge + p_rate * combat.charge_age).min(1.0);
        combat.local_enemy_charge = (combat.server_enemy_charge + e_rate * combat.charge_age).min(1.0);
    }
}

/// Send flee request to the server.
pub fn send_flee(fetched: Arc<Mutex<Option<questlib::combat::CombatState>>>, player_id: String) {
    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        if let Ok(resp) = client.post(&crate::api_url("/combat/flee"))
            .json(&serde_json::json!({"player_id": player_id}))
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
