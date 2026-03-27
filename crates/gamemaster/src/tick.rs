use anyhow::Result;
use questlib::adventure::AdventureFile;
use questlib::supabase::SupabaseClient;
use questlib::types::PlayerUpdate;
use tracing::{debug, info};

use crate::boss;
use crate::events;

/// Main game tick — runs every second.
pub async fn run_tick(
    supabase: &SupabaseClient,
    game_id: &str,
    adventure: &AdventureFile,
) -> Result<()> {
    let players = supabase.read_players(game_id).await?;
    let db_events = supabase.read_events(game_id).await?;

    for player in &players {
        if !player.is_walking {
            continue;
        }

        // Gold: always earned while walking (online or offline)
        let gold_delta = calculate_gold(player.current_speed_kmh, adventure);
        if gold_delta > 0 {
            let new_gold = player.gold + gold_delta;
            supabase
                .upsert_player(
                    &player.id,
                    &PlayerUpdate {
                        gold: Some(new_gold),
                        ..Default::default()
                    },
                )
                .await?;
            debug!("{} earned {} gold (total: {})", player.name, gold_delta, new_gold);
        }

        // Map progress: only if browser is open AND not blocked at a gate
        if player.is_browser_open && !player.is_blocked {
            // km per second = speed_kmh / 3600
            let distance_delta_km = player.current_speed_kmh / 3600.0;
            if distance_delta_km > 0.0 {
                let new_position =
                    (player.map_position_km + distance_delta_km).min(adventure.adventure.total_distance_km);
                supabase
                    .upsert_player(
                        &player.id,
                        &PlayerUpdate {
                            map_position_km: Some(new_position),
                            ..Default::default()
                        },
                    )
                    .await?;
                debug!(
                    "{} advanced to {:.3} km (+{:.4} km)",
                    player.name, new_position, distance_delta_km
                );
            }
        }

        // Check browser presence timeout (30s)
        // If last_seen_at is too old, mark browser as closed
        // (In practice, we'd parse the timestamp — simplified here)
    }

    // Check event triggers
    let updated_players = supabase.read_players(game_id).await?;
    events::check_triggers(supabase, &updated_players, &db_events).await?;

    // Boss tick
    boss::tick_bosses(supabase, game_id, &updated_players).await?;

    // Check for adventure completion
    let all_finished = updated_players
        .iter()
        .all(|p| p.map_position_km >= adventure.adventure.total_distance_km);
    if all_finished && !updated_players.is_empty() {
        info!("All players reached the end! Adventure complete!");
        supabase.update_game_status(game_id, "completed").await?;
    }

    Ok(())
}

/// Calculate gold earned per tick (1 second) based on walking speed.
fn calculate_gold(speed_kmh: f32, adventure: &AdventureFile) -> i32 {
    if speed_kmh < 0.1 {
        return 0;
    }

    // Base gold: gold_per_km converted to per-second at current speed
    // gold_per_second = gold_per_km * (speed_km / 3600)
    let base_gold_per_sec =
        adventure.adventure.gold_per_km * (speed_kmh / 3600.0);

    // Speed bonus: extra gold when walking faster than 4 km/h
    let bonus = if speed_kmh > 4.0 {
        adventure.adventure.speed_bonus_gold * (speed_kmh / 3600.0)
    } else {
        0.0
    };

    // Accumulate fractionally — return at least 1 gold per tick if walking
    let total = base_gold_per_sec + bonus;
    // Since gold is integer, we'll round up to ensure visible progress
    // At 4 km/h with 100 gold/km: ~0.11 gold/sec → 1 gold every ~9 seconds
    // We multiply by 10 to make gold feel more rewarding
    (total * 10.0).ceil() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use questlib::adventure::{AdventureFile, AdventureMeta};

    fn test_adventure() -> AdventureFile {
        AdventureFile {
            adventure: AdventureMeta {
                name: "Test".to_string(),
                description: "Test".to_string(),
                total_distance_km: 75.0,
                gold_per_km: 100.0,
                speed_bonus_gold: 50.0,
            },
            zones: vec![],
        }
    }

    #[test]
    fn gold_zero_when_stationary() {
        assert_eq!(calculate_gold(0.0, &test_adventure()), 0);
    }

    #[test]
    fn gold_positive_when_walking() {
        let gold = calculate_gold(4.0, &test_adventure());
        assert!(gold > 0, "should earn gold at 4 km/h, got {gold}");
    }

    #[test]
    fn gold_higher_with_speed_bonus() {
        let slow = calculate_gold(3.0, &test_adventure());
        let fast = calculate_gold(5.0, &test_adventure());
        assert!(fast > slow, "faster should earn more: {fast} vs {slow}");
    }
}
