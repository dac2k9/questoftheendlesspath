use serde::{Deserialize, Serialize};

/// Top-level adventure file structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdventureFile {
    pub adventure: AdventureMeta,
    pub zones: Vec<Zone>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdventureMeta {
    pub name: String,
    pub description: String,
    pub total_distance_km: f32,
    pub gold_per_km: f32,
    #[serde(default = "default_speed_bonus")]
    pub speed_bonus_gold: f32,
}

fn default_speed_bonus() -> f32 {
    50.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Zone {
    pub name: String,
    pub start_km: f32,
    pub end_km: f32,
    pub tileset: String,
    #[serde(default)]
    pub music: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub events: Vec<EventDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDef {
    pub at_km: f32,
    #[serde(rename = "type")]
    pub event_type: String,
    pub name: String,
    #[serde(default)]
    pub description: String,

    // NPC / Story
    #[serde(default)]
    pub dialogue: Vec<String>,
    #[serde(default)]
    pub dialogue_intro: Vec<String>,
    #[serde(default)]
    pub dialogue_defeat: Vec<String>,

    // Rewards
    #[serde(default)]
    pub reward: Option<Reward>,

    // Boss
    #[serde(default)]
    pub hp: Option<i32>,

    // Hazard
    #[serde(default)]
    pub effect: Option<String>,
    #[serde(default)]
    pub value: Option<f32>,
    #[serde(default)]
    pub duration_km: Option<f32>,

    // Shop
    #[serde(default)]
    pub items: Vec<ShopItem>,

    // Flags
    #[serde(default)]
    pub requires_all_players: bool,
    #[serde(default)]
    pub requires_browser: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reward {
    #[serde(default)]
    pub gold: i32,
    #[serde(default)]
    pub item: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShopItem {
    pub name: String,
    pub cost: i32,
    pub effect: String,
}

impl AdventureFile {
    /// Load an adventure from a YAML file.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let adventure: Self = serde_yaml::from_str(&contents)?;
        Ok(adventure)
    }

    /// Get all events across all zones, sorted by distance.
    pub fn all_events(&self) -> Vec<&EventDef> {
        let mut events: Vec<&EventDef> = self.zones.iter().flat_map(|z| &z.events).collect();
        events.sort_by(|a, b| a.at_km.partial_cmp(&b.at_km).unwrap_or(std::cmp::Ordering::Equal));
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_adventure() {
        let yaml = r#"
adventure:
  name: "Test Adventure"
  description: "A test"
  total_distance_km: 10
  gold_per_km: 100

zones:
  - name: "Zone 1"
    start_km: 0
    end_km: 5
    tileset: meadow
    events:
      - at_km: 2.0
        type: npc
        name: "Test NPC"
        dialogue: ["Hello!"]
        reward: { gold: 50 }
      - at_km: 4.5
        type: boss
        name: "Test Boss"
        hp: 500
        requires_all_players: true
        requires_browser: true
        reward: { gold: 200, item: "sword" }
  - name: "Zone 2"
    start_km: 5
    end_km: 10
    tileset: forest
    events: []
"#;
        let adventure: AdventureFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(adventure.adventure.name, "Test Adventure");
        assert_eq!(adventure.zones.len(), 2);
        assert_eq!(adventure.zones[0].events.len(), 2);
        assert_eq!(adventure.zones[0].events[1].hp, Some(500));
        assert_eq!(adventure.all_events().len(), 2);
    }
}
