pub mod interior;
pub mod path;
pub mod tilemap;
pub mod world;

use serde::{Deserialize, Serialize};

/// Ground terrain types (layer 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ground {
    Grass,
    Road,      // textured/dead grass as path
    Water,
    Sand,
    Stone,     // grey rocky mountain ground
    Snow,
    Swamp,
}

/// Overlay objects (layer 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Overlay {
    Tree,
    PineTree,
    Rock,
    Village,
    Bridge,
    Wheat,
    Chest,
    /// Distinct visual for cave POIs so players can see the entrance on the
    /// overworld without hovering / holding TAB.
    CaveEntrance,
}

/// Combined terrain.
#[derive(Debug, Clone, Copy)]
pub struct Terrain {
    pub ground: Ground,
    pub overlay: Option<Overlay>,
}

impl Terrain {
    pub fn movement_cost(&self) -> u32 {
        if let Some(Overlay::Bridge) = self.overlay {
            return 100;
        }
        let base = self.ground.movement_cost();
        if base == u32::MAX { return u32::MAX; }
        match self.overlay {
            Some(Overlay::Tree | Overlay::PineTree) => base + 30,
            Some(Overlay::Rock) => base + 40,
            Some(Overlay::Village) => 30,
            Some(Overlay::Wheat) => base + 10,
            Some(Overlay::Bridge) => 20,
            Some(Overlay::Chest) => base, // no extra cost
            Some(Overlay::CaveEntrance) => base, // walkable, same as the underlying road
            None => base,
        }
    }

    pub fn is_passable(&self) -> bool {
        if self.overlay == Some(Overlay::Bridge) { return true; }
        self.ground.is_passable()
    }

    pub fn name(&self) -> &'static str {
        match self.overlay {
            Some(Overlay::Tree) => "Forest",
            Some(Overlay::PineTree) => "Pine Forest",
            Some(Overlay::Rock) => "Rocky",
            Some(Overlay::Village) => "Village",
            Some(Overlay::Bridge) => "Bridge",
            Some(Overlay::Wheat) => "Wheat Field",
            Some(Overlay::Chest) => "Chest",
            Some(Overlay::CaveEntrance) => "Cave",
            None => self.ground.name(),
        }
    }
}

impl Ground {
    pub fn movement_cost(self) -> u32 {
        match self {
            Ground::Road => 20,
            Ground::Grass => 40,
            Ground::Sand => 50,
            Ground::Snow => 70,
            Ground::Stone => 80,
            Ground::Swamp => 100,
            Ground::Water => u32::MAX,
        }
    }

    pub fn is_passable(self) -> bool {
        self.movement_cost() < u32::MAX
    }

    pub fn name(self) -> &'static str {
        match self {
            Ground::Grass => "Grass",
            Ground::Road => "Road",
            Ground::Water => "Water",
            Ground::Sand => "Sand",
            Ground::Stone => "Stone",
            Ground::Snow => "Snow",
            Ground::Swamp => "Swamp",
        }
    }

    /// Tile index in the MiniWorld atlas (16 cols per row).
    /// Atlas layout: see build script output for full index list.
    /// Tile index in the MiniWorld atlas.
    /// Multiple variants available — use random_variant() for variety.
    pub fn tile_index(self) -> usize {
        match self {
            Ground::Grass => 5,      // textured grass (more detail)
            Ground::Road => 3,       // light sandy (shore tile — matches original road look)
            Ground::Water => 19,     // blue water
            Ground::Sand => 11,      // sandy
            Ground::Stone => 13,     // grey stone/rock
            Ground::Snow => 22,      // white snow
            Ground::Swamp => 15,     // dark muddy
        }
    }

    /// Get a tile variant based on position for visual variety.
    pub fn tile_index_varied(self, x: usize, y: usize) -> usize {
        let h = ((x.wrapping_mul(374761393).wrapping_add(y.wrapping_mul(668265263))) >> 16) as usize;
        match self {
            Ground::Grass => [5, 6, 7, 8, 9, 10][h % 6],  // textured grass variants
            Ground::Road => [3, 4][h % 2],                    // light sandy road variants
            Ground::Water => 20,                             // solid blue water (no variation)
            Ground::Sand => [11, 12][h % 2],
            Ground::Stone => [13, 17][h % 2],               // grey stone variants
            Ground::Snow => [22, 23, 24, 25][h % 4],       // snow variants
            Ground::Swamp => [14, 15, 16][h % 3],
        }
    }
}

impl Overlay {
    pub fn tile_index(self) -> usize {
        match self {
            Overlay::Tree => 31,       // tree_0_1 (green leafy tree)
            Overlay::PineTree => 35,   // pine_0_1 (green pine)
            Overlay::Rock => 41,       // rock_0_0
            Overlay::Village => 84,    // well_0_0 (placeholder)
            Overlay::Bridge => 71,     // bridge_1_1
            Overlay::Wheat => 61,      // wheat_0_0
            Overlay::Chest => 80,      // crate/chest tile
            Overlay::CaveEntrance => 43, // rock variant — visually distinct from Rock=41
        }
    }

    pub fn tile_index_varied(self, x: usize, y: usize) -> usize {
        let h = ((x.wrapping_mul(374761393).wrapping_add(y.wrapping_mul(668265263))) >> 16) as usize;
        match self {
            Overlay::Tree => [31, 32, 33][h % 3],    // tree variants (green)
            Overlay::PineTree => [35, 36][h % 2],     // pine variants
            Overlay::Chest => 80,                      // chest (no variants)
            _ => self.tile_index(),
        }
    }
}
