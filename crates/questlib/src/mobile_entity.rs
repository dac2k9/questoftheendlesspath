//! Mobile entity types — authored definitions + runtime state for
//! autonomous moving NPCs / monsters / animals.
//!
//! See `adventures/MOBILE_ENTITIES.md` for the design spec this module
//! implements. Pure data + a JSON loader; no Bevy / no server tick
//! logic here. The server (`gamemaster::mobile_entity`) owns the tick
//! loop and the client (`gameclient::entities`) renders.

use serde::{Deserialize, Serialize};

// ── Authored definition (loaded from JSON) ──────────────

/// Entity definition as authored in `adventures/seed{N}_entities.json`.
/// Immutable once loaded — the runtime state lives in
/// `MobileEntityState` and gets serialized into the save file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MobileEntityDef {
    /// Stable, unique per world. Used as save key.
    pub id: String,
    pub kind: EntityKind,
    /// Sprite registry name (e.g. "wolf"). Resolves to an asset path
    /// on the client at render time.
    pub sprite: String,
    /// Initial / respawn tile.
    pub spawn: (usize, usize),
    pub behavior: Behavior,
    #[serde(default)]
    pub movement: Movement,
    pub on_contact: ContactAction,
    /// Null = permanent kill. 600 = respawn after 10 minutes.
    #[serde(default)]
    pub respawn_after_secs: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Monster,
    Npc,
    Animal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Movement {
    /// Tiles per minute. 6 = 1 tile / 10s, the default — feels alive
    /// without zipping. ~30 ≈ player walking speed at 3 km/h.
    #[serde(default = "default_speed")]
    pub speed_tiles_per_min: u32,
}

fn default_speed() -> u32 {
    6
}

impl Default for Movement {
    fn default() -> Self {
        Self {
            speed_tiles_per_min: default_speed(),
        }
    }
}

/// Per-tick decision rule for picking the entity's next tile.
/// MVP ships Wander + Patrol; FollowPath / reactive layers are Phase 2.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Behavior {
    /// Random walk among walkable neighbors, biased to stay within
    /// `radius` tiles of the spawn point.
    Wander { radius: u32 },
    /// Loop through `waypoints` in order; on reaching the last,
    /// advance per `loop_mode`.
    Patrol {
        waypoints: Vec<(usize, usize)>,
        #[serde(default = "default_loop_mode")]
        loop_mode: LoopMode,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoopMode {
    /// 0, 1, …, N, 0, 1, … — teleports back to the first waypoint
    /// once the last is reached. Cleanest for circular routes.
    Wrap,
    /// 0, 1, …, N, N-1, …, 0, 1, … — reverses direction at each end.
    /// Good for back-and-forth patrols.
    Bounce,
}

fn default_loop_mode() -> LoopMode {
    LoopMode::Wrap
}

/// What happens when a player and the entity meet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContactAction {
    /// Triggers combat with the given difficulty when the player steps
    /// onto the entity's tile. Maps to `EventKind::RandomEncounter` (or
    /// `Boss` if difficulty ≥ 6) at fight-start.
    Combat { difficulty: u32 },
    /// Triggers the named event when the player is adjacent
    /// (Chebyshev distance ≤ 1).
    Dialogue { event_id: String },
    /// Opens the named shop event when the player is adjacent.
    Trade { shop_event_id: String },
    /// No effect on contact. For pure-flavor animals.
    None,
}

// ── Runtime state (persists in save file) ───────────────

/// Mutable per-tick runtime state for one entity. Built from a
/// `MobileEntityDef` at boot, mutated by the server tick, serialized
/// into `dev_state.json` so positions and respawn timers survive
/// restarts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MobileEntityState {
    pub current: (usize, usize),
    pub facing: Facing,
    /// Server-monotonic clock, ms since unix epoch. The tick checks
    /// `now − last_step_unix_ms ≥ step_interval_ms` to know when this
    /// entity is allowed to move again.
    #[serde(default)]
    pub last_step_unix_ms: u64,
    pub behavior_state: BehaviorState,
    /// `true` while the entity is alive. When killed:
    ///   - `alive = false`
    ///   - `respawn_at_unix_ms = now + respawn_after_secs * 1000`
    ///   - on tick where `now ≥ respawn_at_unix_ms`, respawn at
    ///     `def.spawn` and flip `alive` back to true.
    pub alive: bool,
    #[serde(default)]
    pub respawn_at_unix_ms: u64,
}

impl MobileEntityState {
    /// Fresh runtime state for a newly-loaded entity definition. Used
    /// when no save data exists yet for this entity id.
    pub fn from_def(def: &MobileEntityDef) -> Self {
        Self {
            current: def.spawn,
            facing: Facing::Down,
            last_step_unix_ms: 0,
            behavior_state: BehaviorState::for_behavior(&def.behavior),
            alive: true,
            respawn_at_unix_ms: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Facing {
    Up,
    Down,
    Left,
    Right,
}

/// Behavior-specific runtime state. Wander is stateless (just random
/// each tick). Patrol tracks where in the waypoint loop we are and
/// which direction we're going (for Bounce mode).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BehaviorState {
    Wander,
    Patrol {
        idx: usize,
        /// Direction along the waypoint list. Always `true` for Wrap;
        /// flips at endpoints for Bounce.
        forward: bool,
    },
}

impl BehaviorState {
    /// Initial behavior-specific state for a fresh entity (or one that
    /// just respawned). Public so the server can rebuild it on
    /// respawn without reaching through `MobileEntityState::from_def`.
    pub fn for_behavior(b: &Behavior) -> Self {
        match b {
            Behavior::Wander { .. } => BehaviorState::Wander,
            Behavior::Patrol { .. } => BehaviorState::Patrol { idx: 0, forward: true },
        }
    }
}

// ── JSON loading ────────────────────────────────────────

/// Top-level JSON shape: `{ "entities": [ ... ] }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitiesFile {
    pub entities: Vec<MobileEntityDef>,
}

/// Parse the contents of an `adventures/seed{N}_entities.json` file.
pub fn parse_entities_json(s: &str) -> Result<Vec<MobileEntityDef>, serde_json::Error> {
    let f: EntitiesFile = serde_json::from_str(s)?;
    Ok(f.entities)
}

// ── Tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_wolf() -> MobileEntityDef {
        MobileEntityDef {
            id: "forest_wolf_1".into(),
            kind: EntityKind::Monster,
            sprite: "wolf".into(),
            spawn: (30, 40),
            behavior: Behavior::Wander { radius: 4 },
            movement: Movement::default(),
            on_contact: ContactAction::Combat { difficulty: 2 },
            respawn_after_secs: Some(600),
        }
    }

    #[test]
    fn def_roundtrip() {
        let d = sample_wolf();
        let json = serde_json::to_string(&d).unwrap();
        let back: MobileEntityDef = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn parses_authoring_format() {
        let json = r#"{
            "entities": [
                {
                    "id": "forest_wolf_1",
                    "kind": "monster",
                    "sprite": "wolf",
                    "spawn": [30, 40],
                    "behavior": { "type": "wander", "radius": 4 },
                    "movement": { "speed_tiles_per_min": 6 },
                    "on_contact": { "type": "combat", "difficulty": 2 },
                    "respawn_after_secs": 600
                },
                {
                    "id": "village_baker",
                    "kind": "npc",
                    "sprite": "baker",
                    "spawn": [35, 22],
                    "behavior": { "type": "patrol",
                                  "waypoints": [[35,22], [37,22], [37,24]],
                                  "loop_mode": "bounce" },
                    "on_contact": { "type": "dialogue", "event_id": "baker_intro" }
                }
            ]
        }"#;
        let entities = parse_entities_json(json).expect("parse");
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].id, "forest_wolf_1");
        assert!(matches!(entities[0].behavior, Behavior::Wander { radius: 4 }));
        assert!(matches!(entities[0].on_contact, ContactAction::Combat { difficulty: 2 }));
        assert_eq!(entities[1].kind, EntityKind::Npc);
        assert!(matches!(
            entities[1].behavior,
            Behavior::Patrol { loop_mode: LoopMode::Bounce, .. }
        ));
    }

    #[test]
    fn movement_default_speed() {
        // Movement omitted in JSON → default 6 tpm.
        let json = r#"{
            "entities": [{
                "id": "x", "kind": "animal", "sprite": "deer",
                "spawn": [0, 0],
                "behavior": { "type": "wander", "radius": 2 },
                "on_contact": { "type": "none" }
            }]
        }"#;
        let entities = parse_entities_json(json).unwrap();
        assert_eq!(entities[0].movement.speed_tiles_per_min, 6);
    }

    #[test]
    fn loop_mode_default_wrap() {
        let json = r#"{
            "entities": [{
                "id": "x", "kind": "monster", "sprite": "goblin",
                "spawn": [0, 0],
                "behavior": { "type": "patrol", "waypoints": [[0,0],[1,0]] },
                "on_contact": { "type": "combat", "difficulty": 1 }
            }]
        }"#;
        let entities = parse_entities_json(json).unwrap();
        match &entities[0].behavior {
            Behavior::Patrol { loop_mode, .. } => assert_eq!(*loop_mode, LoopMode::Wrap),
            _ => panic!("expected Patrol"),
        }
    }

    #[test]
    fn state_from_def_starts_alive_at_spawn() {
        let d = sample_wolf();
        let s = MobileEntityState::from_def(&d);
        assert_eq!(s.current, d.spawn);
        assert!(s.alive);
        assert_eq!(s.behavior_state, BehaviorState::Wander);
        assert_eq!(s.respawn_at_unix_ms, 0);
    }

    #[test]
    fn state_from_def_for_patrol_seeds_idx_zero() {
        let d = MobileEntityDef {
            id: "p".into(),
            kind: EntityKind::Npc,
            sprite: "guard".into(),
            spawn: (10, 10),
            behavior: Behavior::Patrol {
                waypoints: vec![(10, 10), (12, 10), (12, 12)],
                loop_mode: LoopMode::Wrap,
            },
            movement: Movement::default(),
            on_contact: ContactAction::None,
            respawn_after_secs: None,
        };
        let s = MobileEntityState::from_def(&d);
        assert_eq!(
            s.behavior_state,
            BehaviorState::Patrol { idx: 0, forward: true }
        );
    }

    #[test]
    fn state_roundtrip() {
        let s = MobileEntityState {
            current: (12, 14),
            facing: Facing::Right,
            last_step_unix_ms: 1745000000000,
            behavior_state: BehaviorState::Patrol { idx: 2, forward: false },
            alive: false,
            respawn_at_unix_ms: 1745001000000,
        };
        let j = serde_json::to_string(&s).unwrap();
        let back: MobileEntityState = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }
}
