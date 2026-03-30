mod tick;

use std::time::Duration;

use anyhow::{Context, Result};
use questlib::mapgen::WorldMap;
use questlib::supabase::SupabaseClient;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "gamemaster=info".parse().expect("valid filter")),
        )
        .init();

    dotenvy::dotenv().ok();

    let supabase = SupabaseClient::from_env()?;
    let game_id = std::env::var("GAME_ID").context("GAME_ID not set")?;
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

    info!("Game Master running. Tick interval: 1s");
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    // Track per-player state that persists between ticks
    let mut player_fogs: std::collections::HashMap<String, questlib::fog::FogBitfield> =
        std::collections::HashMap::new();
    let mut player_last_distance: std::collections::HashMap<String, i32> =
        std::collections::HashMap::new();

    loop {
        interval.tick().await;
        if let Err(e) = tick::run_tick(
            &supabase,
            &game_id,
            &world,
            &mut player_fogs,
            &mut player_last_distance,
        )
        .await
        {
            error!("Tick error: {e:#}");
        }
    }
}
