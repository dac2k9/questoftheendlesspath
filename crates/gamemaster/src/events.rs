use anyhow::Result;
use questlib::supabase::SupabaseClient;
use questlib::types::{Event, Player, PlayerUpdate};
use tracing::info;

/// Check if any pending events should be triggered based on player positions.
pub async fn check_triggers(
    supabase: &SupabaseClient,
    players: &[Player],
    events: &[Event],
) -> Result<()> {
    for event in events.iter().filter(|e| e.status == "pending") {
        for player in players {
            if player.map_position_km < event.at_km {
                continue;
            }

            match event.event_type.as_str() {
                // Solo events that don't require browser — auto-complete
                "npc" | "treasure" | "story" if !event.requires_browser => {
                    complete_solo_event(supabase, event, player).await?;
                }

                // Boss: requires all players at location + browser open
                "boss" if event.requires_all_players => {
                    let all_at_gate = players
                        .iter()
                        .all(|p| p.map_position_km >= event.at_km && p.is_browser_open);

                    if all_at_gate {
                        info!("All players at boss gate: {}", event.name);
                        // Boss spawning is handled by boss::tick_bosses via the event status
                        supabase.update_event_status(&event.id, "active").await?;
                    } else {
                        // Block the player who arrived first
                        if !player.is_blocked {
                            info!(
                                "{} blocked at {} km, waiting for others at: {}",
                                player.name, event.at_km, event.name
                            );
                            supabase
                                .upsert_player(
                                    &player.id,
                                    &PlayerUpdate {
                                        is_blocked: Some(true),
                                        blocked_at_km: Some(Some(event.at_km)),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                        }
                    }
                }

                // Non-boss events requiring all players
                _ if event.requires_all_players => {
                    let all_here = players.iter().all(|p| p.map_position_km >= event.at_km);
                    if all_here {
                        supabase.update_event_status(&event.id, "active").await?;
                    } else if !player.is_blocked {
                        supabase
                            .upsert_player(
                                &player.id,
                                &PlayerUpdate {
                                    is_blocked: Some(true),
                                    blocked_at_km: Some(Some(event.at_km)),
                                    ..Default::default()
                                },
                            )
                            .await?;
                    }
                }

                // Browser-required events (shop, interactive NPC)
                _ if event.requires_browser && player.is_browser_open => {
                    info!("Activating browser event: {} for {}", event.name, player.name);
                    supabase.update_event_status(&event.id, "active").await?;
                }

                // Hazard — apply speed modifier
                "hazard" => {
                    info!("Player {} entered hazard: {}", player.name, event.name);
                    supabase.update_event_status(&event.id, "active").await?;
                    // Hazard effects are applied in tick.rs based on active hazard events
                }

                _ => {}
            }

            // Only process the first player that triggers — avoid double-triggering
            break;
        }
    }

    Ok(())
}

/// Complete a solo event: grant rewards, mark completed.
async fn complete_solo_event(
    supabase: &SupabaseClient,
    event: &Event,
    player: &Player,
) -> Result<()> {
    info!(
        "{} triggered event: {} ({})",
        player.name, event.name, event.event_type
    );

    // Extract reward from event data
    if let Some(reward) = event.data.get("reward") {
        let gold = reward.get("gold").and_then(|g| g.as_i64()).unwrap_or(0) as i32;
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
            info!("{} received {} gold", player.name, gold);
        }

        if let Some(item) = reward.get("item").and_then(|i| i.as_str()) {
            // Add item to inventory
            let mut inventory = player.inventory.clone();
            if let Some(arr) = inventory.as_array_mut() {
                arr.push(serde_json::Value::String(item.to_string()));
            }
            info!("{} received item: {}", player.name, item);
        }
    }

    supabase.update_event_status(&event.id, "completed").await?;
    Ok(())
}
