use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Player {
    pub id: String,
    pub game_id: String,
    pub name: String,
    pub avatar: String,
    pub current_speed_kmh: f32,
    pub total_distance_m: i32,
    pub current_incline: f32,
    pub map_position_km: f32,
    pub gold: i32,
    pub is_walking: bool,
    pub is_browser_open: bool,
    pub is_blocked: bool,
    pub blocked_at_km: Option<f32>,
    pub inventory: serde_json::Value,
    pub revealed_tiles: Option<String>,
    pub map_tile_x: Option<i32>,
    pub map_tile_y: Option<i32>,
    /// Planned route as JSON array of [x,y] pairs. Written by browser.
    pub planned_route: Option<String>,
    /// Meters walked along current route. Written by game master.
    pub route_meters_walked: Option<f64>,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlayerUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_speed_kmh: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_distance_m: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_incline: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map_position_km: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gold: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_walking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_browser_open: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_blocked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_at_km: Option<Option<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revealed_tiles: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map_tile_x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map_tile_y: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planned_route: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_meters_walked: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    pub id: String,
    pub join_code: String,
    pub adventure_name: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub game_id: String,
    pub at_km: f32,
    pub event_type: String,
    pub name: String,
    pub data: serde_json::Value,
    pub requires_all_players: bool,
    pub requires_browser: bool,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventInsert {
    pub game_id: String,
    pub at_km: f32,
    pub event_type: String,
    pub name: String,
    pub data: serde_json::Value,
    pub requires_all_players: bool,
    pub requires_browser: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BossEncounter {
    pub id: String,
    pub game_id: String,
    pub event_id: String,
    pub boss_name: String,
    pub max_hp: i32,
    pub current_hp: i32,
    pub defeated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BossInsert {
    pub game_id: String,
    pub event_id: String,
    pub boss_name: String,
    pub max_hp: i32,
    pub current_hp: i32,
}
