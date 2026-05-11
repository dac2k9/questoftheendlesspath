use serde::{Deserialize, Serialize};

/// Results of completing an event. Applied by the Game Master.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum EventOutcome {
    /// Award gold to the player.
    Gold { amount: i32 },
    /// Grant an item.
    Item { name: String },
    /// Reveal fog of war around a point.
    RevealFog { x: usize, y: usize, radius: usize },
    /// Spawn/unlock new events (by id).
    SpawnEvents { event_ids: Vec<String> },
    /// Show a notification message in the browser.
    Notification { text: String },
    /// Modify tile movement cost temporarily.
    TileCostModifier { multiplier: f32, duration_tiles: u32 },
    /// Reveal a shop's location on the player's map — NPCs telling you
    /// about a merchant they've heard of. Target is the shop event's id.
    /// No-op if the player has already discovered that shop.
    RevealShop { shop_event_id: String },
    /// Grant an adventure-scoped boon to the player. The boon's effects
    /// apply WHILE the player is in this adventure; they're stored on
    /// `DevPlayerState.adventure_boons[adventure_id]` so switching to
    /// a different adventure deactivates them (and switching back
    /// re-activates). Used by chaos boss drops to layer small,
    /// per-arc power-ups on top of the permanent cross-adventure
    /// boon catalog.
    AdventureBoon { boon_id: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_gold() {
        let o = EventOutcome::Gold { amount: 100 };
        let json = serde_json::to_string(&o).unwrap();
        assert!(json.contains("\"effect\":\"gold\""));
        let roundtrip: EventOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(o, roundtrip);
    }

    #[test]
    fn serialize_spawn_events() {
        let o = EventOutcome::SpawnEvents {
            event_ids: vec!["quest_2".into(), "boss_1".into()],
        };
        let json = serde_json::to_string(&o).unwrap();
        let roundtrip: EventOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(o, roundtrip);
    }

    #[test]
    fn deserialize_from_llm_json() {
        let json = r#"{"effect": "notification", "text": "You found a gem!"}"#;
        let o: EventOutcome = serde_json::from_str(json).unwrap();
        assert!(matches!(o, EventOutcome::Notification { .. }));
    }
}
