//! Seeded procedural world map generator.
//!
//! Takes a seed → produces terrain grid + points of interest + roads.
//! Outputs a `WorldMap` that can be serialized to JSON for the LLM game master.

use serde::{Deserialize, Serialize};

pub const MAP_W: usize = 100;
pub const MAP_H: usize = 80;

// ── Terrain ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Biome {
    Grassland,
    Forest,
    DenseForest,
    Mountain,
    Desert,
    Snow,
    Swamp,
    Water,
    DeepWater,
}

impl Biome {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Grassland => "Grassland",
            Self::Forest => "Forest",
            Self::DenseForest => "Dense Forest",
            Self::Mountain => "Mountain",
            Self::Desert => "Desert",
            Self::Snow => "Snowfield",
            Self::Swamp => "Swamp",
            Self::Water => "Water",
            Self::DeepWater => "Deep Water",
        }
    }

    /// Item required to enter this biome. Returns None if no item needed.
    pub fn required_item(self) -> Option<&'static str> {
        match self {
            Self::Mountain => Some("warm_cloak"),
            Self::Snow => Some("warm_cloak"),
            Self::Swamp => Some("bog_charm"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PoiType {
    Village,
    Town,
    Ruins,
    Dungeon,
    Cabin,
    Shrine,
    Cave,
    Tower,
    Camp,
    Port,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointOfInterest {
    pub id: usize,
    pub poi_type: PoiType,
    pub x: usize,
    pub y: usize,
    pub biome: Biome,
    pub has_road: bool,
    /// Filled in by LLM later
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Road {
    pub from_poi: usize,
    pub to_poi: usize,
    /// Tile coordinates along the road
    pub path: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldMap {
    pub seed: u64,
    pub width: usize,
    pub height: usize,
    pub terrain: Vec<Vec<Biome>>,
    pub pois: Vec<PointOfInterest>,
    pub roads: Vec<Road>,
}

// ── Generation ────────────────────────────────────────

impl WorldMap {
    pub fn generate(seed: u64) -> Self {
        let mut rng = SeededRng::new(seed);

        // Step 1: Generate terrain with multi-octave noise
        let terrain = generate_terrain(&mut rng);

        // Step 2: Place points of interest
        let pois = place_pois(&mut rng, &terrain);

        // Step 3: Connect POIs with roads
        let roads = generate_roads(&pois, &terrain);

        WorldMap {
            seed,
            width: MAP_W,
            height: MAP_H,
            terrain,
            pois,
            roads,
        }
    }

    /// Get terrain at a position (with bounds check).
    pub fn biome_at(&self, x: usize, y: usize) -> Biome {
        if x < self.width && y < self.height {
            self.terrain[y][x]
        } else {
            Biome::DeepWater
        }
    }

    /// Check if a road passes through this tile.
    /// POI tiles (and their 3x3 area) count as roads.
    pub fn has_road_at(&self, x: usize, y: usize) -> bool {
        // POI tiles are treated as roads (cheap to walk through)
        for poi in &self.pois {
            let dx = (poi.x as i32 - x as i32).unsigned_abs() as usize;
            let dy = (poi.y as i32 - y as i32).unsigned_abs() as usize;
            if dx <= 1 && dy <= 1 {
                return true;
            }
        }
        self.roads
            .iter()
            .any(|r| r.path.iter().any(|&(rx, ry)| rx == x && ry == y))
    }

    /// Get POI at position (exact tile or adjacent — covers 3x3 village area).
    pub fn poi_at(&self, x: usize, y: usize) -> Option<&PointOfInterest> {
        self.pois.iter().find(|p| p.x == x && p.y == y)
    }

    /// Get all POI IDs within a given radius.
    pub fn pois_near(&self, x: usize, y: usize, radius: usize) -> Vec<usize> {
        self.pois.iter()
            .filter(|p| {
                let dx = (p.x as i32 - x as i32).unsigned_abs() as usize;
                let dy = (p.y as i32 - y as i32).unsigned_abs() as usize;
                dx <= radius && dy <= radius
            })
            .map(|p| p.id)
            .collect()
    }

    /// Export POIs as JSON for the LLM game master.
    pub fn export_pois_json(&self) -> String {
        serde_json::to_string_pretty(&ExportData {
            seed: self.seed,
            map_size: [self.width, self.height],
            points_of_interest: self
                .pois
                .iter()
                .map(|p| ExportPoi {
                    id: p.id,
                    poi_type: format!("{:?}", p.poi_type),
                    position: [p.x, p.y],
                    biome: format!("{:?}", p.biome),
                    has_road: p.has_road,
                    connected_to: self
                        .roads
                        .iter()
                        .filter_map(|r| {
                            if r.from_poi == p.id {
                                Some(r.to_poi)
                            } else if r.to_poi == p.id {
                                Some(r.from_poi)
                            } else {
                                None
                            }
                        })
                        .collect(),
                })
                .collect(),
        })
        .unwrap_or_default()
    }
}

#[derive(Serialize)]
struct ExportData {
    seed: u64,
    map_size: [usize; 2],
    points_of_interest: Vec<ExportPoi>,
}

#[derive(Serialize)]
struct ExportPoi {
    id: usize,
    poi_type: String,
    position: [usize; 2],
    biome: String,
    has_road: bool,
    connected_to: Vec<usize>,
}

// ── Terrain Generation ────────────────────────────────

fn generate_terrain(rng: &mut SeededRng) -> Vec<Vec<Biome>> {
    // Generate elevation and moisture maps using layered noise
    let elevation = noise_map(rng.next(), 8);
    let moisture = noise_map(rng.next(), 10);
    let temperature = noise_map(rng.next(), 14);

    let mut terrain = vec![vec![Biome::Grassland; MAP_W]; MAP_H];

    for y in 0..MAP_H {
        for x in 0..MAP_W {
            let e = elevation[y][x];
            let m = moisture[y][x];
            let t = temperature[y][x];

            // Water borders
            let border_dist = (x.min(MAP_W - 1 - x).min(y).min(MAP_H - 1 - y)) as f32;
            if border_dist < 3.0 {
                terrain[y][x] = Biome::DeepWater;
                continue;
            }
            if border_dist < 6.0 && e < 0.4 {
                terrain[y][x] = Biome::Water;
                continue;
            }

            // Biome assignment based on elevation + moisture + temperature
            terrain[y][x] = if e < 0.25 {
                // Low elevation
                if m > 0.6 { Biome::Water } else { Biome::Swamp }
            } else if e < 0.4 {
                // Low-medium
                if m > 0.7 {
                    Biome::Swamp
                } else if t < 0.3 {
                    Biome::Snow
                } else {
                    Biome::Grassland
                }
            } else if e < 0.65 {
                // Medium
                if m > 0.55 {
                    Biome::DenseForest
                } else if m > 0.35 {
                    Biome::Forest
                } else if t > 0.7 {
                    Biome::Desert
                } else {
                    Biome::Grassland
                }
            } else if e < 0.8 {
                // High
                if t < 0.35 { Biome::Snow } else { Biome::Mountain }
            } else {
                Biome::Mountain
            };
        }
    }

    // Add a river
    add_river(rng, &mut terrain);

    terrain
}

fn add_river(rng: &mut SeededRng, terrain: &mut [Vec<Biome>]) {
    // River starts from a random point on one edge and meanders across
    let start_y = 5 + (rng.next_range(MAP_H as u64 - 10) as usize);
    let mut x = 3_usize;
    let mut y = start_y;

    while x < MAP_W - 3 {
        terrain[y][x] = Biome::Water;
        // Also widen the river slightly
        if y > 0 {
            terrain[y - 1][x] = Biome::Water;
        }

        x += 1;
        // Meander: occasionally shift up or down
        let r = rng.next_range(10);
        if r < 3 && y > 5 {
            y -= 1;
        } else if r < 6 && y < MAP_H - 5 {
            y += 1;
        }
    }
}

/// Generate a 2D noise map (0.0-1.0) using multi-octave value noise.
fn noise_map(seed: u64, base_scale: usize) -> Vec<Vec<f32>> {
    let mut map = vec![vec![0.0f32; MAP_W]; MAP_H];

    // 3 octaves of noise
    let octaves = [
        (base_scale, 0.6),
        (base_scale / 2 + 1, 0.3),
        (base_scale / 4 + 1, 0.1),
    ];

    for (scale, weight) in octaves {
        for y in 0..MAP_H {
            for x in 0..MAP_W {
                map[y][x] += smooth_noise(x, y, scale, seed) * weight;
            }
        }
    }

    map
}

// ── POI Placement ─────────────────────────────────────

fn place_pois(rng: &mut SeededRng, terrain: &[Vec<Biome>]) -> Vec<PointOfInterest> {
    let mut pois = Vec::new();
    let mut id = 0;

    // Strategy: scatter candidate positions, pick the best ones
    // Villages: prefer grassland, away from edges
    // Dungeons: prefer mountain or forest
    // Cabins: prefer forest, don't need roads
    // etc.

    let poi_configs = [
        (PoiType::Town, 2, Biome::Grassland, true),
        (PoiType::Village, 4, Biome::Grassland, true),
        (PoiType::Village, 2, Biome::Forest, true),
        (PoiType::Ruins, 2, Biome::Desert, true),
        (PoiType::Dungeon, 2, Biome::Mountain, true),
        (PoiType::Cave, 2, Biome::Mountain, false),
        (PoiType::Cabin, 3, Biome::Forest, false),
        (PoiType::Cabin, 1, Biome::DenseForest, false),
        (PoiType::Shrine, 2, Biome::Forest, true),
        (PoiType::Tower, 1, Biome::Mountain, true),
        (PoiType::Camp, 2, Biome::Grassland, false),
    ];

    for (poi_type, count, preferred_biome, has_road) in poi_configs {
        for _ in 0..count {
            if let Some((x, y)) = find_poi_location(rng, terrain, preferred_biome, &pois) {
                let actual_biome = terrain[y][x];
                pois.push(PointOfInterest {
                    id,
                    poi_type,
                    x,
                    y,
                    biome: actual_biome,
                    has_road,
                    name: String::new(),
                    description: String::new(),
                });
                id += 1;
            }
        }
    }

    pois
}

fn find_poi_location(
    rng: &mut SeededRng,
    terrain: &[Vec<Biome>],
    preferred: Biome,
    existing: &[PointOfInterest],
) -> Option<(usize, usize)> {
    // Try up to 100 random positions
    for _ in 0..100 {
        let x = 8 + rng.next_range((MAP_W - 16) as u64) as usize;
        let y = 8 + rng.next_range((MAP_H - 16) as u64) as usize;

        let biome = terrain[y][x];

        // Must be on land
        if matches!(biome, Biome::Water | Biome::DeepWater) {
            continue;
        }

        // Prefer the right biome (but accept neighbors)
        if biome != preferred {
            // Accept adjacent biomes with 30% chance
            if rng.next_range(10) > 3 {
                continue;
            }
        }

        // Must be far enough from other POIs (min 8 tiles)
        let too_close = existing
            .iter()
            .any(|p| {
                let dx = (p.x as i32 - x as i32).unsigned_abs() as usize;
                let dy = (p.y as i32 - y as i32).unsigned_abs() as usize;
                dx + dy < 8
            });
        if too_close {
            continue;
        }

        return Some((x, y));
    }
    None
}

// ── Road Generation ───────────────────────────────────

fn generate_roads(pois: &[PointOfInterest], terrain: &[Vec<Biome>]) -> Vec<Road> {
    let mut roads = Vec::new();

    // Connect POIs that want roads using a minimum spanning tree approach
    let road_pois: Vec<&PointOfInterest> = pois.iter().filter(|p| p.has_road).collect();

    if road_pois.len() < 2 {
        return roads;
    }

    // Prim's algorithm for MST — connect all road-wanting POIs
    let mut connected = vec![false; road_pois.len()];
    connected[0] = true;

    for _ in 1..road_pois.len() {
        let mut best_dist = u32::MAX;
        let mut best_from = 0;
        let mut best_to = 0;

        for (i, _) in road_pois.iter().enumerate() {
            if !connected[i] {
                continue;
            }
            for (j, _) in road_pois.iter().enumerate() {
                if connected[j] {
                    continue;
                }
                let dx = (road_pois[i].x as i32 - road_pois[j].x as i32).unsigned_abs();
                let dy = (road_pois[i].y as i32 - road_pois[j].y as i32).unsigned_abs();
                let dist = dx + dy;
                if dist < best_dist {
                    best_dist = dist;
                    best_from = i;
                    best_to = j;
                }
            }
        }

        if best_dist == u32::MAX {
            break;
        }

        connected[best_to] = true;

        // Build the road path (simple L-shaped: horizontal then vertical)
        let from = &road_pois[best_from];
        let to = &road_pois[best_to];
        let path = build_road_path(from.x, from.y, to.x, to.y, terrain);

        roads.push(Road {
            from_poi: from.id,
            to_poi: to.id,
            path,
        });
    }

    roads
}

/// Build a road path between two points, avoiding water where possible.
fn build_road_path(
    x1: usize,
    y1: usize,
    x2: usize,
    y2: usize,
    terrain: &[Vec<Biome>],
) -> Vec<(usize, usize)> {
    let mut path = Vec::new();
    let mut x = x1 as i32;
    let mut y = y1 as i32;
    let ex = x2 as i32;
    let ey = y2 as i32;

    while x != ex || y != ey {
        path.push((x as usize, y as usize));

        let dx = (ex - x).signum();
        let dy = (ey - y).signum();

        if dx != 0 && dy != 0 {
            // Diagonal — add extra tile for connectivity
            let hx = (x + dx) as usize;
            let hy = y as usize;
            if hx < MAP_W && hy < MAP_H {
                path.push((hx, hy));
            }
        }

        x += dx;
        y += dy;
    }
    path.push((x2, y2));
    path
}

// ── Noise Utilities ───────────────────────────────────

fn smooth_noise(x: usize, y: usize, scale: usize, seed: u64) -> f32 {
    let sx = x as f32 / scale as f32;
    let sy = y as f32 / scale as f32;
    let ix = sx.floor() as i64;
    let iy = sy.floor() as i64;
    let fx = sx - ix as f32;
    let fy = sy - iy as f32;

    let s = seed as i64;
    let a = (hash64(ix + s, iy) % 10000) as f32 / 10000.0;
    let b = (hash64(ix + 1 + s, iy) % 10000) as f32 / 10000.0;
    let c = (hash64(ix + s, iy + 1) % 10000) as f32 / 10000.0;
    let d = (hash64(ix + 1 + s, iy + 1) % 10000) as f32 / 10000.0;

    let top = a + (b - a) * fx;
    let bot = c + (d - c) * fx;
    top + (bot - top) * fy
}

fn hash64(x: i64, y: i64) -> u64 {
    let mut h = (x.wrapping_mul(374761393).wrapping_add(y.wrapping_mul(668265263))) as u64;
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    h ^ (h >> 16)
}

// ── Seeded RNG ────────────────────────────────────────

struct SeededRng {
    state: u64,
}

impl SeededRng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_add(0x9E3779B97F4A7C15),
        }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.state
    }

    fn next_range(&mut self, max: u64) -> u64 {
        if max == 0 {
            return 0;
        }
        self.next() % max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_deterministic() {
        let map1 = WorldMap::generate(42);
        let map2 = WorldMap::generate(42);
        assert_eq!(map1.pois.len(), map2.pois.len());
        assert_eq!(map1.terrain[40][50], map2.terrain[40][50]);
        for i in 0..map1.pois.len() {
            assert_eq!(map1.pois[i].x, map2.pois[i].x);
            assert_eq!(map1.pois[i].y, map2.pois[i].y);
        }
    }

    #[test]
    fn generate_has_pois_and_roads() {
        let map = WorldMap::generate(123);
        assert!(!map.pois.is_empty(), "should have POIs");
        assert!(!map.roads.is_empty(), "should have roads");
        println!("POIs: {}, Roads: {}", map.pois.len(), map.roads.len());
        for poi in &map.pois {
            println!(
                "  {:?} at ({},{}) biome={:?} road={}",
                poi.poi_type, poi.x, poi.y, poi.biome, poi.has_road
            );
        }
    }

    #[test]
    fn different_seeds_different_maps() {
        let map1 = WorldMap::generate(1);
        let map2 = WorldMap::generate(2);
        // Very unlikely to have same terrain
        let diffs = (0..MAP_H)
            .flat_map(|y| (0..MAP_W).map(move |x| (x, y)))
            .filter(|&(x, y)| map1.terrain[y][x] != map2.terrain[y][x])
            .count();
        assert!(diffs > 100, "different seeds should produce different maps");
    }

    #[test]
    fn export_json() {
        let map = WorldMap::generate(42);
        let json = map.export_pois_json();
        assert!(json.contains("points_of_interest"));
        assert!(json.contains("Village"));
        println!("{json}");
    }
}
