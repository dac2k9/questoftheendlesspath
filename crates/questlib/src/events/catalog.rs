use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::state::{EventInstance, EventStatus};
use super::trigger::TriggerContext;

/// Collection of all events in a game. The Game Master's event state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EventCatalog {
    pub events: Vec<EventInstance>,
}

impl EventCatalog {
    /// Find all pending events whose triggers match the current context.
    pub fn check_triggers<'a>(&'a self, ctx: &TriggerContext) -> Vec<&'a EventInstance> {
        self.events
            .iter()
            .filter(|e| e.status == EventStatus::Pending && e.trigger.evaluate(ctx))
            .collect()
    }

    /// Get event by id.
    pub fn get(&self, id: &str) -> Option<&EventInstance> {
        self.events.iter().find(|e| e.id == id)
    }

    /// Get mutable event by id.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut EventInstance> {
        self.events.iter_mut().find(|e| e.id == id)
    }

    /// All completed event IDs (for prerequisite checking).
    pub fn completed_ids(&self) -> HashSet<String> {
        self.events
            .iter()
            .filter(|e| e.status == EventStatus::Completed)
            .map(|e| e.id.clone())
            .collect()
    }

    /// All active events (for browser rendering).
    pub fn active_events(&self) -> Vec<&EventInstance> {
        self.events
            .iter()
            .filter(|e| e.status == EventStatus::Active)
            .collect()
    }

    /// Load from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialize to JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::kind::EventKind;
    use crate::events::outcome::EventOutcome;
    use crate::events::trigger::TriggerCondition;
    use crate::mapgen::Biome;

    fn test_catalog() -> EventCatalog {
        EventCatalog {
            events: vec![
                EventInstance {
                    id: "treasure_1".into(),
                    name: "Hidden Chest".into(),
                    description: "A chest in the forest.".into(),
                    kind: EventKind::Treasure { description: "Gold chest".into() },
                    trigger: TriggerCondition::AtTile { x: 50, y: 40 },
                    outcomes: vec![EventOutcome::Gold { amount: 100 }],
                    status: EventStatus::Pending,
                    requires_all_players: false,
                    requires_browser: false,
                    triggered_at: None,
                    completed_at: None,
                    repeatable: false,
                },
                EventInstance {
                    id: "npc_sage".into(),
                    name: "Old Sage".into(),
                    description: "A wise old man.".into(),
                    kind: EventKind::NpcDialogue {
                        speaker: "Sage Merin".into(),
                        portrait: None,
                        lines: vec!["Hello traveler!".into()],
                        choices: vec![],
                    },
                    trigger: TriggerCondition::AtPoi { poi_id: 3 },
                    outcomes: vec![EventOutcome::Gold { amount: 50 }],
                    status: EventStatus::Pending,
                    requires_all_players: false,
                    requires_browser: true,
                    triggered_at: None,
                    completed_at: None,
                    repeatable: false,
                },
                EventInstance {
                    id: "boss_troll".into(),
                    name: "Bridge Troll".into(),
                    description: "A troll blocks the path.".into(),
                    kind: EventKind::Boss {
                        boss_name: "Bridge Troll".into(),
                        max_hp: 800,
                        portrait: None,
                        dialogue_intro: vec!["NONE SHALL PASS!".into()],
                        dialogue_defeat: vec!["Ugh...".into()],
                    },
                    trigger: TriggerCondition::All {
                        conditions: vec![
                            TriggerCondition::AtPoi { poi_id: 5 },
                            TriggerCondition::EventCompleted { event_id: "npc_sage".into() },
                        ],
                    },
                    outcomes: vec![
                        EventOutcome::Gold { amount: 300 },
                        EventOutcome::Item { name: "troll_shield".into() },
                    ],
                    status: EventStatus::Pending,
                    requires_all_players: true,
                    requires_browser: true,
                    triggered_at: None,
                    completed_at: None,
                    repeatable: false,
                },
            ],
        }
    }

    fn ctx_at(tile: (usize, usize), poi: Option<usize>, completed: &[&str]) -> TriggerContext {
        TriggerContext {
            player_tile: tile,
            player_poi: poi,
            nearby_poi_ids: poi.into_iter().collect(),
            player_biome: Biome::Forest,
            total_distance_m: 500,
            inventory: vec![],
            completed_events: completed.iter().map(|s| s.to_string()).collect(),
            rng_roll: 0.5,
        }
    }

    #[test]
    fn check_triggers_at_tile() {
        let catalog = test_catalog();
        let ctx = ctx_at((50, 40), None, &[]);
        let triggered = catalog.check_triggers(&ctx);
        assert_eq!(triggered.len(), 1);
        assert_eq!(triggered[0].id, "treasure_1");
    }

    #[test]
    fn check_triggers_at_poi() {
        let catalog = test_catalog();
        let ctx = ctx_at((30, 20), Some(3), &[]);
        let triggered = catalog.check_triggers(&ctx);
        assert_eq!(triggered.len(), 1);
        assert_eq!(triggered[0].id, "npc_sage");
    }

    #[test]
    fn prerequisite_not_met() {
        let catalog = test_catalog();
        // At POI 5 but npc_sage not completed → boss should NOT trigger
        let ctx = ctx_at((60, 50), Some(5), &[]);
        let triggered = catalog.check_triggers(&ctx);
        assert!(triggered.iter().all(|e| e.id != "boss_troll"));
    }

    #[test]
    fn prerequisite_met() {
        let catalog = test_catalog();
        // At POI 5 AND npc_sage completed → boss triggers
        let ctx = ctx_at((60, 50), Some(5), &["npc_sage"]);
        let triggered = catalog.check_triggers(&ctx);
        assert!(triggered.iter().any(|e| e.id == "boss_troll"));
    }

    #[test]
    fn completed_events_tracked() {
        let mut catalog = test_catalog();
        catalog.get_mut("treasure_1").unwrap().transition(EventStatus::Active).unwrap();
        catalog.get_mut("treasure_1").unwrap().transition(EventStatus::Completed).unwrap();

        let ids = catalog.completed_ids();
        assert!(ids.contains("treasure_1"));
        assert!(!ids.contains("npc_sage"));
    }

    #[test]
    fn already_active_not_retriggered() {
        let mut catalog = test_catalog();
        catalog.get_mut("treasure_1").unwrap().transition(EventStatus::Active).unwrap();

        let ctx = ctx_at((50, 40), None, &[]);
        let triggered = catalog.check_triggers(&ctx);
        // Should not include treasure_1 since it's already Active
        assert!(triggered.iter().all(|e| e.id != "treasure_1"));
    }

    #[test]
    fn json_roundtrip() {
        let catalog = test_catalog();
        let json = catalog.to_json();
        let roundtrip = EventCatalog::from_json(&json).unwrap();
        assert_eq!(roundtrip.events.len(), 3);
        assert_eq!(roundtrip.events[0].id, "treasure_1");
        assert_eq!(roundtrip.events[2].id, "boss_troll");
    }

    #[test]
    fn active_events() {
        let mut catalog = test_catalog();
        assert!(catalog.active_events().is_empty());
        catalog.get_mut("npc_sage").unwrap().transition(EventStatus::Active).unwrap();
        let active = catalog.active_events();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "npc_sage");
    }
}
