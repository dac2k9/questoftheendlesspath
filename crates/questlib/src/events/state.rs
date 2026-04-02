use serde::{Deserialize, Serialize};

use super::kind::EventKind;
use super::outcome::EventOutcome;
use super::trigger::TriggerCondition;

/// Lifecycle state of an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Pending,
    Active,
    Completed,
    Failed,
    Dismissed,
}

/// A concrete event instance in a game.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventInstance {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub kind: EventKind,
    pub trigger: TriggerCondition,
    #[serde(default)]
    pub outcomes: Vec<EventOutcome>,
    #[serde(default = "default_status")]
    pub status: EventStatus,
    /// If true, all players must be present for this event.
    #[serde(default)]
    pub requires_all_players: bool,
    /// If true, browser must be open to interact with this event.
    #[serde(default)]
    pub requires_browser: bool,
    #[serde(default)]
    pub triggered_at: Option<u64>,
    #[serde(default)]
    pub completed_at: Option<u64>,
    /// If true, event resets to Pending after completion (e.g. shops).
    #[serde(default)]
    pub repeatable: bool,
}

fn default_status() -> EventStatus {
    EventStatus::Pending
}

#[derive(Debug)]
pub struct InvalidTransition {
    pub from: EventStatus,
    pub to: EventStatus,
}

impl std::fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid transition: {:?} → {:?}", self.from, self.to)
    }
}

impl EventInstance {
    /// Attempt a state transition. Returns Ok if valid.
    pub fn transition(&mut self, to: EventStatus) -> Result<(), InvalidTransition> {
        let valid = matches!(
            (self.status, to),
            (EventStatus::Pending, EventStatus::Active)
                | (EventStatus::Active, EventStatus::Completed)
                | (EventStatus::Active, EventStatus::Failed)
                | (EventStatus::Active, EventStatus::Dismissed)
        );
        if valid {
            self.status = to;
            Ok(())
        } else {
            Err(InvalidTransition { from: self.status, to })
        }
    }

    /// Force a status change, bypassing transition validation.
    /// Used for combat retreat (Active → Pending).
    pub fn force_status(&mut self, status: EventStatus) {
        self.status = status;
    }

    /// Whether this event auto-completes (doesn't need browser interaction).
    pub fn auto_completes(&self) -> bool {
        !self.requires_browser
            && matches!(
                self.kind,
                EventKind::Treasure { .. }
                    | EventKind::StoryBeat { .. }
                    | EventKind::EnvironmentalEffect { .. }
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::trigger::TriggerCondition;

    fn test_event() -> EventInstance {
        EventInstance {
            id: "test".into(),
            name: "Test Event".into(),
            description: "A test.".into(),
            kind: EventKind::Treasure {
                description: "Gold chest".into(),
            },
            trigger: TriggerCondition::Always,
            outcomes: vec![EventOutcome::Gold { amount: 50 }],
            status: EventStatus::Pending,
            requires_all_players: false,
            requires_browser: false,
            triggered_at: None,
            completed_at: None,
            repeatable: false,
        }
    }

    #[test]
    fn valid_transitions() {
        let mut e = test_event();
        assert!(e.transition(EventStatus::Active).is_ok());
        assert_eq!(e.status, EventStatus::Active);
        assert!(e.transition(EventStatus::Completed).is_ok());
        assert_eq!(e.status, EventStatus::Completed);
    }

    #[test]
    fn active_to_failed() {
        let mut e = test_event();
        e.transition(EventStatus::Active).unwrap();
        assert!(e.transition(EventStatus::Failed).is_ok());
    }

    #[test]
    fn active_to_dismissed() {
        let mut e = test_event();
        e.transition(EventStatus::Active).unwrap();
        assert!(e.transition(EventStatus::Dismissed).is_ok());
    }

    #[test]
    fn invalid_skip_active() {
        let mut e = test_event();
        assert!(e.transition(EventStatus::Completed).is_err());
    }

    #[test]
    fn invalid_backwards() {
        let mut e = test_event();
        e.transition(EventStatus::Active).unwrap();
        e.transition(EventStatus::Completed).unwrap();
        assert!(e.transition(EventStatus::Active).is_err());
    }

    #[test]
    fn auto_completes() {
        let treasure = test_event();
        assert!(treasure.auto_completes());

        let mut dialogue = test_event();
        dialogue.kind = EventKind::NpcDialogue {
            speaker: "NPC".into(),
            portrait: None,
            lines: vec!["Hi".into()],
            choices: vec![],
        };
        dialogue.requires_browser = true;
        assert!(!dialogue.auto_completes());
    }

    #[test]
    fn serialize_roundtrip() {
        let e = test_event();
        let json = serde_json::to_string(&e).unwrap();
        let roundtrip: EventInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.id, e.id);
        assert_eq!(roundtrip.status, EventStatus::Pending);
    }
}
