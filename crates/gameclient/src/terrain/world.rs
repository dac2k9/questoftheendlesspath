use bevy::prelude::*;
use questlib::mapgen::{self, Biome, WorldMap};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Ground, Overlay, Terrain};

// World dimensions are NO LONGER compile-time constants. They're
// runtime values set when `WorldGrid::from_seed_with_dims` runs
// (called from `spawn_world` after the client gets `map_width` /
// `map_height` from the `/join` response). The old `world_w()` /
// `world_h()` consts used `mapgen::MAP_W` / `MAP_H` which baked the
// default 100×80 in everywhere — making the chaos adventure's
// 200×160 world impossible to render without a refactor.
//
// All call sites read `world_w()` / `world_h()` (loads the atomic),
// or `world.width` / `world.height` if they already have the
// WorldGrid Resource. The atomics keep working even before
// `spawn_world` runs — defaults match the frost_quest size so
// systems that fire on InGame entry don't crash with "world not
// initialized" before the world plugin's first frame.
static W_ATOMIC: AtomicUsize = AtomicUsize::new(mapgen::MAP_W);
static H_ATOMIC: AtomicUsize = AtomicUsize::new(mapgen::MAP_H);

pub fn world_w() -> usize { W_ATOMIC.load(Ordering::Relaxed) }
pub fn world_h() -> usize { H_ATOMIC.load(Ordering::Relaxed) }
pub fn set_world_dims(w: usize, h: usize) {
    W_ATOMIC.store(w, Ordering::Relaxed);
    H_ATOMIC.store(h, Ordering::Relaxed);
}

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
    /// Generate a default-sized world (100 × 80). Kept for tests
    /// and any caller that doesn't yet know per-adventure dims.
    pub fn from_seed(seed: u64) -> Self {
        Self::from_seed_with_dims(seed, mapgen::MAP_W, mapgen::MAP_H)
    }

    /// Generate a world of the given dimensions. Also publishes
    /// the dims to the `world_w()` / `world_h()` atomics so legacy
    /// call sites read the right value.
    pub fn from_seed_with_dims(seed: u64, width: usize, height: usize) -> Self {
        set_world_dims(width, height);
        let map = WorldMap::generate_sized(seed, width, height);

        let mut cells = vec![
            vec![Cell { ground: Ground::Grass, overlay: None }; width];
            height
        ];

        // Convert biomes to ground tiles
        for y in 0..height {
            for x in 0..width {
                cells[y][x].ground = biome_to_ground(map.biome_at(x, y));
            }
        }

        // Carve roads
        for road in &map.roads {
            for &(rx, ry) in &road.path {
                if rx < width && ry < height {
                    cells[ry][rx].ground = Ground::Road;
                    cells[ry][rx].overlay = None;
                }
            }
        }

        // Place overlays based on biome (trees in forests, rocks in mountains)
        for y in 0..height {
            for x in 0..width {
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

        // Place POI overlays — make POI tiles walkable (Road) and mark the
        // center tile with a type-appropriate overlay so players can see
        // what each POI is from the map.
        //
        // POIs whose type has a custom landmark PNG (see
        // `tilemap::poi_sprite_path`) get a separate 48×48 Sprite spawned
        // later, so we skip the tile-atlas overlay for them. Otherwise a
        // tiny well icon would be stacked under the illustration.
        for poi in &map.pois {
            if poi.x < width && poi.y < height {
                use questlib::mapgen::PoiType::*;
                let has_custom_sprite = matches!(
                    poi.poi_type,
                    Town | Village | Cave | Cabin | Shrine | Ruins | Dungeon | Camp | Tower
                );
                let poi_overlay = match poi.poi_type {
                    Cave => Overlay::CaveEntrance,
                    _ => Overlay::Village,
                };
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let px = (poi.x as i32 + dx) as usize;
                        let py = (poi.y as i32 + dy) as usize;
                        if px < width && py < height {
                            cells[py][px].ground = Ground::Road; // cheap to walk through
                            cells[py][px].overlay = None;
                            if dx == 0 && dy == 0 && !has_custom_sprite {
                                cells[py][px].overlay = Some(poi_overlay);
                            }
                        }
                    }
                }
            }
        }

        // Chests are spawned as separate sprites (not baked into map texture)
        // so they can be despawned when opened.

        WorldGrid {
            cells,
            width,
            height,
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

    /// Movement cost matching the server's calculation (questlib::route::tile_cost).
    /// MUST be used for position computation to stay in sync with the game master.
    pub fn server_tile_cost(&self, x: usize, y: usize) -> u32 {
        let biome = self.map.biome_at(x, y);
        let has_road = self.map.has_road_at(x, y);
        questlib::route::tile_cost(biome, has_road)
    }

    pub fn tile_to_world(x: usize, y: usize) -> Vec2 {
        Vec2::new(x as f32 * TILE_PX, -(y as f32) * TILE_PX)
    }

    pub fn world_to_tile(pos: Vec2) -> (usize, usize) {
        // Offset by half tile since tile_to_world puts tile center at the position
        let x = ((pos.x + TILE_PX * 0.5) / TILE_PX).floor().max(0.0) as usize;
        let y = ((-pos.y + TILE_PX * 0.5) / TILE_PX).floor().max(0.0) as usize;
        (x.min(world_w() - 1), y.min(world_h() - 1))
    }
}

fn biome_to_ground(biome: Biome) -> Ground {
    match biome {
        Biome::Grassland => Ground::Grass,
        Biome::Forest => Ground::Grass,        // grass base, trees as overlay
        Biome::DenseForest => Ground::Grass,    // grass base, dense trees as overlay
        Biome::Mountain => Ground::Stone,        // grey rocky ground
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
