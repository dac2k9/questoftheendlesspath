//! Interior spaces — caves, castles, dungeons. Anything you enter via a
//! portal on the overworld (or another interior) and leave via another
//! portal. Shared data model so future "shortcut caves" and "multi-room
//! castles" are the same abstraction.
//!
//! Kept deliberately minimal in Phase 1: walkability + portals + chests.
//! Monsters, events, and per-interior music/art are post-MVP.

use serde::{Deserialize, Serialize};

// ── Location ────────────────────────────────────────

/// Where a player currently is. `Overworld` is the main map; `Interior(id)`
/// is one of the loaded `InteriorMap`s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Location {
    Overworld,
    Interior { id: String },
}

impl Default for Location {
    fn default() -> Self { Location::Overworld }
}

impl Location {
    pub fn interior_id(&self) -> Option<&str> {
        match self {
            Location::Interior { id } => Some(id.as_str()),
            Location::Overworld => None,
        }
    }
}

// ── Portal ──────────────────────────────────────────

/// Where a portal leads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PortalDest {
    /// Drop the player at a fixed overworld tile.
    Overworld { x: i32, y: i32 },
    /// Enter another interior at a specific spawn tile.
    Interior { id: String, x: usize, y: usize },
    /// Return the player to the tile they came from (the last overworld tile
    /// they stood on before entering). Use this for normal cave exits so
    /// stepping out doesn't land on the entrance POI and immediately re-enter.
    OverworldReturn,
}

/// A walkable tile that, when stepped on (or clicked), relocates the player.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Portal {
    pub x: usize,
    pub y: usize,
    pub destination: PortalDest,
    /// Shown on hover / in entry prompts. e.g. "Deeper into the dark…"
    #[serde(default)]
    pub label: String,
    /// If set, the player must have this event id in their completed_events
    /// before they can use this portal. Used for shortcut caves: each
    /// entrance is unlocked only by discovering that side from the outside.
    #[serde(default)]
    pub unlock_event_id: Option<String>,
}

// ── InteriorMap ─────────────────────────────────────

/// Simple tile grid. Floor is walkable; wall is not. All floors cost the
/// same; no biomes inside (yet). Tile index = y * width + x.
///
/// JSON shape: `{"kind": "wall"}` / `{"kind": "floor"}`. The tag is
/// required because the authored interior JSONs use that object form
/// (per-tile) — without the tag, serde expects bare strings and
/// rejects the existing files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InteriorTile {
    Wall,
    Floor,
}

impl InteriorTile {
    pub fn is_walkable(&self) -> bool { matches!(self, InteriorTile::Floor) }
}

/// A monster that spawns at a specific tile. Difficulty drives loot and
/// combat stats (reuses the same values as overworld WorldMonster).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteriorMonster {
    pub x: usize,
    pub y: usize,
    pub monster_type: crate::mapgen::MonsterType,
    #[serde(default = "default_monster_difficulty")]
    pub difficulty: u32,
}

fn default_monster_difficulty() -> u32 { 1 }

/// What a chest contains.
///
/// - `gold` + `items` are always granted.
/// - `rolls` are independent coin flips: each entry is granted with its own
///   probability. Rolls are deterministic per (player, chest, item) so you
///   can't save-scum rerolls.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChestLoot {
    #[serde(default = "default_chest_gold")]
    pub gold: i32,
    /// Item ids to grant every time the chest is opened.
    #[serde(default)]
    pub items: Vec<String>,
    /// Per-item independent probability drops.
    #[serde(default)]
    pub rolls: Vec<LootRoll>,
}

/// A single "flip a coin" entry in a `ChestLoot`. `chance` is clamped to
/// `[0.0, 1.0]` at evaluation time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LootRoll {
    pub item_id: String,
    pub chance: f32,
}

fn default_chest_gold() -> i32 { 30 }

/// Deterministic `[0.0, 1.0)` value derived from `(player_id, chest_key, item_id)`.
/// Same inputs → same output, across ticks and processes. Keeps chest rolls
/// reproducible and un-rerollable, while still varying across players and
/// chests/items.
pub fn roll_rng(player_id: &str, chest_key: &str, item_id: &str) -> f32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    player_id.hash(&mut h);
    chest_key.hash(&mut h);
    item_id.hash(&mut h);
    // Take the low 24 bits so f32 can represent the result exactly and the
    // mapping to [0,1) is uniform.
    let v = (h.finish() & 0x00FF_FFFF) as f32;
    v / (1u32 << 24) as f32
}

/// Returns the list of `item_id`s that the rolls granted for this
/// (player, chest) combination. Order matches the input `rolls`.
pub fn evaluate_rolls(rolls: &[LootRoll], player_id: &str, chest_key: &str) -> Vec<String> {
    rolls.iter()
        .filter(|r| {
            let v = roll_rng(player_id, chest_key, &r.item_id);
            v < r.chance.clamp(0.0, 1.0)
        })
        .map(|r| r.item_id.clone())
        .collect()
}

/// A chest inside an interior. Tracked by index in
/// `DevPlayerState.opened_chests` as "<interior_id>:chest:<idx>".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteriorChest {
    pub x: usize,
    pub y: usize,
    #[serde(default)]
    pub loot: ChestLoot,
}

/// One hand-authored or generated interior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteriorMap {
    pub id: String,
    pub name: String,
    pub width: usize,
    pub height: usize,
    /// Flat row-major tile grid. len must equal width * height.
    pub tiles: Vec<InteriorTile>,
    /// Portals placed on specific floor tiles.
    #[serde(default)]
    pub portals: Vec<Portal>,
    /// Chests. Tracked in the player's opened_chests as
    /// "<interior_id>:chest:<idx>" (see `chest_key`).
    #[serde(default)]
    pub chests: Vec<InteriorChest>,
    /// Monster spawns. Tracked in the player's defeated_monsters as
    /// "<interior_id>:monster:<idx>" (see `monster_key`).
    #[serde(default)]
    pub monsters: Vec<InteriorMonster>,
    /// Flat floor-tile movement cost in meters. Consistent across the map
    /// — caves don't have roads.
    #[serde(default = "default_tile_cost")]
    pub floor_cost_m: u32,
}

fn default_tile_cost() -> u32 { 40 }

impl InteriorMap {
    pub fn tile_at(&self, x: usize, y: usize) -> Option<InteriorTile> {
        if x >= self.width || y >= self.height { return None; }
        self.tiles.get(y * self.width + x).copied()
    }

    pub fn is_walkable(&self, x: usize, y: usize) -> bool {
        self.tile_at(x, y).map_or(false, |t| t.is_walkable())
    }

    /// Returns the portal index at (x, y) if a portal is there.
    pub fn portal_at(&self, x: usize, y: usize) -> Option<usize> {
        self.portals.iter().position(|p| p.x == x && p.y == y)
    }

    /// Returns chest index at (x, y), if any.
    pub fn chest_at(&self, x: usize, y: usize) -> Option<usize> {
        self.chests.iter().position(|c| c.x == x && c.y == y)
    }

    /// Returns monster index at (x, y), if any.
    pub fn monster_at(&self, x: usize, y: usize) -> Option<usize> {
        self.monsters.iter().position(|m| m.x == x && m.y == y)
    }

    /// Validates the map: correct tile count, portals/chests on floor tiles.
    pub fn validate(&self) -> Result<(), String> {
        if self.tiles.len() != self.width * self.height {
            return Err(format!("tile count {} != {}x{}", self.tiles.len(), self.width, self.height));
        }
        for (i, p) in self.portals.iter().enumerate() {
            if !self.is_walkable(p.x, p.y) {
                return Err(format!("portal {} at ({},{}) is not on a floor tile", i, p.x, p.y));
            }
        }
        for (i, c) in self.chests.iter().enumerate() {
            if !self.is_walkable(c.x, c.y) {
                return Err(format!("chest {} at ({},{}) is not on a floor tile", i, c.x, c.y));
            }
        }
        for (i, m) in self.monsters.iter().enumerate() {
            if !self.is_walkable(m.x, m.y) {
                return Err(format!("monster {} at ({},{}) is not on a floor tile", i, m.x, m.y));
            }
        }
        Ok(())
    }
}

// ── Chest key helpers ───────────────────────────────

/// Compound key used in `DevPlayerState.opened_chests` for interior chests.
/// Keeps overworld chest keys backward-compatible (just numeric ids).
pub fn chest_key(interior_id: &str, chest_idx: usize) -> String {
    format!("{}:chest:{}", interior_id, chest_idx)
}

/// Compound key used in `DevPlayerState.defeated_monsters` for interior
/// monsters. Overworld monster keys stay as "monster_<idx>".
pub fn monster_key(interior_id: &str, monster_idx: usize) -> String {
    format!("{}:monster:{}", interior_id, monster_idx)
}

/// Combat `event_id` used when a player starts fighting an interior monster.
/// Kept distinct from the `chest_key` / `monster_key` storage format so the
/// tick loop can detect it by prefix and route to the interior-specific
/// victory handler.
pub fn monster_combat_event_id(interior_id: &str, monster_idx: usize) -> String {
    format!("interior_monster:{}:{}", interior_id, monster_idx)
}

/// Parse `interior_monster:<id>:<idx>`. Returns `(interior_id, idx)` or None
/// if the string doesn't match.
pub fn parse_monster_combat_event_id(event_id: &str) -> Option<(&str, usize)> {
    let rest = event_id.strip_prefix("interior_monster:")?;
    let (id, idx_str) = rest.rsplit_once(':')?;
    Some((id, idx_str.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_map() -> InteriorMap {
        InteriorMap {
            id: "test".into(),
            name: "Test".into(),
            width: 3,
            height: 3,
            tiles: vec![
                InteriorTile::Wall,  InteriorTile::Floor, InteriorTile::Wall,
                InteriorTile::Floor, InteriorTile::Floor, InteriorTile::Floor,
                InteriorTile::Wall,  InteriorTile::Floor, InteriorTile::Wall,
            ],
            portals: vec![Portal {
                x: 1, y: 0,
                destination: PortalDest::Overworld { x: 10, y: 10 },
                label: "Exit".into(),
                unlock_event_id: None,
            }],
            chests: vec![InteriorChest { x: 1, y: 2, loot: ChestLoot::default() }],
            monsters: vec![],
            floor_cost_m: 40,
        }
    }

    #[test]
    fn walkability() {
        let m = tiny_map();
        assert!(!m.is_walkable(0, 0));
        assert!(m.is_walkable(1, 1));
        assert!(m.is_walkable(1, 0));
        assert!(m.tile_at(3, 0).is_none());
    }

    #[test]
    fn portal_lookup() {
        let m = tiny_map();
        assert_eq!(m.portal_at(1, 0), Some(0));
        assert_eq!(m.portal_at(1, 1), None);
    }

    #[test]
    fn chest_lookup() {
        let m = tiny_map();
        assert_eq!(m.chest_at(1, 2), Some(0));
        assert_eq!(m.chest_at(0, 0), None);
    }

    #[test]
    fn validate_ok() {
        assert!(tiny_map().validate().is_ok());
    }

    #[test]
    fn validate_portal_on_wall() {
        let mut m = tiny_map();
        m.portals[0].x = 0; // now on a wall
        m.portals[0].y = 0;
        assert!(m.validate().is_err());
    }

    #[test]
    fn chest_key_format() {
        assert_eq!(chest_key("cave", 3), "cave:chest:3");
    }

    #[test]
    fn monster_key_format() {
        assert_eq!(monster_key("cave", 7), "cave:monster:7");
    }

    #[test]
    fn monster_combat_event_id_roundtrip() {
        let eid = monster_combat_event_id("whispering_cave", 2);
        assert_eq!(eid, "interior_monster:whispering_cave:2");
        assert_eq!(parse_monster_combat_event_id(&eid), Some(("whispering_cave", 2)));
    }

    #[test]
    fn parse_rejects_non_interior_event() {
        assert_eq!(parse_monster_combat_event_id("monster_3"), None);
        assert_eq!(parse_monster_combat_event_id("something_else"), None);
    }

    #[test]
    fn roll_rng_is_deterministic() {
        let a = roll_rng("p1", "cave:chest:0", "iron_sword");
        let b = roll_rng("p1", "cave:chest:0", "iron_sword");
        assert_eq!(a, b);
        // Different player → different value (very high probability).
        let c = roll_rng("p2", "cave:chest:0", "iron_sword");
        assert!((a - c).abs() > 1e-6);
    }

    #[test]
    fn roll_rng_is_in_unit_interval() {
        for seed in &["alice", "bob", "charlie"] {
            let v = roll_rng(seed, "k", "item");
            assert!(v >= 0.0 && v < 1.0, "roll out of range: {v}");
        }
    }

    #[test]
    fn evaluate_rolls_chance_0_grants_nothing() {
        let rolls = vec![LootRoll { item_id: "potion".into(), chance: 0.0 }];
        assert!(evaluate_rolls(&rolls, "p", "k").is_empty());
    }

    #[test]
    fn evaluate_rolls_chance_1_always_grants() {
        let rolls = vec![LootRoll { item_id: "potion".into(), chance: 1.0 }];
        assert_eq!(evaluate_rolls(&rolls, "p", "k"), vec!["potion".to_string()]);
    }

    #[test]
    fn evaluate_rolls_independent() {
        // Two 50% rolls on different items: independent flips.
        let rolls = vec![
            LootRoll { item_id: "a".into(), chance: 0.5 },
            LootRoll { item_id: "b".into(), chance: 0.5 },
        ];
        // Run across many (player, key) seeds; roughly ~25% should grant both.
        let mut both = 0;
        let mut neither = 0;
        let n = 200;
        for i in 0..n {
            let pid = format!("p{}", i);
            let granted = evaluate_rolls(&rolls, &pid, "chest");
            match granted.len() {
                0 => neither += 1,
                2 => both += 1,
                _ => {}
            }
        }
        // Loose sanity bounds — if the hash distribution is terribly skewed
        // we'd see this fail. Expect roughly 50 each; tolerate 20..80.
        assert!(both > 20 && both < n - 20,      "both count={both}");
        assert!(neither > 20 && neither < n - 20, "neither count={neither}");
    }

    #[test]
    fn location_default_overworld() {
        let l: Location = Default::default();
        assert_eq!(l, Location::Overworld);
        assert_eq!(l.interior_id(), None);
    }

    #[test]
    fn location_interior_id() {
        let l = Location::Interior { id: "cave".into() };
        assert_eq!(l.interior_id(), Some("cave"));
    }
}
