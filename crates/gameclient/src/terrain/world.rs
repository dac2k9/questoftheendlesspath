use bevy::prelude::*;

use super::{Ground, Overlay, Terrain};

pub const WORLD_W: usize = 100;
pub const WORLD_H: usize = 80;
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
}

impl WorldGrid {
    pub fn generate() -> Self {
        let mut cells = vec![
            vec![Cell { ground: Ground::Grass, overlay: None }; WORLD_W];
            WORLD_H
        ];

        // Roads
        carve_road(&mut cells, 10, 40, 90, 40);
        carve_road(&mut cells, 50, 10, 50, 70);
        carve_road(&mut cells, 20, 20, 80, 60);

        // Trees (scattered, denser in some areas)
        for y in 0..WORLD_H {
            for x in 0..WORLD_W {
                if cells[y][x].ground != Ground::Grass { continue; }
                let n = smooth_noise(x + 200, y + 200, 6);
                let n2 = smooth_noise(x + 300, y + 300, 3);
                let fx = x as f32 / WORLD_W as f32;
                let fy = y as f32 / WORLD_H as f32;

                if fx < 0.4 && fy < 0.4 && n > 0.35 {
                    cells[y][x].overlay = Some(Overlay::Tree);
                } else if fx > 0.6 && fy < 0.3 && n > 0.4 {
                    cells[y][x].overlay = Some(Overlay::PineTree);
                } else if n > 0.72 && n2 > 0.5 {
                    cells[y][x].overlay = Some(Overlay::Tree);
                }
            }
        }

        // Villages
        place_village(&mut cells, 50, 40);
        place_village(&mut cells, 25, 25);
        place_village(&mut cells, 75, 55);

        WorldGrid { cells, width: WORLD_W, height: WORLD_H }
    }

    pub fn get(&self, x: usize, y: usize) -> Terrain {
        if x < self.width && y < self.height {
            self.cells[y][x].terrain()
        } else {
            Terrain { ground: Ground::Grass, overlay: None }
        }
    }

    pub fn get_ground(&self, x: usize, y: usize) -> Ground {
        if x < self.width && y < self.height { self.cells[y][x].ground } else { Ground::Grass }
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

fn carve_road(cells: &mut [Vec<Cell>], x1: usize, y1: usize, x2: usize, y2: usize) {
    // Walk from (x1,y1) to (x2,y2) one step at a time.
    // For diagonal moves, place an extra tile to avoid gaps.
    let mut x = x1 as i32;
    let mut y = y1 as i32;
    let ex = x2 as i32;
    let ey = y2 as i32;

    while x != ex || y != ey {
        // Place current tile
        if (x as usize) < WORLD_W && (y as usize) < WORLD_H {
            cells[y as usize][x as usize].ground = Ground::Road;
            cells[y as usize][x as usize].overlay = None;
        }

        let dx = (ex - x).signum();
        let dy = (ey - y).signum();

        if dx != 0 && dy != 0 {
            // Diagonal step — place an extra tile to fill the corner
            // Step horizontally first, then diagonally
            let hx = (x + dx) as usize;
            let hy = y as usize;
            if hx < WORLD_W && hy < WORLD_H {
                cells[hy][hx].ground = Ground::Road;
                cells[hy][hx].overlay = None;
            }
        }

        x += dx;
        y += dy;
    }

    // Place final tile
    if (ex as usize) < WORLD_W && (ey as usize) < WORLD_H {
        cells[ey as usize][ex as usize].ground = Ground::Road;
        cells[ey as usize][ex as usize].overlay = None;
    }
}

fn place_village(cells: &mut [Vec<Cell>], cx: usize, cy: usize) {
    for dy in 0..3 {
        for dx in 0..3 {
            let x = cx + dx;
            let y = cy + dy;
            if x < WORLD_W && y < WORLD_H {
                cells[y][x].ground = Ground::Road;
                cells[y][x].overlay = Some(Overlay::Village);
            }
        }
    }
}

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
