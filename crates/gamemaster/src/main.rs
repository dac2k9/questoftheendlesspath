mod devserver;
mod tick;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use questlib::mapgen::WorldMap;
use tracing::{error, info};

use devserver::{DevPlayerState, SharedState};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "gamemaster=info".parse().expect("valid filter")),
        )
        .init();

    dotenvy::dotenv().ok();

    let seed: u64 = std::env::var("MAP_SEED")
        .unwrap_or_else(|_| "42".to_string())
        .parse()
        .unwrap_or(42);

    info!("Generating world map from seed {seed}");
    let world = WorldMap::generate(seed);
    info!(
        "World: {}x{} tiles, {} POIs, {} roads",
        world.width, world.height, world.pois.len(), world.roads.len()
    );

    // Initialize shared player state
    let state: SharedState = Arc::new(Mutex::new(HashMap::new()));

    // Add default players
    {
        let mut lock = state.lock().unwrap();
        let game_id = std::env::var("GAME_ID").unwrap_or_default();

        // Find starting position
        let start = world.pois.iter()
            .find(|p| matches!(p.poi_type, questlib::mapgen::PoiType::Town | questlib::mapgen::PoiType::Village))
            .map(|p| (p.x as i32, p.y as i32))
            .unwrap_or((50, 40));

        lock.insert(
            "a0000000-0000-0000-0000-000000000001".to_string(),
            DevPlayerState {
                id: "a0000000-0000-0000-0000-000000000001".to_string(),
                name: "Dac".to_string(),
                map_tile_x: start.0,
                map_tile_y: start.1,
                ..Default::default()
            },
        );
        lock.insert(
            "b0000000-0000-0000-0000-000000000002".to_string(),
            DevPlayerState {
                id: "b0000000-0000-0000-0000-000000000002".to_string(),
                name: "Apanloco".to_string(),
                map_tile_x: start.0,
                map_tile_y: start.1,
                ..Default::default()
            },
        );
    }

    // Start dev HTTP server
    let server_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = devserver::start_dev_server(server_state).await {
            error!("Dev server error: {e}");
        }
    });

    // Track per-player fog
    let mut player_fogs: HashMap<String, questlib::fog::FogBitfield> = HashMap::new();
    let mut player_last_distance: HashMap<String, i32> = HashMap::new();

    info!("Game Master running (dev mode). Tick interval: 3s. Dev server on :3001");
    let mut interval = tokio::time::interval(Duration::from_secs(3));

    loop {
        interval.tick().await;
        if let Err(e) = tick::run_tick_dev(
            &state,
            &world,
            &mut player_fogs,
            &mut player_last_distance,
        ) {
            error!("Tick error: {e:#}");
        }
    }
}
