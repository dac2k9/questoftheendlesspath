//! WASM-compatible Supabase REST client for the browser.
//! Uses reqwest (which uses fetch in WASM) with the anon key (read-only).
//! Can also write specific fields via RPC.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use crate::states::AppState;
use crate::GameSession;

pub struct SupabasePlugin;

impl Plugin for SupabasePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SupabaseConfig::default())
            .insert_resource(PolledPlayerState::default())
            .add_systems(OnEnter(AppState::InGame), start_polling)
            .add_systems(Update, receive_poll_results.run_if(in_state(AppState::InGame)));
    }
}

#[derive(Resource, Default)]
pub struct SupabaseConfig {
    pub url: String,
    pub anon_key: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PlayerRow {
    pub id: String,
    pub name: String,
    pub current_speed_kmh: f32,
    pub total_distance_m: i32,
    pub is_walking: bool,
    pub map_tile_x: Option<i32>,
    pub map_tile_y: Option<i32>,
    pub gold: i32,
    pub revealed_tiles: Option<String>,
    pub planned_route: Option<String>,
    pub route_meters_walked: Option<f64>,
}

/// Polled state from Supabase, shared between async task and Bevy systems.
#[derive(Resource, Default)]
pub struct PolledPlayerState {
    pub players: Arc<Mutex<Vec<PlayerRow>>>,
    pub last_update: f64,
}

/// Timer for polling interval.
#[derive(Resource)]
struct PollTimer(Timer);

fn start_polling(mut commands: Commands) {
    // First tick fires immediately, then every 5 seconds
    let mut timer = Timer::from_seconds(5.0, TimerMode::Repeating);
    timer.tick(std::time::Duration::from_secs(5)); // force first tick
    commands.insert_resource(PollTimer(timer));
}

fn receive_poll_results(
    time: Res<Time>,
    mut timer: ResMut<PollTimer>,
    config: Res<SupabaseConfig>,
    session: Res<GameSession>,
    state: Res<PolledPlayerState>,
) {
    timer.0.tick(time.delta());
    if !timer.0.just_finished() {
        return;
    }

    // Dev mode: poll local dev server
    let dev_url = "http://localhost:3001/players".to_string();
    let players_ref = state.players.clone();

    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        let resp = client.get(&dev_url).send().await;

        if let Ok(resp) = resp {
            if let Ok(players) = resp.json::<Vec<PlayerRow>>().await {
                if let Ok(mut lock) = players_ref.lock() {
                    *lock = players;
                }
            }
        }
    });
}

/// Write the planned route — uses dev server in dev mode.
pub fn write_planned_route(
    _config: &SupabaseConfig,
    player_id: &str,
    route_json: &str,
) {
    if player_id.is_empty() {
        return;
    }

    let url = "http://localhost:3001/set_route".to_string();

    #[derive(Serialize)]
    struct Params {
        player_id: String,
        route: String,
    }

    let body = serde_json::to_string(&Params {
        player_id: player_id.to_string(),
        route: route_json.to_string(),
    })
    .unwrap_or_default();

    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        let _ = client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await;
    });
}

/// Send browser heartbeat.
pub fn send_heartbeat(config: &SupabaseConfig, player_id: &str) {
    if config.url.is_empty() || player_id.is_empty() {
        return;
    }

    let url = format!("{}/rest/v1/rpc/browser_heartbeat", config.url);
    let key = config.anon_key.clone();

    #[derive(Serialize)]
    struct Params {
        p_player_id: String,
    }

    let body = serde_json::to_string(&Params {
        p_player_id: player_id.to_string(),
    })
    .unwrap_or_default();

    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        let _ = client
            .post(&url)
            .header("apikey", &key)
            .header("Authorization", format!("Bearer {}", &key))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await;
    });
}
