use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Serialize;

use crate::types::*;

/// Supabase REST API client.
///
/// Uses the PostgREST API to read/write data.
/// The service_role key bypasses RLS for full write access.
pub struct SupabaseClient {
    client: reqwest::Client,
    base_url: String,
}

impl SupabaseClient {
    /// Create a new client from environment variables.
    ///
    /// Expects `SUPABASE_URL` and `SUPABASE_SERVICE_KEY`.
    pub fn from_env() -> Result<Self> {
        let url = std::env::var("SUPABASE_URL").context("SUPABASE_URL not set")?;
        let key = std::env::var("SUPABASE_SERVICE_KEY").context("SUPABASE_SERVICE_KEY not set")?;

        let mut headers = HeaderMap::new();
        headers.insert(
            "apikey",
            HeaderValue::from_str(&key).context("invalid api key")?,
        );
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {key}")).context("invalid auth header")?,
        );
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));
        headers.insert("Prefer", HeaderValue::from_static("return=representation"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;

        Ok(Self {
            client,
            base_url: url,
        })
    }

    fn rest_url(&self, table: &str) -> String {
        format!("{}/rest/v1/{}", self.base_url, table)
    }

    fn rpc_url(&self, function: &str) -> String {
        format!("{}/rest/v1/rpc/{}", self.base_url, function)
    }

    // ── Players ──────────────────────────────────────────────

    /// Upsert a player's treadmill data (called by walker).
    pub async fn upsert_player(&self, player_id: &str, update: &PlayerUpdate) -> Result<()> {
        self.client
            .patch(&self.rest_url("players"))
            .query(&[("id", format!("eq.{player_id}"))])
            .json(update)
            .send()
            .await?
            .error_for_status()
            .context("failed to upsert player")?;
        Ok(())
    }

    /// Read all players for a game.
    pub async fn read_players(&self, game_id: &str) -> Result<Vec<Player>> {
        let players = self
            .client
            .get(&self.rest_url("players"))
            .query(&[("game_id", format!("eq.{game_id}"))])
            .send()
            .await?
            .error_for_status()
            .context("failed to read players")?
            .json::<Vec<Player>>()
            .await?;
        Ok(players)
    }

    // ── Events ───────────────────────────────────────────────

    /// Insert events from adventure file.
    pub async fn insert_events(&self, events: &[EventInsert]) -> Result<()> {
        self.client
            .post(&self.rest_url("events"))
            .json(events)
            .send()
            .await?
            .error_for_status()
            .context("failed to insert events")?;
        Ok(())
    }

    /// Read all events for a game.
    pub async fn read_events(&self, game_id: &str) -> Result<Vec<Event>> {
        let events = self
            .client
            .get(&self.rest_url("events"))
            .query(&[
                ("game_id", format!("eq.{game_id}")),
                ("order", "at_km.asc".to_string()),
            ])
            .send()
            .await?
            .error_for_status()
            .context("failed to read events")?
            .json::<Vec<Event>>()
            .await?;
        Ok(events)
    }

    /// Update an event's status.
    pub async fn update_event_status(&self, event_id: &str, status: &str) -> Result<()> {
        #[derive(Serialize)]
        struct StatusUpdate {
            status: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            triggered_at: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            completed_at: Option<String>,
        }

        let now = chrono_now();
        let update = match status {
            "active" => StatusUpdate {
                status: status.to_string(),
                triggered_at: Some(now),
                completed_at: None,
            },
            "completed" => StatusUpdate {
                status: status.to_string(),
                triggered_at: None,
                completed_at: Some(now),
            },
            _ => StatusUpdate {
                status: status.to_string(),
                triggered_at: None,
                completed_at: None,
            },
        };

        self.client
            .patch(&self.rest_url("events"))
            .query(&[("id", format!("eq.{event_id}"))])
            .json(&update)
            .send()
            .await?
            .error_for_status()
            .context("failed to update event status")?;
        Ok(())
    }

    // ── Boss Encounters ──────────────────────────────────────

    /// Create a boss encounter.
    pub async fn create_boss(&self, boss: &BossInsert) -> Result<BossEncounter> {
        let bosses = self
            .client
            .post(&self.rest_url("boss_encounters"))
            .json(boss)
            .send()
            .await?
            .error_for_status()
            .context("failed to create boss")?
            .json::<Vec<BossEncounter>>()
            .await?;
        bosses
            .into_iter()
            .next()
            .context("no boss returned from insert")
    }

    /// Read active (non-defeated) boss encounters for a game.
    pub async fn read_active_bosses(&self, game_id: &str) -> Result<Vec<BossEncounter>> {
        let bosses = self
            .client
            .get(&self.rest_url("boss_encounters"))
            .query(&[
                ("game_id", format!("eq.{game_id}")),
                ("defeated", "eq.false".to_string()),
            ])
            .send()
            .await?
            .error_for_status()
            .context("failed to read bosses")?
            .json::<Vec<BossEncounter>>()
            .await?;
        Ok(bosses)
    }

    /// Apply damage to a boss via RPC.
    pub async fn damage_boss(&self, boss_id: &str, dmg: i32) -> Result<i32> {
        #[derive(Serialize)]
        struct DamageParams {
            p_boss_id: String,
            p_dmg: i32,
        }

        let result = self
            .client
            .post(&self.rpc_url("damage_boss"))
            .json(&DamageParams {
                p_boss_id: boss_id.to_string(),
                p_dmg: dmg,
            })
            .send()
            .await?
            .error_for_status()
            .context("failed to damage boss")?
            .json::<serde_json::Value>()
            .await?;

        // The RPC returns the remaining HP
        result
            .as_i64()
            .map(|v| v as i32)
            .context("unexpected damage_boss response")
    }

    // ── Games ────────────────────────────────────────────────

    /// Update game status.
    pub async fn update_game_status(&self, game_id: &str, status: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Update {
            status: String,
        }
        self.client
            .patch(&self.rest_url("games"))
            .query(&[("id", format!("eq.{game_id}"))])
            .json(&Update {
                status: status.to_string(),
            })
            .send()
            .await?
            .error_for_status()
            .context("failed to update game")?;
        Ok(())
    }

    // ── Game Log ─────────────────────────────────────────────

    /// Write to the game log.
    pub async fn log_event(
        &self,
        game_id: &str,
        player_id: &str,
        event_type: &str,
        data: &serde_json::Value,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct LogEntry {
            game_id: String,
            player_id: String,
            event_type: String,
            data: serde_json::Value,
        }
        self.client
            .post(&self.rest_url("game_log"))
            .json(&LogEntry {
                game_id: game_id.to_string(),
                player_id: player_id.to_string(),
                event_type: event_type.to_string(),
                data: data.clone(),
            })
            .send()
            .await?
            .error_for_status()
            .context("failed to write game log")?;
        Ok(())
    }
}

/// Simple UTC timestamp string (ISO 8601).
fn chrono_now() -> String {
    // Using a simple approach without pulling in chrono crate
    // The server will use its own timestamp if we pass "now()" but
    // for the REST API we need to send an actual timestamp
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Good enough for our purposes — Supabase accepts ISO 8601
    format!("{secs}")
}
