mod boss;
mod events;
mod tick;

use std::time::Duration;

use anyhow::{Context, Result};
use questlib::adventure::AdventureFile;
use questlib::supabase::SupabaseClient;
use questlib::types::EventInsert;
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
    let adventure_path =
        std::env::var("ADVENTURE_PATH").unwrap_or_else(|_| "adventures/dragons_path.yaml".into());

    info!("Loading adventure from {adventure_path}");
    let adventure = AdventureFile::load(std::path::Path::new(&adventure_path))?;
    info!(
        "Adventure: {} ({} km, {} zones, {} events)",
        adventure.adventure.name,
        adventure.adventure.total_distance_km,
        adventure.zones.len(),
        adventure.all_events().len(),
    );

    // Seed events into database
    seed_events(&supabase, &game_id, &adventure).await?;

    info!("Game Master running. Tick interval: 1s");
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    loop {
        interval.tick().await;
        if let Err(e) = tick::run_tick(&supabase, &game_id, &adventure).await {
            error!("Tick error: {e:#}");
        }
    }
}

async fn seed_events(
    supabase: &SupabaseClient,
    game_id: &str,
    adventure: &AdventureFile,
) -> Result<()> {
    // Check if events already exist
    let existing = supabase.read_events(game_id).await?;
    if !existing.is_empty() {
        info!("Events already seeded ({} events), skipping", existing.len());
        return Ok(());
    }

    let inserts: Vec<EventInsert> = adventure
        .all_events()
        .into_iter()
        .map(|e| EventInsert {
            game_id: game_id.to_string(),
            at_km: e.at_km,
            event_type: e.event_type.clone(),
            name: e.name.clone(),
            data: serde_json::to_value(e).unwrap_or_default(),
            requires_all_players: e.requires_all_players,
            requires_browser: e.requires_browser,
        })
        .collect();

    info!("Seeding {} events", inserts.len());
    supabase.insert_events(&inserts).await?;
    info!("Events seeded successfully");
    Ok(())
}
