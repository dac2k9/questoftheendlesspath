use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::mapgen::Biome;

/// Conditions that activate events. Composable with All/Any.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "condition", rename_all = "snake_case")]
pub enum TriggerCondition {
    /// Player is at a specific tile.
    AtTile { x: usize, y: usize },
    /// Player is at or adjacent to a POI.
    AtPoi { poi_id: usize },
    /// Player is in a specific biome.
    InBiome { biome: Biome },
    /// Player has walked at least N meters total.
    DistanceWalked { meters_min: u32 },
    /// Another event has been completed.
    EventCompleted { event_id: String },
    /// Player has an item.
    HasItem { item: String },
    /// Random chance per tick while in a biome.
    RandomInBiome { biome: Biome, chance: f32 },
    /// All conditions must be true.
    All { conditions: Vec<TriggerCondition> },
    /// At least one condition must be true.
    Any { conditions: Vec<TriggerCondition> },
    /// Inverts the inner condition. Useful for "missing prereq"
    /// notifications, e.g. `not(has_item warm_cloak)` to warn the
    /// player when they reach a gated area without the gate item.
    /// Field is named `inner` (not `condition`) to avoid colliding
    /// with the serde tag (`#[serde(tag = "condition")]`).
    Not { inner: Box<TriggerCondition> },
    /// Always true (for manually triggered events).
    Always,
}

/// Snapshot of player state used to evaluate triggers.
/// Pure data, no references to game systems.
pub struct TriggerContext {
    pub player_tile: (usize, usize),
    pub player_poi: Option<usize>,
    /// POI IDs within a wider radius (for fuzzy matching).
    pub nearby_poi_ids: Vec<usize>,
    pub player_biome: Biome,
    pub total_distance_m: u32,
    pub inventory: Vec<String>,
    pub completed_events: HashSet<String>,
    pub rng_roll: f32,
}

impl TriggerCondition {
    /// Evaluate this condition against the given context. Pure function.
    pub fn evaluate(&self, ctx: &TriggerContext) -> bool {
        match self {
            Self::AtTile { x, y } => ctx.player_tile == (*x, *y),
            Self::AtPoi { poi_id } => ctx.player_poi == Some(*poi_id),
            Self::InBiome { biome } => ctx.player_biome == *biome,
            Self::DistanceWalked { meters_min } => ctx.total_distance_m >= *meters_min,
            Self::EventCompleted { event_id } => ctx.completed_events.contains(event_id),
            Self::HasItem { item } => ctx.inventory.iter().any(|i| i == item),
            Self::RandomInBiome { biome, chance } => {
                ctx.player_biome == *biome && ctx.rng_roll < *chance
            }
            Self::All { conditions } => conditions.iter().all(|c| c.evaluate(ctx)),
            Self::Any { conditions } => conditions.iter().any(|c| c.evaluate(ctx)),
            Self::Not { inner } => !inner.evaluate(ctx),
            Self::Always => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> TriggerContext {
        TriggerContext {
            player_tile: (50, 40),
            player_poi: Some(3),
            nearby_poi_ids: vec![3, 5],
            player_biome: Biome::Forest,
            total_distance_m: 500,
            inventory: vec!["map_fragment".into(), "sword".into()],
            completed_events: {
                let mut s = HashSet::new();
                s.insert("quest_a".into());
                s
            },
            rng_roll: 0.3,
        }
    }

    #[test]
    fn at_tile_match() {
        assert!(TriggerCondition::AtTile { x: 50, y: 40 }.evaluate(&ctx()));
        assert!(!TriggerCondition::AtTile { x: 51, y: 40 }.evaluate(&ctx()));
    }

    #[test]
    fn at_poi_match() {
        assert!(TriggerCondition::AtPoi { poi_id: 3 }.evaluate(&ctx()));
        assert!(!TriggerCondition::AtPoi { poi_id: 5 }.evaluate(&ctx()));
    }

    #[test]
    fn in_biome_match() {
        assert!(TriggerCondition::InBiome { biome: Biome::Forest }.evaluate(&ctx()));
        assert!(!TriggerCondition::InBiome { biome: Biome::Desert }.evaluate(&ctx()));
    }

    #[test]
    fn distance_walked() {
        assert!(TriggerCondition::DistanceWalked { meters_min: 500 }.evaluate(&ctx()));
        assert!(TriggerCondition::DistanceWalked { meters_min: 100 }.evaluate(&ctx()));
        assert!(!TriggerCondition::DistanceWalked { meters_min: 1000 }.evaluate(&ctx()));
    }

    #[test]
    fn event_completed() {
        assert!(TriggerCondition::EventCompleted { event_id: "quest_a".into() }.evaluate(&ctx()));
        assert!(!TriggerCondition::EventCompleted { event_id: "quest_b".into() }.evaluate(&ctx()));
    }

    #[test]
    fn has_item() {
        assert!(TriggerCondition::HasItem { item: "sword".into() }.evaluate(&ctx()));
        assert!(!TriggerCondition::HasItem { item: "shield".into() }.evaluate(&ctx()));
    }

    #[test]
    fn random_in_biome() {
        // rng_roll = 0.3, chance = 0.5 → 0.3 < 0.5 → true (in Forest)
        assert!(TriggerCondition::RandomInBiome { biome: Biome::Forest, chance: 0.5 }.evaluate(&ctx()));
        // Wrong biome
        assert!(!TriggerCondition::RandomInBiome { biome: Biome::Desert, chance: 0.5 }.evaluate(&ctx()));
        // rng_roll = 0.3, chance = 0.1 → 0.3 < 0.1 → false
        assert!(!TriggerCondition::RandomInBiome { biome: Biome::Forest, chance: 0.1 }.evaluate(&ctx()));
    }

    #[test]
    fn all_combinator() {
        let cond = TriggerCondition::All {
            conditions: vec![
                TriggerCondition::AtPoi { poi_id: 3 },
                TriggerCondition::EventCompleted { event_id: "quest_a".into() },
            ],
        };
        assert!(cond.evaluate(&ctx()));

        let cond_fail = TriggerCondition::All {
            conditions: vec![
                TriggerCondition::AtPoi { poi_id: 3 },
                TriggerCondition::EventCompleted { event_id: "quest_b".into() },
            ],
        };
        assert!(!cond_fail.evaluate(&ctx()));
    }

    #[test]
    fn any_combinator() {
        let cond = TriggerCondition::Any {
            conditions: vec![
                TriggerCondition::AtPoi { poi_id: 99 },
                TriggerCondition::HasItem { item: "sword".into() },
            ],
        };
        assert!(cond.evaluate(&ctx()));

        let cond_fail = TriggerCondition::Any {
            conditions: vec![
                TriggerCondition::AtPoi { poi_id: 99 },
                TriggerCondition::HasItem { item: "shield".into() },
            ],
        };
        assert!(!cond_fail.evaluate(&ctx()));
    }

    #[test]
    fn always() {
        assert!(TriggerCondition::Always.evaluate(&ctx()));
    }

    #[test]
    fn not_combinator() {
        // Inventory has "sword" → has_item true → not is false
        let no_shield = TriggerCondition::Not {
            inner: Box::new(TriggerCondition::HasItem { item: "shield".into() }),
        };
        assert!(no_shield.evaluate(&ctx()));
        let no_sword = TriggerCondition::Not {
            inner: Box::new(TriggerCondition::HasItem { item: "sword".into() }),
        };
        assert!(!no_sword.evaluate(&ctx()));
    }

    #[test]
    fn not_serialize_roundtrip() {
        let cond = TriggerCondition::Not {
            inner: Box::new(TriggerCondition::HasItem { item: "warm_cloak".into() }),
        };
        let json = serde_json::to_string(&cond).unwrap();
        let roundtrip: TriggerCondition = serde_json::from_str(&json).unwrap();
        assert_eq!(cond, roundtrip);
    }

    #[test]
    fn serialize_roundtrip() {
        let cond = TriggerCondition::All {
            conditions: vec![
                TriggerCondition::AtPoi { poi_id: 5 },
                TriggerCondition::EventCompleted { event_id: "intro".into() },
            ],
        };
        let json = serde_json::to_string(&cond).unwrap();
        let roundtrip: TriggerCondition = serde_json::from_str(&json).unwrap();
        assert_eq!(cond, roundtrip);
    }
}
