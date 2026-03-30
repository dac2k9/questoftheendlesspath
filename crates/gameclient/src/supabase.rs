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
    commands.insert_resource(PollTimer(Timer::from_seconds(0.5, TimerMode::Repeating)));
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

    if config.url.is_empty() || session.game_id.is_empty() {
        return;
    }

    // Spawn async fetch
    let url = format!(
        "{}/rest/v1/players?game_id=eq.{}&select=id,name,current_speed_kmh,total_distance_m,is_walking,map_tile_x,map_tile_y,gold,revealed_tiles,planned_route,route_meters_walked",
        config.url, session.game_id
    );
    let key = config.anon_key.clone();
    let players_ref = state.players.clone();

    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        let resp = client
            .get(&url)
            .header("apikey", &key)
            .header("Authorization", format!("Bearer {}", &key))
            .send()
            .await;

        if let Ok(resp) = resp {
            if let Ok(players) = resp.json::<Vec<PlayerRow>>().await {
                if let Ok(mut lock) = players_ref.lock() {
                    *lock = players;
                }
            }
        }
    });
}

/// Write the planned route to Supabase for this player.
pub fn write_planned_route(
    config: &SupabaseConfig,
    player_id: &str,
    route_json: &str,
) {
    if config.url.is_empty() || player_id.is_empty() {
        return;
    }

    let url = format!(
        "{}/rest/v1/players?id=eq.{}",
        config.url, player_id
    );
    let key = config.anon_key.clone();

    #[derive(Serialize)]
    struct RouteUpdate {
        planned_route: String,
        route_meters_walked: f64,
    }

    let body = RouteUpdate {
        planned_route: route_json.to_string(),
        route_meters_walked: 0.0,
    };

    let body_str = serde_json::to_string(&body).unwrap_or_default();

    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        let _ = client
            .patch(&url)
            .header("apikey", &key)
            .header("Authorization", format!("Bearer {}", &key))
            .header("Content-Type", "application/json")
            .body(body_str)
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
