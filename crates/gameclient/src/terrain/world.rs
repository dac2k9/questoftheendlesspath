use bevy::prelude::*;

use super::{Ground, Overlay, Terrain};

pub const WORLD_W: usize = 100;
pub const WORLD_H: usize = 80;
pub const TILE_PX: f32 = 16.0;

/// A single cell in the world grid.
#[derive(Clone, Copy)]
pub struct Cell {
    pub ground: Ground,
    pub overlay: Option<Overlay>,
}

impl Cell {
    pub fn terrain(&self) -> Terrain {
        Terrain {
            ground: self.ground,
            overlay: self.overlay,
        }
    }
}

/// The game world grid.
#[derive(Resource)]
pub struct WorldGrid {
    pub cells: Vec<Vec<Cell>>,
    pub width: usize,
    pub height: usize,
}

impl WorldGrid {
    pub fn generate() -> Self {
        let mut cells = vec![
            vec![
                Cell {
                    ground: Ground::Grass,
                    overlay: None,
                };
                WORLD_W
            ];
            WORLD_H
        ];

        // Step 1: Generate ground layer
        for y in 0..WORLD_H {
            for x in 0..WORLD_W {
                cells[y][x].ground = ground_at(x, y);
            }
        }

        // Step 2: Carve roads (on ground layer)
        carve_road(&mut cells, 10, 40, 90, 40);
        carve_road(&mut cells, 50, 10, 50, 70);
        carve_road(&mut cells, 20, 20, 80, 60);

        // Step 3: Place overlays (trees, villages)
        for y in 0..WORLD_H {
            for x in 0..WORLD_W {
                cells[y][x].overlay = overlay_at(x, y, cells[y][x].ground);
            }
        }

        // Step 4: Place villages (clear trees, add buildings)
        place_village(&mut cells, 50, 40);
        place_village(&mut cells, 25, 25);
        place_village(&mut cells, 75, 55);

        // Step 5: Place bridges where roads cross water
        for y in 0..WORLD_H {
            for x in 0..WORLD_W {
                if cells[y][x].ground == Ground::Road {
                    let has_water = [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)]
                        .iter()
                        .any(|&(nx, ny)| {
                            nx < WORLD_W && ny < WORLD_H && cells[ny][nx].ground == Ground::Water
                        });
                    if has_water {
                        cells[y][x].overlay = Some(Overlay::Bridge);
                    }
                }
            }
        }

        WorldGrid {
            cells,
            width: WORLD_W,
            height: WORLD_H,
        }
    }

    pub fn get(&self, x: usize, y: usize) -> Terrain {
        if x < self.width && y < self.height {
            self.cells[y][x].terrain()
        } else {
            Terrain {
                ground: Ground::Water,
                overlay: None,
            }
        }
    }

    pub fn get_ground(&self, x: usize, y: usize) -> Ground {
        if x < self.width && y < self.height {
            self.cells[y][x].ground
        } else {
            Ground::Water
        }
    }

    pub fn tile_to_world(x: usize, y: usize) -> Vec2 {
        Vec2::new(x as f32 * TILE_PX, -(y as f32) * TILE_PX)
    }

    pub fn world_to_tile(pos: Vec2) -> (usize, usize) {
        let x = (pos.x / TILE_PX).floor().max(0.0) as usize;
        let y = ((-pos.y) / TILE_PX).floor().max(0.0) as usize;
        (x.min(WORLD_W - 1), y.min(WORLD_H - 1))
    }
}

// ── Ground Generation ─────────────────────────────────

fn ground_at(x: usize, y: usize) -> Ground {
    if x < 2 || x >= WORLD_W - 2 || y < 2 || y >= WORLD_H - 2 {
        return Ground::Water;
    }
    if x < 5 || x >= WORLD_W - 5 || y < 5 || y >= WORLD_H - 5 {
        if smooth_noise(x, y, 3) > 0.5 {
            return Ground::Water;
        }
    }

    let n1 = smooth_noise(x, y, 12);
    let n2 = smooth_noise(x + 50, y + 50, 8);

    // River
    let river_x = 50.0 + 15.0 * (y as f32 * 0.08).sin();
    if (x as f32 - river_x).abs() < 2.0 {
        return Ground::Water;
    }

    // Biome regions
    let fx = x as f32 / WORLD_W as f32;
    let fy = y as f32 / WORLD_H as f32;

    // Top-right: mountains
    if fx > 0.65 && fy < 0.3 {
        if n1 > 0.4 { return Ground::Mountain; }
        if n2 > 0.6 { return Ground::Snow; }
        return Ground::Mountain;
    }

    // Bottom-left: swamp
    if fx < 0.3 && fy > 0.65 {
        if n1 > 0.5 { return Ground::Water; }
        return Ground::Swamp;
    }

    // Bottom-right: desert
    if fx > 0.65 && fy > 0.65 {
        if n1 > 0.7 { return Ground::Lava; }
        return Ground::Sand;
    }

    Ground::Grass
}

// ── Overlay Generation ────────────────────────────────

fn overlay_at(x: usize, y: usize, ground: Ground) -> Option<Overlay> {
    // Only place trees on grass
    if ground != Ground::Grass {
        return None;
    }

    let n = smooth_noise(x + 200, y + 200, 6);
    let n2 = smooth_noise(x + 300, y + 300, 3);
    let fx = x as f32 / WORLD_W as f32;
    let fy = y as f32 / WORLD_H as f32;

    // Top-left forest region: dense trees
    if fx < 0.4 && fy < 0.4 {
        if n > 0.35 {
            return Some(Overlay::Tree);
        }
    }

    // Scattered trees elsewhere
    if n > 0.7 && n2 > 0.5 {
        return Some(Overlay::Tree);
    }

    None
}

fn carve_road(cells: &mut [Vec<Cell>], x1: usize, y1: usize, x2: usize, y2: usize) {
    let steps = ((x2 as i32 - x1 as i32).abs().max((y2 as i32 - y1 as i32).abs()) * 2) as usize;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = (x1 as f32 + (x2 as f32 - x1 as f32) * t).round() as usize;
        let y = (y1 as f32 + (y2 as f32 - y1 as f32) * t).round() as usize;
        if x < WORLD_W && y < WORLD_H {
            cells[y][x].ground = Ground::Road;
            cells[y][x].overlay = None; // clear trees from road
        }
    }
}

fn place_village(cells: &mut [Vec<Cell>], cx: usize, cy: usize) {
    for dy in 0..3 {
        for dx in 0..3 {
            let x = cx + dx;
            let y = cy + dy;
            if x < WORLD_W && y < WORLD_H {
                cells[y][x].ground = Ground::Grass;
                cells[y][x].overlay = Some(Overlay::Village);
            }
        }
    }
}

// ── Noise ─────────────────────────────────────────────

fn smooth_noise(x: usize, y: usize, scale: usize) -> f32 {
    let sx = x as f32 / scale as f32;
    let sy = y as f32 / scale as f32;
    let ix = sx.floor() as i32;
    let iy = sy.floor() as i32;
    let fx = sx - ix as f32;
    let fy = sy - iy as f32;

    let a = (hash(ix, iy) % 1000) as f32 / 1000.0;
    let b = (hash(ix + 1, iy) % 1000) as f32 / 1000.0;
    let c = (hash(ix, iy + 1) % 1000) as f32 / 1000.0;
    let d = (hash(ix + 1, iy + 1) % 1000) as f32 / 1000.0;

    let top = a + (b - a) * fx;
    let bot = c + (d - c) * fx;
    top + (bot - top) * fy
}

fn hash(x: i32, y: i32) -> u32 {
    let mut h = (x.wrapping_mul(374761393).wrapping_add(y.wrapping_mul(668265263))) as u32;
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    h ^ (h >> 16)
}
