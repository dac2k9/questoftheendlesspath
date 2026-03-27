pub mod path;
pub mod tilemap;
pub mod world;

use serde::{Deserialize, Serialize};

/// Ground terrain types (layer 0) — these use autotiling with 3x3 blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ground {
    Grass,
    Road,
    Water,
    Sand,
    Snow,
    Swamp,
    Lava,
    Mountain,
}

/// Overlay objects (layer 1) — drawn on top of ground with transparency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Overlay {
    Tree,
    DenseTree,
    Village,
    Bridge,
}

/// Combined terrain for game logic.
#[derive(Debug, Clone, Copy)]
pub struct Terrain {
    pub ground: Ground,
    pub overlay: Option<Overlay>,
}

impl Terrain {
    /// Meters required to traverse this tile.
    pub fn movement_cost(&self) -> u32 {
        // Overlay can modify cost
        let base = self.ground.movement_cost();
        if base == u32::MAX {
            return u32::MAX;
        }
        match self.overlay {
            Some(Overlay::Tree) => base + 150,
            Some(Overlay::DenseTree) => base + 250,
            Some(Overlay::Bridge) => 100, // bridges override to easy crossing
            Some(Overlay::Village) => 150,
            None => base,
        }
    }

    pub fn is_passable(&self) -> bool {
        // Bridges make water passable
        if self.overlay == Some(Overlay::Bridge) {
            return true;
        }
        self.ground.is_passable()
    }

    /// Incline percentage (placeholder).
    pub fn incline(&self) -> f32 {
        self.ground.incline()
    }

    /// Display name for HUD.
    pub fn name(&self) -> &'static str {
        match self.overlay {
            Some(Overlay::Tree) => "Forest",
            Some(Overlay::DenseTree) => "Dense Forest",
            Some(Overlay::Village) => "Village",
            Some(Overlay::Bridge) => "Bridge",
            None => self.ground.name(),
        }
    }
}

impl Ground {
    pub fn movement_cost(self) -> u32 {
        match self {
            Ground::Road => 100,
            Ground::Grass => 200,
            Ground::Sand => 250,
            Ground::Snow => 350,
            Ground::Swamp => 500,
            Ground::Mountain => 600,
            Ground::Lava => 800,
            Ground::Water => u32::MAX,
        }
    }

    pub fn is_passable(self) -> bool {
        self.movement_cost() < u32::MAX
    }

    pub fn incline(self) -> f32 {
        match self {
            Ground::Mountain => 0.0, // TODO: set later
            _ => 0.0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Ground::Grass => "Grass",
            Ground::Road => "Road",
            Ground::Water => "Water",
            Ground::Sand => "Sand",
            Ground::Snow => "Snow",
            Ground::Swamp => "Swamp",
            Ground::Lava => "Lava",
            Ground::Mountain => "Mountain",
        }
    }

    /// Base tile index — top-left corner of the 3x3 autotile block.
    /// The 3x3 block is laid out as:
    ///   base+0,  base+1,  base+2     (top-left, top, top-right)
    ///   base+27, base+28, base+29    (left, center, right)
    ///   base+54, base+55, base+56    (bot-left, bot, bot-right)
    /// Base tile index (top-left of 3x3 autotile block).
    /// PunyWorld tileset layout (27 cols):
    ///   Row 0-2, Col 0-2:  Grass (green)
    ///   Row 0-2, Col 3-5:  Dirt/Road (sandy brown)
    ///   Row 0-2, Col 6-8:  Dark grass (grass-to-dirt transitions)
    ///   Row 0-2, Col 9-11: Another sand variant
    ///   Row 0-2, Col 15-17: Lighter sand
    ///   Row 7-9, Col 0-8:  Forest ground (dark green)
    ///   Row 10-12, Col 0-2: Water edges
    ///   Row 10-12, Col 18-20: Solid water fill area
    pub fn autotile_base(self) -> u32 {
        match self {
            Ground::Grass => 0,           // row 0, col 0: green grass
            Ground::Road => 3,            // row 0, col 3: dirt/sandy path
            Ground::Sand => 15,           // row 0, col 15: lighter sand
            Ground::Water => 270,         // row 10, col 0: water with edges
            Ground::Snow => 15,           // reuse sand block for now (light)
            Ground::Swamp => 189,         // row 7, col 0: dark forest ground
            Ground::Mountain => 6,        // row 0, col 6: dark grass/rock
            Ground::Lava => 9,            // row 0, col 9: another sand variant (placeholder)
        }
    }

    /// Center tile index (used when all neighbors match).
    pub fn center_tile(self) -> u32 {
        self.autotile_base() + 28 // row+1, col+1
    }

    /// Color fallback.
    pub fn color(self) -> (f32, f32, f32) {
        match self {
            Ground::Grass => (0.33, 0.66, 0.33),
            Ground::Road => (0.66, 0.60, 0.40),
            Ground::Water => (0.20, 0.40, 0.66),
            Ground::Sand => (0.80, 0.73, 0.53),
            Ground::Snow => (0.87, 0.87, 0.93),
            Ground::Swamp => (0.27, 0.33, 0.20),
            Ground::Mountain => (0.53, 0.47, 0.40),
            Ground::Lava => (0.60, 0.20, 0.07),
        }
    }
}

impl Overlay {
    /// Tile index for the overlay sprite.
    pub fn tile_index(self) -> u32 {
        match self {
            Overlay::Tree => 138,        // row 5, col 3: tree sprite
            Overlay::DenseTree => 165,   // row 6, col 3: dense/darker tree
            Overlay::Village => 769,     // building (row 28, col 13)
            Overlay::Bridge => 85,       // row 3, col 4: wooden bridge
        }
    }
}

/// Compute the autotile index for a ground tile based on its neighbors.
/// `same_fn` returns true if the neighbor at (nx, ny) has the same ground type.
pub fn autotile_index(ground: Ground, x: usize, y: usize, same_fn: impl Fn(usize, usize) -> bool) -> u32 {
    let base = ground.autotile_base();

    let up = y > 0 && same_fn(x, y - 1);
    let down = same_fn(x, y + 1);
    let left = x > 0 && same_fn(x - 1, y);
    let right = same_fn(x + 1, y);

    // Map neighbor pattern to 3x3 block offset
    // The 3x3 block handles: corners, edges, center
    let col_offset = match (left, right) {
        (false, false) => 1, // isolated horizontally → center column (could use special)
        (false, true) => 0,  // left edge
        (true, false) => 2,  // right edge
        (true, true) => 1,   // center
    };

    let row_offset = match (up, down) {
        (false, false) => 1, // isolated vertically → center row
        (false, true) => 0,  // top edge
        (true, false) => 2,  // bottom edge
        (true, true) => 1,   // center
    };

    base + row_offset * 27 + col_offset
}
