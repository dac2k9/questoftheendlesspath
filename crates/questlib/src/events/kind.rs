use serde::{Deserialize, Serialize};

/// Typed event payloads. Each variant carries only the fields it needs.
/// The `#[serde(tag = "type")]` means JSON uses `"type": "npc_dialogue"` etc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    NpcDialogue {
        speaker: String,
        #[serde(default)]
        portrait: Option<String>,
        lines: Vec<String>,
        #[serde(default)]
        choices: Vec<DialogueChoice>,
    },
    Treasure {
        description: String,
    },
    RandomEncounter {
        enemy_name: String,
        description: String,
        #[serde(default)]
        difficulty: u32,
    },
    Quest {
        quest_id: String,
        title: String,
        description: String,
        #[serde(default)]
        objectives: Vec<QuestObjective>,
    },
    Shop {
        merchant_name: String,
        #[serde(default)]
        items: Vec<ShopItem>,
    },
    /// A forge NPC that upgrades equipped items for gold. No per-event
    /// config beyond the NPC's name — pricing, slots, and the cap are all
    /// server-enforced constants in the `/forge_upgrade` handler.
    Forge {
        npc_name: String,
    },
    Boss {
        boss_name: String,
        max_hp: i32,
        #[serde(default)]
        portrait: Option<String>,
        #[serde(default)]
        dialogue_intro: Vec<String>,
        #[serde(default)]
        dialogue_defeat: Vec<String>,
        /// Scales the boss's HP and attack with the player's level so
        /// late-game story bosses still feel challenging. Base formula:
        /// `hp += 20 × (lvl−1)`, `atk += 2 × (lvl−1)`. Defaults to false —
        /// existing bosses keep fixed stats unless explicitly marked.
        #[serde(default)]
        scales_with_player: bool,
    },
    StoryBeat {
        lines: Vec<String>,
    },
    EnvironmentalEffect {
        effect: EnvironmentalEffectType,
        value: f32,
        #[serde(default)]
        duration_tiles: Option<u32>,
    },
    /// Arriving at this event teleports the player into an interior space.
    /// The client swaps its tilemap when it notices location changed.
    CaveEntrance {
        interior_id: String,
        /// Tile to drop the player on inside the interior.
        spawn_x: usize,
        spawn_y: usize,
        /// Optional one-liner flavor text shown in a notification.
        #[serde(default)]
        flavor: String,
        /// If set, the player must have this item in inventory to enter.
        /// One is consumed on successful entry. Typical: "torch".
        #[serde(default)]
        consume_on_entry: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DialogueChoice {
    pub label: String,
    #[serde(default)]
    pub outcome_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuestObjective {
    pub description: String,
    pub target: QuestTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuestTarget {
    ReachLocation { poi_id: usize },
    WalkDistance { meters: u32 },
    DefeatBoss { event_id: String },
    CollectItem { item: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShopItem {
    pub name: String,
    pub cost: i32,
    #[serde(default)]
    pub effect: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentalEffectType {
    SpeedMultiplier,
    TileCostMultiplier,
    FogRadius,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_npc_dialogue() {
        let kind = EventKind::NpcDialogue {
            speaker: "Sage".into(),
            portrait: None,
            lines: vec!["Hello!".into(), "Good luck.".into()],
            choices: vec![],
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"npc_dialogue\""));
        let roundtrip: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, roundtrip);
    }

    #[test]
    fn serialize_boss() {
        let kind = EventKind::Boss {
            boss_name: "Troll".into(),
            max_hp: 800,
            portrait: None,
            dialogue_intro: vec!["NONE SHALL PASS!".into()],
            dialogue_defeat: vec!["Ugh...".into()],
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"boss\""));
        let roundtrip: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, roundtrip);
    }

    #[test]
    fn serialize_quest_with_objectives() {
        let kind = EventKind::Quest {
            quest_id: "q1".into(),
            title: "Find the Gem".into(),
            description: "Locate the gem.".into(),
            objectives: vec![QuestObjective {
                description: "Go to the cave".into(),
                target: QuestTarget::ReachLocation { poi_id: 5 },
            }],
        };
        let json = serde_json::to_string(&kind).unwrap();
        let roundtrip: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, roundtrip);
    }

    #[test]
    fn deserialize_from_llm_style_json() {
        let json = r#"{
            "type": "treasure",
            "description": "A shiny chest!"
        }"#;
        let kind: EventKind = serde_json::from_str(json).unwrap();
        assert!(matches!(kind, EventKind::Treasure { .. }));
    }
}
