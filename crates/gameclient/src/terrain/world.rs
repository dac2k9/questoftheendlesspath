use bevy::prelude::*;
use questlib::mapgen::{self, Biome, WorldMap};

use super::{Ground, Overlay, Terrain};

pub const WORLD_W: usize = mapgen::MAP_W;
pub const WORLD_H: usize = mapgen::MAP_H;
pub const TILE_PX: f32 = 16.0;

#[derive(Clone, Copy)]
pub struct Cell {
    pub ground: Ground,
    pub overlay: Option<Overlay>,
}

impl Cell {
    pub fn terrain(&self) -> Terrain {
        Terrain { ground: self.ground, overlay: self.overlay }
    }
}

#[derive(Resource)]
pub struct WorldGrid {
    pub cells: Vec<Vec<Cell>>,
    pub width: usize,
    pub height: usize,
    /// The underlying generated map (for POIs, roads, etc.)
    pub map: WorldMap,
}

impl WorldGrid {
    /// Generate world from seed using questlib map generator.
    pub fn from_seed(seed: u64) -> Self {
        let map = WorldMap::generate(seed);

        let mut cells = vec![
            vec![Cell { ground: Ground::Grass, overlay: None }; WORLD_W];
            WORLD_H
        ];

        // Convert biomes to ground tiles
        for y in 0..WORLD_H {
            for x in 0..WORLD_W {
                cells[y][x].ground = biome_to_ground(map.biome_at(x, y));
            }
        }

        // Carve roads
        for road in &map.roads {
            for &(rx, ry) in &road.path {
                if rx < WORLD_W && ry < WORLD_H {
                    cells[ry][rx].ground = Ground::Road;
                    cells[ry][rx].overlay = None;
                }
            }
        }

        // Place overlays based on biome (trees in forests, rocks in mountains)
        for y in 0..WORLD_H {
            for x in 0..WORLD_W {
                // Don't place overlays on roads or POIs
                if cells[y][x].ground == Ground::Road {
                    continue;
                }
                if map.poi_at(x, y).is_some() {
                    continue;
                }

                let biome = map.biome_at(x, y);
                let h = hash(x, y);

                cells[y][x].overlay = match biome {
                    Biome::Forest => {
                        if h % 3 != 0 { Some(Overlay::Tree) } else { None }
                    }
                    Biome::DenseForest => {
                        if h % 4 != 0 { Some(Overlay::Tree) } else { None }
                    }
                    Biome::Grassland => {
                        if h % 12 == 0 { Some(Overlay::Tree) } else { None }
                    }
                    Biome::Mountain => {
                        if h % 4 == 0 { Some(Overlay::Rock) } else { None }
                    }
                    Biome::Snow => {
                        if h % 6 == 0 { Some(Overlay::PineTree) } else { None }
                    }
                    _ => None,
                };
            }
        }

        // Place POI overlays (villages, etc.)
        for poi in &map.pois {
            if poi.x < WORLD_W && poi.y < WORLD_H {
                // Clear area around POI
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let px = (poi.x as i32 + dx) as usize;
                        let py = (poi.y as i32 + dy) as usize;
                        if px < WORLD_W && py < WORLD_H {
                            cells[py][px].overlay = None;
                            // POI center gets a village marker
                            if dx == 0 && dy == 0 {
                                cells[py][px].overlay = Some(Overlay::Village);
                            }
                        }
                    }
                }
            }
        }

        WorldGrid {
            cells,
            width: WORLD_W,
            height: WORLD_H,
            map,
        }
    }

    pub fn get(&self, x: usize, y: usize) -> Terrain {
        if x < self.width && y < self.height {
            self.cells[y][x].terrain()
        } else {
            Terrain { ground: Ground::Water, overlay: None }
        }
    }

    pub fn get_ground(&self, x: usize, y: usize) -> Ground {
        if x < self.width && y < self.height { self.cells[y][x].ground } else { Ground::Water }
    }

    pub fn tile_to_world(x: usize, y: usize) -> Vec2 {
        Vec2::new(x as f32 * TILE_PX, -(y as f32) * TILE_PX)
    }

    pub fn world_to_tile(pos: Vec2) -> (usize, usize) {
        // Offset by half tile since tile_to_world puts tile center at the position
        let x = ((pos.x + TILE_PX * 0.5) / TILE_PX).floor().max(0.0) as usize;
        let y = ((-pos.y + TILE_PX * 0.5) / TILE_PX).floor().max(0.0) as usize;
        (x.min(WORLD_W - 1), y.min(WORLD_H - 1))
    }
}

fn biome_to_ground(biome: Biome) -> Ground {
    match biome {
        Biome::Grassland => Ground::Grass,
        Biome::Forest => Ground::Grass,        // grass base, trees as overlay
        Biome::DenseForest => Ground::Grass,    // grass base, dense trees as overlay
        Biome::Mountain => Ground::Sand,        // rocky/sandy ground
        Biome::Desert => Ground::Sand,
        Biome::Snow => Ground::Snow,
        Biome::Swamp => Ground::Swamp,
        Biome::Water => Ground::Water,
        Biome::DeepWater => Ground::Water,
    }
}

fn hash(x: usize, y: usize) -> usize {
    let mut h = (x.wrapping_mul(374761393).wrapping_add(y.wrapping_mul(668265263))) as u32;
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    (h ^ (h >> 16)) as usize
}
