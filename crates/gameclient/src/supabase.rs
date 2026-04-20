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
            .add_systems(OnEnter(AppState::InGame), start_long_poll)
            .add_systems(Update, check_long_poll.run_if(in_state(AppState::InGame)));
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
    pub total_distance_m: f64,
    pub is_walking: bool,
    pub map_tile_x: Option<i32>,
    pub map_tile_y: Option<i32>,
    pub gold: i32,
    pub revealed_tiles: Option<String>,
    pub planned_route: Option<String>,
    pub route_meters_walked: Option<f64>,
    #[serde(default)]
    pub facing: questlib::route::Facing,
    #[serde(default)]
    pub interp_meters_target: Option<f64>,
    #[serde(default)]
    pub interp_duration_secs: Option<f32>,
    #[serde(default)]
    pub inventory: Vec<questlib::items::InventorySlot>,
    #[serde(default)]
    pub equipment: questlib::items::EquipmentLoadout,
    #[serde(default)]
    pub opened_chests: Vec<String>,
    #[serde(default)]
    pub defeated_monsters: Vec<String>,
    #[serde(default)]
    pub champion: Option<String>,
    /// Which "scene" the player is in. None/absent = overworld;
    /// Some(interior_id) = inside that interior.
    #[serde(default, deserialize_with = "deserialize_location")]
    pub location: Option<String>,
}

/// The server sends `location` as `{"kind": "overworld"}` or
/// `{"kind": "interior", "id": "..."}`. Collapse to the interior id (or None).
fn deserialize_location<'de, D>(d: D) -> Result<Option<String>, D::Error>
where D: serde::Deserializer<'de>
{
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum Loc {
        Overworld,
        Interior { id: String },
    }
    Option::<Loc>::deserialize(d).map(|opt| match opt {
        Some(Loc::Interior { id }) => Some(id),
        _ => None,
    })
}

/// Long-poll response wrapper from server.
#[derive(Deserialize)]
struct PollResponse {
    tick: u64,
    players: Vec<PlayerRow>,
}

/// Polled state from Supabase, shared between async task and Bevy systems.
#[derive(Resource, Default)]
pub struct PolledPlayerState {
    pub players: Arc<Mutex<Vec<PlayerRow>>>,
    pub last_update: f64,
}

/// Shared state for long-poll loop: in-flight flag + last-seen tick generation.
#[derive(Resource, Default, Clone)]
struct LongPollState {
    inner: Arc<Mutex<LongPollInner>>,
}

#[derive(Default)]
struct LongPollInner {
    in_flight: bool,
    last_tick: u64,
}

fn start_long_poll(mut commands: Commands) {
    commands.insert_resource(LongPollState::default());
}

/// Each frame: if no long-poll is in flight, fire one. When it returns, data
/// is written to PolledPlayerState and the flag is cleared, so next frame fires again.
fn check_long_poll(
    state: Res<PolledPlayerState>,
    poll_state: Res<LongPollState>,
) {
    let last_tick = {
        let mut lock = poll_state.inner.lock().unwrap();
        if lock.in_flight {
            return;
        }
        lock.in_flight = true;
        lock.last_tick
    };

    let players_ref = state.players.clone();
    let poll_inner = poll_state.inner.clone();

    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        let url = crate::api_url(&format!("/players/poll?after={}", last_tick));
        let resp = client.get(&url).send().await;

        if let Ok(resp) = resp {
            if let Ok(poll) = resp.json::<PollResponse>().await {
                if let Ok(mut lock) = players_ref.lock() {
                    *lock = poll.players;
                }
                if let Ok(mut lock) = poll_inner.lock() {
                    lock.last_tick = poll.tick;
                }
            }
        }

        // Clear in-flight flag — next frame will fire another request
        if let Ok(mut lock) = poll_inner.lock() {
            lock.in_flight = false;
        }
    });
}

/// Write the planned route — uses dev server in dev mode.
/// When `meters` is Some, preserves walked progress (used when extending a route).
pub fn write_planned_route(
    player_id: &str,
    route_json: &str,
    meters: Option<f64>,
) {
    if player_id.is_empty() {
        return;
    }

    let url = crate::api_url("/set_route");

    #[derive(Serialize)]
    struct Params {
        player_id: String,
        route: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        meters: Option<f64>,
    }

    let body = serde_json::to_string(&Params {
        player_id: player_id.to_string(),
        route: route_json.to_string(),
        meters,
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
