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
}

// ── InteriorMap ─────────────────────────────────────

/// Simple tile grid. Floor is walkable; wall is not. All floors cost the
/// same; no biomes inside (yet). Tile index = y * width + x.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteriorTile {
    Wall,
    Floor,
}

impl InteriorTile {
    pub fn is_walkable(&self) -> bool { matches!(self, InteriorTile::Floor) }
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
    /// Chest positions. ids are local to the interior; tracked in the
    /// player's opened_chests as "<interior_id>:chest:<idx>".
    #[serde(default)]
    pub chests: Vec<(usize, usize)>,
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
        self.chests.iter().position(|&(cx, cy)| cx == x && cy == y)
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
        for (i, &(x, y)) in self.chests.iter().enumerate() {
            if !self.is_walkable(x, y) {
                return Err(format!("chest {} at ({},{}) is not on a floor tile", i, x, y));
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
            }],
            chests: vec![(1, 2)],
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
