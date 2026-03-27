use anyhow::Result;
use questlib::supabase::SupabaseClient;
use questlib::types::{BossInsert, Event, Player, PlayerUpdate};
use tracing::info;

/// Tick all active boss encounters — apply damage based on walking speed.
pub async fn tick_bosses(
    supabase: &SupabaseClient,
    game_id: &str,
    players: &[Player],
) -> Result<()> {
    // Check for events that just became active and need a boss spawned
    let events = supabase.read_events(game_id).await?;
    for event in events.iter().filter(|e| e.status == "active" && e.event_type == "boss") {
        let existing_bosses = supabase.read_active_bosses(game_id).await?;
        let already_spawned = existing_bosses.iter().any(|b| b.event_id == event.id);

        if !already_spawned {
            let hp = event
                .data
                .get("hp")
                .and_then(|h| h.as_i64())
                .unwrap_or(1000) as i32;

            let boss_name = event
                .data
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or(&event.name)
                .to_string();

            info!("Spawning boss: {} (HP: {})", boss_name, hp);
            supabase
                .create_boss(&BossInsert {
                    game_id: game_id.to_string(),
                    event_id: event.id.clone(),
                    boss_name: boss_name.clone(),
                    max_hp: hp,
                    current_hp: hp,
                })
                .await?;
        }
    }

    // Apply damage to active bosses
    let bosses = supabase.read_active_bosses(game_id).await?;
    for boss in &bosses {
        // DPS = sum of walking speeds of all players who have browser open
        let dps: f32 = players
            .iter()
            .filter(|p| p.is_walking && p.is_browser_open)
            .map(|p| p.current_speed_kmh)
            .sum();

        let damage = dps.round() as i32;
        if damage <= 0 {
            continue;
        }

        let remaining_hp = supabase.damage_boss(&boss.id, damage).await?;
        info!(
            "Boss {} took {} damage (DPS from walking). HP: {}/{}",
            boss.boss_name, damage, remaining_hp, boss.max_hp
        );

        if remaining_hp <= 0 {
            info!("Boss {} defeated!", boss.boss_name);
            on_boss_defeated(supabase, game_id, boss, &events, players).await?;
        }
    }

    Ok(())
}

/// Handle boss defeat: distribute rewards, unblock players, complete event.
async fn on_boss_defeated(
    supabase: &SupabaseClient,
    game_id: &str,
    boss: &questlib::types::BossEncounter,
    events: &[Event],
    players: &[Player],
) -> Result<()> {
    // Find the event for this boss
    let event = events.iter().find(|e| e.id == boss.event_id);

    // Distribute rewards to all players
    if let Some(event) = event {
        if let Some(reward) = event.data.get("reward") {
            let gold = reward.get("gold").and_then(|g| g.as_i64()).unwrap_or(0) as i32;

            for player in players {
                if gold > 0 {
                    let new_gold = player.gold + gold;
                    supabase
                        .upsert_player(
                            &player.id,
                            &PlayerUpdate {
                                gold: Some(new_gold),
                                ..Default::default()
                            },
                        )
                        .await?;
                    info!("{} received {} gold from boss defeat", player.name, gold);
                }
            }
        }

        // Mark event completed
        supabase.update_event_status(&event.id, "completed").await?;
    }

    // Unblock all players
    for player in players.iter().filter(|p| p.is_blocked) {
        supabase
            .upsert_player(
                &player.id,
                &PlayerUpdate {
                    is_blocked: Some(false),
                    blocked_at_km: Some(None),
                    ..Default::default()
                },
            )
            .await?;
        info!("{} unblocked after boss defeat", player.name);
    }

    Ok(())
}
