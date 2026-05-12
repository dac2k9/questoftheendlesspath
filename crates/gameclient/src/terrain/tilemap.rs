use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use super::world::{WorldGrid, world_w, world_h, TILE_PX};
use super::path::{DisplayRoute, InterpolationState, find_path_with_items, position_and_index_from_route_meters, position_from_route_meters, tile_index_from_meters};
use crate::states::AppState;
use crate::supabase::{self, PolledPlayerState, SupabaseConfig};
use crate::{GameFont, GameSession};

/// Per-champion sprite sheet metadata. Sheets use inconsistent layouts
/// (some padded like Katan, most plain grids). Frame size is always 16×16.
///
/// `facing_rows` / `facing_flip` map facing → atlas row + x-flip, in order
/// [Down, Up, Right, Left]. Most packs have a dedicated Left row (default).
/// Others (Zhinja) only supply Right and use flip_x for Left.
pub struct ChampionInfo {
    pub bytes: &'static [u8],
    pub cols: u32,
    pub rows: u32,
    pub padded: bool,
    pub facing_rows: [usize; 4],
    pub facing_flip: [bool; 4],
}

/// Look up sprite-sheet metadata for a champion. Unknown names fall back to Katan.
pub fn champion_info(name: &str) -> ChampionInfo {
    // Default: Down=row 0, Up=row 1, Right=row 2, Left=row 3. No flips.
    const DEFAULT_ROWS: [usize; 4] = [0, 1, 2, 3];
    const NO_FLIP: [bool; 4] = [false; 4];
    // Zhinja: row 0 is a side view facing LEFT (not Right as first guessed).
    // Down=row 1, Up=row 2. Right reuses row 0 with horizontal flip.
    const ZHINJA_ROWS: [usize; 4] = [1, 2, 0, 0];
    const ZHINJA_FLIP: [bool; 4] = [false, false, true, false];
    match name {
        "Zhinja"    => ChampionInfo { bytes: include_bytes!("../../assets/sprites/Zhinja.png"),    cols: 6, rows: 9,  padded: true,  facing_rows: ZHINJA_ROWS, facing_flip: ZHINJA_FLIP },
        "Arthax"    => ChampionInfo { bytes: include_bytes!("../../assets/sprites/Arthax.png"),    cols: 5, rows: 8,  padded: false, facing_rows: DEFAULT_ROWS, facing_flip: NO_FLIP },
        "Börg"      => ChampionInfo { bytes: include_bytes!("../../assets/sprites/Börg.png"),      cols: 6, rows: 8,  padded: false, facing_rows: DEFAULT_ROWS, facing_flip: NO_FLIP },
        "Gangblanc" => ChampionInfo { bytes: include_bytes!("../../assets/sprites/Gangblanc.png"), cols: 8, rows: 8,  padded: false, facing_rows: DEFAULT_ROWS, facing_flip: NO_FLIP },
        "Grum"      => ChampionInfo { bytes: include_bytes!("../../assets/sprites/Grum.png"),      cols: 5, rows: 9,  padded: false, facing_rows: DEFAULT_ROWS, facing_flip: NO_FLIP },
        "Kanji"     => ChampionInfo { bytes: include_bytes!("../../assets/sprites/Kanji.png"),     cols: 6, rows: 8,  padded: false, facing_rows: DEFAULT_ROWS, facing_flip: NO_FLIP },
        "Okomo"     => ChampionInfo { bytes: include_bytes!("../../assets/sprites/Okomo.png"),     cols: 5, rows: 12, padded: false, facing_rows: DEFAULT_ROWS, facing_flip: NO_FLIP },
        _           => ChampionInfo { bytes: include_bytes!("../../assets/sprites/Katan.png"),     cols: 6, rows: 8,  padded: true,  facing_rows: DEFAULT_ROWS, facing_flip: NO_FLIP },
    }
}

/// Index of a facing direction (0=Down, 1=Up, 2=Right, 3=Left).
fn facing_idx(facing: Facing) -> usize {
    match facing { Facing::Down => 0, Facing::Up => 1, Facing::Right => 2, Facing::Left => 3 }
}

/// Build the TextureAtlasLayout matching a champion sheet.
pub fn champion_atlas_layout(info: &ChampionInfo) -> TextureAtlasLayout {
    if info.padded {
        TextureAtlasLayout::from_grid(
            UVec2::new(16, 16), info.cols, info.rows,
            Some(UVec2::new(2, 2)), Some(UVec2::new(1, 1)),
        )
    } else {
        TextureAtlasLayout::from_grid(UVec2::new(16, 16), info.cols, info.rows, None, None)
    }
}

/// Backwards-compatible wrapper: raw bytes only. Prefer `champion_info` when
/// you also need the atlas layout.
pub fn champion_bytes(name: &str) -> &'static [u8] {
    champion_info(name).bytes
}

pub struct TilemapPlugin;

impl Plugin for TilemapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::InGame), spawn_world)
            .add_systems(
                Update,
                (
                    apply_server_state,
                    interpolate_movement,
                    render_character,
                ).chain().run_if(in_state(AppState::InGame)),
            )
            .add_systems(
                Update,
                (
                    handle_map_click,
                    handle_clear_route,
                    handle_pan,
                ).run_if(in_state(AppState::InGame)
                    .and(not(crate::combat::combat_active))
                    .and(not(crate::dialogue::dialogue_active))
                    .and(not(crate::dialogue::shop_active))
                    .and(not(crate::dialogue::forge_active))),
            )
            .add_systems(
                Update,
                (
                    handle_zoom,
                    toggle_poi_labels,
                    update_fog_texture,
                    update_camera,
                    handle_debug_menu,
                    update_chest_sprites,
                    animate_monsters,
                    update_other_players,
                    poll_shops,
                    toggle_shop_labels,
                ).run_if(in_state(AppState::InGame)),
            );
    }
}

// ── Components ────────────────────────────────────────

#[derive(Component)]
pub struct MapSprite;

#[derive(Component)]
pub struct FogSprite;

#[derive(Component)]
struct PathMarker;

#[derive(Component)]
pub struct PlayerSprite;

#[derive(Component)]
struct OtherPlayerSprite {
    id: String,
    /// Unrounded sub-pixel position the lerp operates on. Only the
    /// rounded value is written to the transform — without this, the
    /// in-place rounding eats fractional progress each frame and the
    /// sprite freezes ~7 px shy of a static target (most visible when
    /// another player stops mid-tile from your perspective).
    visual_pos: Vec2,
}

#[derive(Component)]
struct OtherPlayerAnim {
    timer: Timer,
    frame: usize,
    moving: bool,
    cols: u32,
    facing_rows: [usize; 4],
    facing_flip: [bool; 4],
}

#[derive(Component)]
struct OtherPlayerName(String); // player_id

#[derive(Component)]
struct ChestSprite(usize); // chest index

#[derive(Component)]
struct MonsterSprite(usize); // monster index

#[derive(Component)]
struct MonsterAnimation {
    timer: Timer,
    frame: usize,
    cols: usize, // number of columns in this sprite sheet
    /// Monster difficulty — stronger monsters fidget faster (more menacing).
    difficulty: u32,
}

#[derive(Component)]
struct TileInfoText;

#[derive(Component)]
pub struct PoiLabel;

/// Custom landmark sprite placed over POIs whose type has a matching
/// PNG in assets/poi/. Kept as a marker component in case we later want
/// to fade them in/out, scale them by zoom, etc.
#[derive(Component)]
pub struct PoiCustomSprite;

/// Maps a POI type to its custom landmark asset: `(asset_path, tile_size)`.
/// tile_size = 1 → sprite fits one tile (default, small landmarks).
/// tile_size = 3 → sprite spans 3×3 tiles (large iconic landmarks like
/// castles or fortresses). The world.rs pass clears overlays in a 3×3
/// area around each POI, so tile_size up to 3 renders cleanly; larger
/// would require widening that pass.
///
/// Add new entries here when new PNGs are dropped in `assets/poi/`.
pub fn poi_sprite_path(ty: questlib::mapgen::PoiType) -> Option<(&'static str, u32)> {
    use questlib::mapgen::PoiType::*;
    match ty {
        Town    => Some(("poi/town.png", 1)),
        Village => Some(("poi/village.png", 1)),
        Cave    => Some(("poi/cave_entrance.png", 1)),
        Cabin   => Some(("poi/cabin.png", 1)),
        Shrine  => Some(("poi/witch_hut.png", 1)),
        Ruins   => Some(("poi/ruins.png", 1)),
        Dungeon => Some(("poi/dungeon.png", 1)),
        Camp    => Some(("poi/camp.png", 1)),
        Tower   => Some(("poi/tower.png", 1)),
        // Authored chaos-adventure landmarks. Castles render 3×3
        // (iconic boss locations); travel gates render 1×1 since
        // they slot in alongside other small POIs.
        CastleFrost  => Some(("generated/poi/castle_frost.png", 3)),
        CastleFlame  => Some(("generated/poi/castle_flame.png", 3)),
        CastleShadow => Some(("generated/poi/castle_shadow.png", 3)),
        CastleStorm  => Some(("generated/poi/castle_storm.png", 3)),
        TravelGate   => Some(("generated/poi/travel_gate.png", 1)),
        RefugeeCamp  => Some(("generated/poi/refugee_camp.png", 2)),
        WizardsSpire => Some(("generated/poi/wizards_spire.png", 2)),
        // Port still has no custom art; falls back to the tile-atlas
        // overlay (Overlay::Village). Drop a PNG into assets/poi/ +
        // add a branch here when art arrives.
        _       => None,
    }
}

/// A shop marker — spawned once per shop in the player's revealed_shops.
/// `String` is the shop's event id so we can avoid duplicate spawns
/// across refetches.
#[derive(Component)]
struct ShopLabel(String);

#[derive(Component)]
struct PlayerNameTag;

#[derive(Component)]
struct DebugMenuUi;

#[derive(Component)]
struct LoadingText;

use questlib::route::Facing;

/// Legacy fixed row mapping — kept only for any outside callers. Prefer
/// `facing_idx(facing)` into the per-champion `facing_rows` array.
fn facing_base_row(facing: Facing) -> usize {
    match facing {
        Facing::Down => 0,
        Facing::Up => 1,
        Facing::Right => 2,
        Facing::Left => 3,
    }
}

#[derive(Component)]
struct WalkAnimation {
    timer: Timer,
    frame: usize,
    facing: Facing,
    moving: bool,
    /// Columns in the atlas — used to compute `row * cols + frame` since
    /// different sprite sheets have different widths (5, 6, or 8 cols).
    cols: u32,
    /// Atlas row per facing [Down, Up, Right, Left]. Varies by sprite pack.
    facing_rows: [usize; 4],
    /// Whether to set sprite.flip_x for each facing [Down, Up, Right, Left].
    /// Used when a sheet has no Left row and reuses Right mirrored.
    facing_flip: [bool; 4],
}

// ── Resources ─────────────────────────────────────────

/// Authoritative player state from server. Updated every poll.
#[derive(Resource, Default)]
pub struct MyPlayerState {
    pub tile_x: i32,
    pub tile_y: i32,
    pub route: Vec<(usize, usize)>,
    pub route_meters: f64,
    pub speed_kmh: f32,
    pub is_walking: bool,
    pub gold: i32,
    pub revealed_tiles: String,
    pub facing: questlib::route::Facing,
    pub total_distance_m: f64,
    pub initialized: bool,
    pub last_poll_tile: (i32, i32),
    pub inventory: Vec<questlib::items::InventorySlot>,
    pub equipment: questlib::items::EquipmentLoadout,
    pub opened_chests: Vec<String>,
    pub defeated_monsters: Vec<String>,
    /// None = overworld; Some(id) = inside that interior.
    pub location: Option<String>,
    /// Events this player has personally completed. Mirrors the server-side
    /// `DevPlayerState.completed_events`. Used for portal-unlock visuals.
    pub completed_events: Vec<String>,
    /// Per-item forge upgrade level (0..=5). Mirrors server-side
    /// DevPlayerState.item_upgrades. Used by the Forge UI to show current
    /// level and next-tier cost per equipped item.
    pub item_upgrades: std::collections::HashMap<String, u8>,
    /// Permanent meta-progression boons earned across adventures.
    pub boons: Vec<String>,
    /// When `Some`, the boon picker modal opens and the player must
    /// pick one of `choices` before continuing. Cleared by the server
    /// after `/select_boon`.
    pub pending_boon_choice: Option<crate::supabase::PendingBoonChoice>,
    /// Temporary buffs from consumed potions. Mirrors server-side
    /// DevPlayerState.active_buffs. The boon HUD shows each as a
    /// chip with a time-remaining tooltip.
    pub active_buffs: Vec<questlib::items::ActiveBuff>,
    /// Which adventure this player is in (mirrors server-side
    /// DevPlayerState.adventure_id). Used by the adventure-menu UI
    /// to label the current vs other adventures.
    pub adventure_id: String,
}

/// Smoothly interpolated visual state, decoupled from server state.
#[derive(Resource)]
struct VisualState {
    pos: Vec2,
    initialized: bool,
    had_route: bool,
}

impl Default for VisualState {
    fn default() -> Self { Self { pos: Vec2::ZERO, initialized: false, had_route: false } }
}

#[derive(Resource, Default)]
struct CameraPan { active: bool, last_pos: Option<Vec2> }

#[derive(Resource)]
pub struct DebugOptions {
    pub show_menu: bool,
    pub fog_disabled: bool,
    pub show_pois: bool,
    pub lighting_enabled: bool,
    pub water_shader_enabled: bool,
    /// When true, the water shader's sun direction follows the cursor
    /// (XY) and mouse wheel (Z). For visualizing how normals interact
    /// with sun position. F8 toggles.
    pub debug_sun_enabled: bool,
    /// Debug sun direction (x, y, z). Updated each frame when
    /// debug_sun_enabled; otherwise the default "sun from upper-left".
    pub debug_sun_x: f32,
    pub debug_sun_y: f32,
    pub debug_sun_z: f32,
    /// When true, the water shader renders normals as RGB instead of
    /// running Phong. Classic normal-map visualization: flat surfaces
    /// appear as purplish blue (0.5, 0.5, 1.0), tilted surfaces take
    /// on different hues showing the normal direction.
    pub debug_show_normals: bool,
    /// When true, the terrain lighting overlay renders the raw
    /// heightmap as grayscale (water = black, mountain = white) to
    /// verify the biome→height table and blur. F10 toggles.
    pub debug_show_heightmap: bool,
    /// Multiplier on the heightmap gradient used to derive per-pixel
    /// normals in the terrain lighting shader. Bigger values = more
    /// dramatic slope shading. (No live binding — see fog shadow
    /// height for what PageUp/PageDown does now.)
    pub terrain_height_amp: f32,
    /// Effective "height" of the fog wall in world pixels — drives the
    /// length of fog shadows in the fog of war shader. Static (16 = 1
    /// tile); was previously tuned via PgUp/PgDn but those keys now
    /// drive `tile_z_factor` instead.
    pub fog_shadow_height_px: f32,
    /// Multiplier on per-vertex tile Z in the procedural ground mesh
    /// (PgUp/PgDn). 0.0 = flat. Each vertex's Z = max-neighbor biome
    /// height (0..1) × this factor. Pure 2D ortho only sees this as
    /// depth-order changes; meaningful when the camera tilts.
    pub tile_z_factor: f32,
    /// When true, swap the baked tile atlas for a Material2d shader
    /// that bilinearly blends per-biome flat colors at every tile
    /// boundary — no hand-crafted transition tiles. F4 toggles.
    /// Phase 1 prototype; see procedural_ground.rs.
    pub procedural_terrain_enabled: bool,
    /// When true (along with procedural_terrain_enabled), replace the
    /// world's biome map with a synthetic test grid showing the same
    /// pattern in every relevant biome combination, and switch the
    /// shader to flat-color rendering so the autotile rounding rules
    /// are visible per-pattern. F5 toggles.
    pub procedural_test_mode: bool,
}

impl Default for DebugOptions {
    fn default() -> Self {
        Self {
            show_menu: false,
            fog_disabled: false,
            show_pois: false,
            lighting_enabled: true,
            water_shader_enabled: true,
            debug_sun_enabled: false,
            debug_sun_x: 0.0,
            debug_sun_y: 0.0,
            debug_sun_z: 200.0,
            debug_show_normals: false,
            debug_show_heightmap: false,
            // Default bumped from the old hardcoded 9 so mountains
            // read more strongly out of the box.
            terrain_height_amp: 80.0,
            fog_shadow_height_px: 4.0,
            tile_z_factor: 0.5,
            procedural_terrain_enabled: true,
            procedural_test_mode: false,
        }
    }
}

#[derive(Resource)]
pub struct FogOfWar {
    pub revealed: Vec<bool>,
    pub dirty: bool,
}

impl FogOfWar {
    fn new() -> Self { Self::new_sized(world_w(), world_h()) }
    /// Allocate fog for an explicit world size. Use this from
    /// `spawn_world` to bypass any atomic-staleness race on the
    /// `world_w()/world_h()` getters.
    fn new_sized(w: usize, h: usize) -> Self {
        Self { revealed: vec![false; w * h], dirty: true }
    }
    fn reveal_around(&mut self, cx: usize, cy: usize, radius: usize) {
        let r = radius as i32;
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy > r * r { continue; }
                let x = cx as i32 + dx;
                let y = cy as i32 + dy;
                if x >= 0 && x < world_w() as i32 && y >= 0 && y < world_h() as i32 {
                    let idx = y as usize * world_w() + x as usize;
                    if !self.revealed[idx] { self.revealed[idx] = true; self.dirty = true; }
                }
            }
        }
    }
    fn is_revealed(&self, x: usize, y: usize) -> bool {
        if x < world_w() && y < world_h() { self.revealed[y * world_w() + x] } else { false }
    }
}

// ── Texture Baking (unchanged) ────────────────────────

fn bake_map_texture(world: &WorldGrid, tileset_img: &Image, tileset_cols: usize) -> Image {
    let map_w = world.width * 16;
    let map_h = world.height * 16;
    let mut pixels = vec![0u8; map_w * map_h * 4];
    let ts_w = tileset_img.width() as usize;
    let ts_data = &tileset_img.data;
    let tile_slot = 20;
    for y in 0..world.height {
        for x in 0..world.width {
            let ground = world.get_ground(x, y);
            blit_tile(&mut pixels, map_w, x * 16, y * 16, ts_data, ts_w, ground.tile_index_varied(x, y), tileset_cols, tile_slot);
            if let Some(overlay) = world.cells[y][x].overlay {
                blit_tile_alpha(&mut pixels, map_w, x * 16, y * 16, ts_data, ts_w, overlay.tile_index_varied(x, y), tileset_cols, tile_slot);
            }
        }
    }
    Image::new(Extent3d { width: map_w as u32, height: map_h as u32, depth_or_array_layers: 1 }, TextureDimension::D2, pixels, TextureFormat::Rgba8UnormSrgb, default())
}

/// Bake the ground tiles ONLY (no overlays / trees / chests / etc.).
/// The procedural ground shader samples this so its UV-jitter at tile
/// boundaries doesn't drag in tree silhouettes from neighbor tiles —
/// trees should sit *on top* of the biome, not be part of its border.
fn bake_ground_only_texture(world: &WorldGrid, tileset_img: &Image, tileset_cols: usize) -> Image {
    let map_w = world.width * 16;
    let map_h = world.height * 16;
    let mut pixels = vec![0u8; map_w * map_h * 4];
    let ts_w = tileset_img.width() as usize;
    let ts_data = &tileset_img.data;
    let tile_slot = 20;
    for y in 0..world.height {
        for x in 0..world.width {
            let ground = world.get_ground(x, y);
            blit_tile(&mut pixels, map_w, x * 16, y * 16, ts_data, ts_w, ground.tile_index_varied(x, y), tileset_cols, tile_slot);
        }
    }
    Image::new(Extent3d { width: map_w as u32, height: map_h as u32, depth_or_array_layers: 1 }, TextureDimension::D2, pixels, TextureFormat::Rgba8UnormSrgb, default())
}

/// Bake just the overlay sprites on transparent background. The
/// procedural ground composites this layer over its jittered ground
/// at the un-shifted UV — so trees stay rooted to their actual tile
/// while the ground beneath them mixes organically with neighbors.
fn bake_overlays_only_texture(world: &WorldGrid, tileset_img: &Image, tileset_cols: usize) -> Image {
    let map_w = world.width * 16;
    let map_h = world.height * 16;
    let mut pixels = vec![0u8; map_w * map_h * 4]; // alpha 0 by default
    let ts_w = tileset_img.width() as usize;
    let ts_data = &tileset_img.data;
    let tile_slot = 20;
    for y in 0..world.height {
        for x in 0..world.width {
            if let Some(overlay) = world.cells[y][x].overlay {
                blit_tile_alpha(&mut pixels, map_w, x * 16, y * 16, ts_data, ts_w, overlay.tile_index_varied(x, y), tileset_cols, tile_slot);
            }
        }
    }
    Image::new(Extent3d { width: map_w as u32, height: map_h as u32, depth_or_array_layers: 1 }, TextureDimension::D2, pixels, TextureFormat::Rgba8UnormSrgb, default())
}

fn blit_tile(dst: &mut [u8], dst_w: usize, dx: usize, dy: usize, src: &[u8], src_w: usize, tile_idx: usize, cols: usize, slot: usize) {
    let col = tile_idx % cols; let row = tile_idx / cols;
    let sx = col * slot + 2; let sy = row * slot + 2;
    for py in 0..16 { for px in 0..16 {
        let si = ((sy + py) * src_w + (sx + px)) * 4;
        let di = ((dy + py) * dst_w + (dx + px)) * 4;
        if si + 3 < src.len() && di + 3 < dst.len() { dst[di..di+4].copy_from_slice(&src[si..si+4]); }
    }}
}

fn blit_tile_alpha(dst: &mut [u8], dst_w: usize, dx: usize, dy: usize, src: &[u8], src_w: usize, tile_idx: usize, cols: usize, slot: usize) {
    let col = tile_idx % cols; let row = tile_idx / cols;
    let sx = col * slot + 2; let sy = row * slot + 2;
    for py in 0..16 { for px in 0..16 {
        let si = ((sy + py) * src_w + (sx + px)) * 4;
        let di = ((dy + py) * dst_w + (dx + px)) * 4;
        if si + 3 < src.len() && di + 3 < dst.len() && src[si + 3] > 128 { dst[di..di+4].copy_from_slice(&src[si..si+4]); }
    }}
}

/// Build the per-tile fog mask: 100×80 R8, one byte per tile
/// (0 = fogged, 255 = revealed). Sampled with linear filter on the
/// GPU so the boundary between fogged and revealed becomes a soft
/// half-tile fade on each side instead of a hard pixel edge.
fn create_fog_mask(fog: &FogOfWar, debug: &DebugOptions) -> Image {
    create_fog_mask_sized(fog, debug, world_w(), world_h())
}

/// Same as `create_fog_mask` but with explicit dims — used at spawn
/// time to avoid the atomic-getter race that pinned the fog texture
/// to 100×80 on chaos worlds. The update_fog_texture loop keeps using
/// the atomic-getter version since by that point spawn_world has
/// already pushed correct values.
fn create_fog_mask_sized(fog: &FogOfWar, debug: &DebugOptions, w: usize, h: usize) -> Image {
    let mut data = vec![0u8; w * h];
    for ty in 0..h { for tx in 0..w {
        let revealed = debug.fog_disabled || fog.is_revealed(tx, ty);
        data[ty * w + tx] = if revealed { 255 } else { 0 };
    }}
    let mut img = Image::new(
        Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::R8Unorm,
        bevy::render::render_asset::RenderAssetUsages::all(),
    );
    use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..Default::default()
    });
    img
}

// ── Spawn World ───────────────────────────────────────

fn spawn_world(
    mut commands: Commands,
    font: Res<GameFont>,
    asset_server: Res<AssetServer>,
    mut images: ResMut<Assets<Image>>,
    mut atlases: ResMut<Assets<TextureAtlasLayout>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials_fog: ResMut<Assets<super::fog_shader::FogMaterial>>,
    session: Res<GameSession>,
) {
    // Seed + dimensions come from the server's /join response (the
    // player's current adventure). Defaults make old servers and
    // sessions that joined before this plumb-through was wired still
    // work — 12345 / 100×80 reproduces the original frost_quest world.
    let seed = if session.map_seed != 0 { session.map_seed } else { 12345 };
    let width = if session.map_width != 0 {
        session.map_width as usize
    } else {
        questlib::mapgen::MAP_W
    };
    let height = if session.map_height != 0 {
        session.map_height as usize
    } else {
        questlib::mapgen::MAP_H
    };
    info!("[spawn_world] seed={} dims={}×{}", seed, width, height);
    let world = WorldGrid::from_seed_with_dims(seed, width, height);

    let tileset_bytes = include_bytes!("../../assets/tilesets/miniworld.png");
    let tileset_dyn = image::load_from_memory(tileset_bytes).expect("tileset");
    let tileset_rgba = tileset_dyn.to_rgba8();
    let (ts_w, ts_h) = tileset_rgba.dimensions();
    let tileset_img = Image::new(Extent3d { width: ts_w, height: ts_h, depth_or_array_layers: 1 }, TextureDimension::D2, tileset_rgba.into_raw(), TextureFormat::Rgba8UnormSrgb, default());

    let map_img = bake_map_texture(&world, &tileset_img, 16);
    let map_handle = images.add(map_img);
    // Use local dims (set above to drive from_seed_with_dims) so the
    // map sprite + fog mesh are never sized against stale atomics.
    let map_cx = (width as f32 * TILE_PX) / 2.0 - TILE_PX / 2.0;
    let map_cy = -(height as f32 * TILE_PX) / 2.0 + TILE_PX / 2.0;

    commands.spawn((Sprite { image: map_handle, ..default() }, Transform::from_xyz(map_cx, map_cy, 0.0), Visibility::Hidden, MapSprite));
    // Expose ground-only and overlays-only textures so procedural_ground
    // can sample biomes for jittered borders without dragging in tree
    // silhouettes, then composite the actual overlays back on top at
    // their un-shifted positions.
    let ground_only_handle = images.add(bake_ground_only_texture(&world, &tileset_img, 16));
    let overlays_only_handle = images.add(bake_overlays_only_texture(&world, &tileset_img, 16));
    commands.insert_resource(super::procedural_ground::BakedGroundTexture(ground_only_handle));
    commands.insert_resource(super::procedural_ground::BakedOverlaysTexture(overlays_only_handle));

    let fog = FogOfWar::new_sized(width, height);
    let debug = DebugOptions::default();
    let fog_mask_handle = images.add(create_fog_mask_sized(&fog, &debug, width, height));
    // Spawning fog as Mesh2d + FogMaterial replaces the 1600×1280
    // baked-pixel sprite with a tiny 100×80 mask sampled by the GPU
    // with linear filter — soft half-tile fade at every
    // revealed/unrevealed boundary.
    //
    // Source dims from the local `width` / `height` (which drove
    // `from_seed_with_dims` 30 lines up) rather than the
    // world_w()/world_h() atomics — when atomics were stale the fog
    // mesh only covered the NW quadrant of a 200×160 world, leaving
    // the SE 3/4 effectively unfogged regardless of mask state.
    let w_world = width as f32 * TILE_PX;
    let h_world = height as f32 * TILE_PX;
    // Make the fog mesh much bigger than the world so when the camera
    // zooms out beyond the world rectangle the fog still covers the
    // area (otherwise the camera's ClearColor would show through).
    // The shader force-fogs anything outside world bounds.
    const FOG_SCALE: f32 = 6.0;
    let fog_mesh = meshes.add(Rectangle::new(w_world * FOG_SCALE, h_world * FOG_SCALE));
    let fog_material = materials_fog.add(super::fog_shader::FogMaterial {
        params: super::fog_shader::FogParams {
            color: Vec4::new(15.0 / 255.0, 15.0 / 255.0, 25.0 / 255.0, 1.0),
            // sun_pos updated each frame by update_fog_material; default
            // is high noon so even the very first frame has a sane value.
            sun_pos: Vec4::new(0.0, 0.0, 1.0, 0.0),
            world: Vec4::new(FOG_SCALE, w_world, h_world, 0.0),
        },
        mask: fog_mask_handle,
    });
    commands.spawn((
        Mesh2d(fog_mesh),
        MeshMaterial2d(fog_material),
        Transform::from_xyz(map_cx, map_cy, 2.0),
        // Visible from spawn so the all-fogged initial mask covers the
        // procedural ground / water shader / etc. (which are Visible by
        // default and would otherwise leak the world during the brief
        // pre-init window). MapSprite is still Hidden → Visible at init,
        // but it renders below the fog anyway. Interior entry/exit still
        // toggles this via interior.rs.
        Visibility::Visible,
        FogSprite,
    ));

    // Spawn chest sprites (above map, below fog)
    {
        let chest_tile_idx = super::Overlay::Chest.tile_index();
        let ts_cols = tileset_img.width() as usize / 20;
        let tc = chest_tile_idx % ts_cols;
        let tr = chest_tile_idx / ts_cols;
        let sx = tc * 20 + 2;
        let sy = tr * 20 + 2;
        let ts_data = &tileset_img.data;
        let ts_w = tileset_img.width() as usize;
        let mut chest_pixels = vec![0u8; 16 * 16 * 4];
        for py in 0..16 { for px in 0..16 {
            let si = ((sy + py) * ts_w + (sx + px)) * 4;
            let di = (py * 16 + px) * 4;
            if si + 3 < ts_data.len() { chest_pixels[di..di+4].copy_from_slice(&ts_data[si..si+4]); }
        }}
        let chest_img = Image::new(Extent3d { width: 16, height: 16, depth_or_array_layers: 1 }, TextureDimension::D2, chest_pixels, TextureFormat::Rgba8UnormSrgb, default());
        let chest_handle = images.add(chest_img);

        for (i, &(cx, cy)) in world.map.chests.iter().enumerate() {
            let pos = WorldGrid::tile_to_world(cx, cy);
            let lift = super::procedural_ground::tile_lift(&world, cx, cy);
            commands.spawn((
                Sprite { image: chest_handle.clone(), ..default() },
                Transform::from_xyz(pos.x, pos.y + lift, 1.5),
                Visibility::Hidden,
                ChestSprite(i),
            ));
        }
    }

    // Spawn monster sprites (animated, speed synced to player)
    {
        use questlib::mapgen::MonsterType;
        use std::collections::HashMap;

        let monster_files: &[(MonsterType, &[u8])] = &[
            (MonsterType::Slime, include_bytes!("../../assets/sprites/monsters/Slime.png")),
            (MonsterType::ClubGoblin, include_bytes!("../../assets/sprites/monsters/ClubGoblin.png")),
            (MonsterType::ArcherGoblin, include_bytes!("../../assets/sprites/monsters/ArcherGoblin.png")),
            (MonsterType::GiantCrab, include_bytes!("../../assets/sprites/monsters/GiantCrab.png")),
            (MonsterType::Minotaur, include_bytes!("../../assets/sprites/monsters/Minotaur.png")),
            (MonsterType::Yeti, include_bytes!("../../assets/sprites/monsters/Yeti.png")),
            (MonsterType::Wendigo, include_bytes!("../../assets/sprites/monsters/Wendigo.png")),
            (MonsterType::PurpleDemon, include_bytes!("../../assets/sprites/monsters/PurpleDemon.png")),
            (MonsterType::Necromancer, include_bytes!("../../assets/sprites/monsters/Necromancer.png")),
            (MonsterType::SkeletonSoldier, include_bytes!("../../assets/sprites/monsters/Skeleton-Soldier.png")),
        ];

        // Load sprite sheets as full textures + create atlas layouts
        let mut sprite_map: HashMap<MonsterType, (Handle<Image>, Handle<TextureAtlasLayout>, usize)> = HashMap::new();
        for (mtype, bytes) in monster_files {
            let dyn_img = image::load_from_memory(bytes).expect("monster sprite");
            let rgba = dyn_img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let cols = (w / 16) as usize;
            let rows = (h / 16) as usize;
            // Count non-empty frames in row 0 before consuming rgba
            let raw_data = rgba.as_raw();
            let mut anim_frames = 0usize;
            for c in 0..cols {
                let mut has_pixels = false;
                for py in 0..16 { for px in 0..16 {
                    let si = (py * w as usize + c * 16 + px) * 4;
                    if si + 3 < raw_data.len() && raw_data[si + 3] > 10 { has_pixels = true; }
                }}
                if has_pixels { anim_frames = c + 1; }
            }
            let img = Image::new(
                Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                TextureDimension::D2, rgba.into_raw(), TextureFormat::Rgba8UnormSrgb, default(),
            );
            let layout = TextureAtlasLayout::from_grid(UVec2::new(16, 16), cols as u32, rows as u32, None, None);
            sprite_map.insert(*mtype, (images.add(img), atlases.add(layout), anim_frames));
        }

        for (i, monster) in world.map.monsters.iter().enumerate() {
            if let Some((tex, layout, cols)) = sprite_map.get(&monster.monster_type) {
                let pos = WorldGrid::tile_to_world(monster.x, monster.y);
                let lift = super::procedural_ground::tile_lift(&world, monster.x, monster.y);
                commands.spawn((
                    Sprite {
                        image: tex.clone(),
                        texture_atlas: Some(TextureAtlas { layout: layout.clone(), index: 0 }),
                        ..default()
                    },
                    Transform::from_xyz(pos.x, pos.y + lift, 1.5),
                    Visibility::Hidden,
                    MonsterSprite(i),
                    MonsterAnimation {
                        timer: Timer::from_seconds(monster_idle_period(monster.difficulty), TimerMode::Repeating),
                        frame: 0,
                        cols: *cols,
                        difficulty: monster.difficulty,
                    },
                ));
            }
        }
    }

    // Player character (hidden until server sends position)
    let champion_name = if !session.champion.is_empty() { &session.champion } else { "Katan" };
    let info = champion_info(champion_name);
    let champion_dyn = image::load_from_memory(info.bytes).expect("player sprite");
    let champion_rgba = champion_dyn.to_rgba8();
    let (cw, ch) = champion_rgba.dimensions();
    let champion_tex = images.add(Image::new(
        Extent3d { width: cw, height: ch, depth_or_array_layers: 1 },
        TextureDimension::D2, champion_rgba.into_raw(), TextureFormat::Rgba8UnormSrgb, default(),
    ));
    let layout_handle = atlases.add(champion_atlas_layout(&info));
    commands.spawn((
        Sprite { image: champion_tex, texture_atlas: Some(TextureAtlas { layout: layout_handle, index: 0 }), ..default() },
        Transform::from_xyz(0.0, 0.0, 5.0), Visibility::Hidden, PlayerSprite,
        WalkAnimation { timer: Timer::from_seconds(0.15, TimerMode::Repeating), frame: 0, facing: Facing::Down, moving: false, cols: info.cols, facing_rows: info.facing_rows, facing_flip: info.facing_flip },
    ));
    commands.spawn((Text2d::new(""), TextFont { font: font.0.clone(), font_size: 8.0, ..default() }, TextColor(Color::srgb(0.1, 0.1, 0.1)), Transform::from_xyz(0.0, 12.0, 6.0), Visibility::Hidden, PlayerNameTag));
    commands.spawn((Text2d::new(""), TextFont { font: font.0.clone(), font_size: 8.0, ..default() }, TextColor(Color::srgb(1.0, 1.0, 1.0)), Transform::from_xyz(0.0, 0.0, 10.0), Visibility::Hidden, TileInfoText));

    // Loading text
    commands.spawn((Node { position_type: PositionType::Absolute, top: Val::Percent(45.0), width: Val::Percent(100.0), justify_content: JustifyContent::Center, ..default() }, LoadingText))
        .with_children(|p| { p.spawn((Text::new("Loading world..."), TextFont { font: font.0.clone(), font_size: 16.0, ..default() }, TextColor(Color::srgb(0.77, 0.64, 0.35)))); });

    // POI labels
    for poi in &world.map.pois {
        let pos = WorldGrid::tile_to_world(poi.x, poi.y);
        let lift = super::procedural_ground::tile_lift(&world, poi.x, poi.y);
        commands.spawn((Text2d::new(format!("{:?}", poi.poi_type)), TextFont { font: font.0.clone(), font_size: 8.0, ..default() }, TextColor(Color::srgb(0.1, 0.1, 0.1)), Transform::from_xyz(pos.x, pos.y + lift - 12.0, 8.0), Visibility::Hidden, PoiLabel));
    }

    // Custom POI sprites — illustrated landmark art drawn over the tile
    // overlay for supported POI types. Size per POI type: 1×1 tile for
    // small landmarks (houses, huts), up to 3×3 tiles for iconic ones
    // (castles, fortresses). Z=1.7 puts them above ground (0.0) +
    // monsters (1.5) but below fog (2.0), so fogged landmarks stay
    // hidden until the player reveals the tile.
    //
    // To add a new POI type here later: drop a PNG into
    // crates/gameclient/assets/poi/ and add a branch to poi_sprite_path
    // with its desired tile_size (1–3). Source PNGs should have a
    // transparent background; RGB-only PNGs will show their baked
    // background rectangle.
    for poi in &world.map.pois {
        if let Some((path, tile_size)) = poi_sprite_path(poi.poi_type) {
            let pos = WorldGrid::tile_to_world(poi.x, poi.y);
            let lift = super::procedural_ground::tile_lift(&world, poi.x, poi.y);
            let px = TILE_PX * tile_size as f32;
            // NOTE: no `?v=CLIENT_VERSION` cache-busting here. Bevy's
            // AssetServer treats the string as an asset path, not a URL,
            // and its extension detection sees `png?v=N` → no PNG loader
            // matches → sprites go blank. Browser-level stale PNG after
            // an art update is handled with a hard refresh for now.
            commands.spawn((
                Sprite {
                    image: asset_server.load(path),
                    custom_size: Some(Vec2::new(px, px)),
                    ..default()
                },
                Transform::from_xyz(pos.x, pos.y + lift, 1.7),
                PoiCustomSprite,
            ));
        }
    }

    commands.insert_resource(fog);
    commands.insert_resource(debug);
    commands.insert_resource(MyPlayerState::default());
    commands.insert_resource(DisplayRoute::default());
    commands.insert_resource(InterpolationState::default());
    commands.insert_resource(VisualState::default());
    commands.insert_resource(ShopPollState::default());
    commands.insert_resource(CameraPan::default());
    commands.insert_resource(world);
}

// ── Core Systems ──────────────────────────────────────

/// Read polled data from server → update MyPlayerState.
fn apply_server_state(
    polled: Res<PolledPlayerState>,
    session: Res<GameSession>,
    mut state: ResMut<MyPlayerState>,
    mut interp: ResMut<InterpolationState>,
    mut display_route: ResMut<DisplayRoute>,
    mut fog: ResMut<FogOfWar>,
    mut commands: Commands,
    mut player_tf: Query<(&mut Transform, &mut Visibility), (With<PlayerSprite>, Without<Camera2d>, Without<MapSprite>, Without<FogSprite>)>,
    mut camera_tf: Query<&mut Transform, (With<Camera2d>, Without<PlayerSprite>)>,
    mut map_vis: Query<&mut Visibility, (With<MapSprite>, Without<PlayerSprite>, Without<FogSprite>)>,
    mut fog_vis: Query<&mut Visibility, (With<FogSprite>, Without<PlayerSprite>, Without<MapSprite>)>,
    loading_q: Query<Entity, With<LoadingText>>,
    path_markers: Query<Entity, With<PathMarker>>,
    world: Option<Res<WorldGrid>>,
) {
    let Ok(players) = polled.players.lock() else { return };
    if players.is_empty() || session.player_name.is_empty() { return; }
    let Some(me) = players.iter().find(|p| p.name.eq_ignore_ascii_case(&session.player_name)) else { return };

    let tile_changed = me.map_tile_x.unwrap_or(0) != state.tile_x || me.map_tile_y.unwrap_or(0) != state.tile_y;

    // Update state from server
    state.tile_x = me.map_tile_x.unwrap_or(0);
    state.tile_y = me.map_tile_y.unwrap_or(0);
    state.speed_kmh = me.current_speed_kmh;
    state.is_walking = me.is_walking;
    state.gold = me.gold;
    state.facing = me.facing;
    state.total_distance_m = me.total_distance_m;
    state.inventory = me.inventory.clone();
    state.equipment = me.equipment.clone();
    state.opened_chests = me.opened_chests.clone();
    state.defeated_monsters = me.defeated_monsters.clone();
    state.location = me.location.clone();
    state.completed_events = me.completed_events.clone();
    state.item_upgrades = me.item_upgrades.clone();
    state.boons = me.boons.clone();
    state.pending_boon_choice = me.pending_boon_choice.clone();
    state.active_buffs = me.active_buffs.clone();
    // Detect adventure switch — clear local fog so we don't carry
    // stale reveals from the previous world. The server has already
    // reset its per-player fog; without this, the client keeps the
    // old bitfield forever and the player wonders why they "remember"
    // a world they've never visited.
    let prev_adv = state.adventure_id.clone();
    if !me.adventure_id.is_empty() {
        state.adventure_id = me.adventure_id.clone();
    }
    if !prev_adv.is_empty() && prev_adv != state.adventure_id {
        for cell in fog.revealed.iter_mut() { *cell = false; }
        fog.dirty = true;
    }

    // Parse route from server — check if server has caught up to local changes.
    let server_in_sync = if let Some(ref route_json) = me.planned_route {
        if !route_json.is_empty() {
            if let Some(route) = questlib::route::parse_route_json(route_json) {
                if !display_route.locally_modified {
                    state.route = route.clone();
                    display_route.waypoints = route;
                    true
                } else if state.route == route {
                    // Server caught up to our local route — clear flag
                    display_route.locally_modified = false;
                    true
                } else {
                    false // server still has stale route
                }
            } else {
                !display_route.locally_modified
            }
        } else {
            if !display_route.locally_modified {
                state.route.clear();
                display_route.waypoints.clear();
                true
            } else if display_route.waypoints.is_empty() {
                // Server confirmed empty route matches our local clear
                display_route.locally_modified = false;
                true
            } else {
                false
            }
        }
    } else {
        !display_route.locally_modified
    };

    // Only accept server meters/interp when the server has our current route.
    // Otherwise its meters refer to a stale route and would cause jumps.
    if server_in_sync {
        let server_meters = me.route_meters_walked.unwrap_or(0.0);
        let target = me.interp_meters_target.unwrap_or(server_meters);
        let duration = me.interp_duration_secs.unwrap_or(0.0);
        state.route_meters = server_meters;
        interp.start_meters = server_meters;
        interp.target_meters = target;
        interp.duration = duration;
        interp.elapsed = 0.0;
    }


    // Update fog from server
    if let Some(ref encoded) = me.revealed_tiles {
        if !encoded.is_empty() {
            if let Some(server_fog) = questlib::fog::FogBitfield::from_base64(encoded) {
                for y in 0..world_h() {
                    for x in 0..world_w() {
                        if server_fog.is_revealed(x, y) && !fog.is_revealed(x, y) {
                            fog.revealed[y * world_w() + x] = true;
                            fog.dirty = true;
                        }
                    }
                }
            }
        }
    }

    // First init — show everything, snap camera
    if !state.initialized {
        state.initialized = true;

        let pos = WorldGrid::tile_to_world(state.tile_x as usize, state.tile_y as usize);
        for (mut tf, mut vis) in &mut player_tf { tf.translation.x = pos.x; tf.translation.y = pos.y; *vis = Visibility::Visible; }
        for mut vis in &mut map_vis { *vis = Visibility::Visible; }
        for mut vis in &mut fog_vis { *vis = Visibility::Visible; }
        for mut cam in &mut camera_tf { cam.translation.x = pos.x; cam.translation.y = pos.y; }
        for entity in &loading_q { commands.entity(entity).despawn_recursive(); }
    }

    // Redraw path markers when tile changes (but not if user just modified route)
    if (tile_changed || !state.initialized) && !display_route.locally_modified {
        for entity in &path_markers { commands.entity(entity).despawn(); }
        if let Some(world) = &world {
            let tile_idx = tile_index_from_meters(&state.route, state.route_meters, world);
            draw_path_markers(&mut commands, &display_route.waypoints, tile_idx, &fog);
        }
    }

    state.last_poll_tile = (state.tile_x, state.tile_y);
}

/// Between polls: advance interpolation timer. The actual meters are computed
/// by InterpolationState::current_meters() which lerps between server-confirmed
/// position and the server's projected target. Can never overshoot.
fn interpolate_movement(
    time: Res<Time>,
    mut interp: ResMut<InterpolationState>,
) {
    if interp.duration > 0.0 {
        interp.elapsed += time.delta_secs();
    }
}

/// Set character position with smooth interpolation.
fn render_character(
    state: Res<MyPlayerState>,
    interp: Res<InterpolationState>,
    mut visual: ResMut<VisualState>,
    session: Res<GameSession>,
    time: Res<Time>,
    world: Option<Res<WorldGrid>>,
    mut player_q: Query<(&mut Transform, &mut WalkAnimation, &mut Sprite), With<PlayerSprite>>,
    mut nametag_q: Query<(&mut Transform, &mut Visibility), (With<PlayerNameTag>, Without<PlayerSprite>)>,
    keys: Res<ButtonInput<KeyCode>>,
) {
    if !state.initialized { return; }

    // Compute target position and facing from route
    let total_meters = interp.current_meters();
    let (target_pos, visual_facing) = if !state.route.is_empty() {
        if let Some(world) = &world {
            if let Some((pos, idx)) = position_and_index_from_route_meters(&state.route, total_meters, world) {
                let facing = questlib::route::facing_along_route(&state.route, idx);
                (pos, facing)
            } else {
                (WorldGrid::tile_to_world(state.tile_x as usize, state.tile_y as usize), state.facing)
            }
        } else {
            (WorldGrid::tile_to_world(state.tile_x as usize, state.tile_y as usize), state.facing)
        }
    } else {
        (WorldGrid::tile_to_world(state.tile_x as usize, state.tile_y as usize), state.facing)
    };

    // Initialize visual position on first frame (snap, don't lerp)
    if !visual.initialized {
        visual.pos = target_pos;
        visual.initialized = true;
    }

    // Snap to tile position when route just completed (no lerp back)
    let has_route = !state.route.is_empty();
    if visual.had_route && !has_route {
        visual.pos = target_pos;
    }
    visual.had_route = has_route;

    // Smoothly interpolate visual position toward target.
    // Uses exponential decay: lerp factor = 1 - e^(-rate * dt)
    // Rate of 6 gives smooth movement (~115ms to close half the gap).
    let dt = time.delta_secs();
    let lerp_factor = 1.0 - (-6.0_f32 * dt).exp();
    visual.pos = visual.pos.lerp(target_pos, lerp_factor);

    // Snap if very close to avoid perpetual micro-drift
    if visual.pos.distance_squared(target_pos) < 0.01 {
        visual.pos = target_pos;
    }

    // Same tile-lift the ground mesh uses, sampled at the player's
    // current visual position. Looking up by `world_to_tile` (no
    // bilinear interp) means a tile boundary creates a 1–4 px jump,
    // but tiles transition fast enough that it reads as a step
    // rather than jitter.
    let player_lift = if let Some(world) = &world {
        let (tx, ty) = WorldGrid::world_to_tile(visual.pos);
        super::procedural_ground::tile_lift(world, tx, ty)
    } else {
        0.0
    };

    for (mut tf, mut anim, mut sprite) in &mut player_q {
        // Round to whole pixels to prevent sprite atlas bleed
        tf.translation.x = visual.pos.x.round();
        tf.translation.y = (visual.pos.y + player_lift).round();

        // Derive facing from the visual position on the route
        anim.facing = visual_facing;

        let idx = facing_idx(anim.facing);
        sprite.flip_x = anim.facing_flip[idx];

        let should_animate = state.is_walking && state.speed_kmh > 0.1;
        let cols = anim.cols as usize;
        if should_animate {
            let speed_factor = state.speed_kmh.clamp(0.5, 6.0);
            anim.timer.set_duration(std::time::Duration::from_secs_f32(0.3 / speed_factor));
            anim.timer.tick(time.delta());
            if anim.timer.just_finished() { anim.frame = (anim.frame % 4) + 1; }
            let row = anim.facing_rows[idx];
            if let Some(ref mut atlas) = sprite.texture_atlas { atlas.index = row * cols + anim.frame; }
            anim.moving = true;
        } else if anim.moving {
            anim.moving = false;
            anim.frame = 0;
            let row = anim.facing_rows[idx];
            if let Some(ref mut atlas) = sprite.texture_atlas { atlas.index = row * cols; }
        }
    }

    // Name tag
    if let Ok((player_tf, _, _)) = player_q.get_single() {
        let show = keys.pressed(KeyCode::Tab);
        for (mut tf, mut vis) in &mut nametag_q {
            tf.translation.x = player_tf.translation.x;
            tf.translation.y = player_tf.translation.y + 12.0;
            *vis = if show { Visibility::Visible } else { Visibility::Hidden };
        }
    }
}

// ── Route Planning ────────────────────────────────────

fn handle_map_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform)>,
    world: Res<WorldGrid>,
    fog: Res<FogOfWar>,
    debug: Res<DebugOptions>,
    session: Res<GameSession>,
    mut state: ResMut<MyPlayerState>,
    mut display_route: ResMut<DisplayRoute>,
    mut interp: ResMut<InterpolationState>,
    mut commands: Commands,
    path_markers: Query<Entity, With<PathMarker>>,
    mut info_q: Query<(&mut Text2d, &mut Transform), (With<TileInfoText>, Without<PlayerSprite>)>,
    mut notifications: ResMut<crate::dialogue::NotificationQueue>,
    ui_hover: Res<crate::UiHovered>,
) {
    // Defer to crate::terrain::interior when the player is inside one —
    // pathfinding, rendering, and route submission all diverge there.
    if state.location.is_some() { return; }
    let Ok(window) = windows.get_single() else { return };
    let Ok((camera, cam_tf)) = camera_q.get_single() else { return };
    let Some(cursor) = window.cursor_position() else { return };
    let Ok(world_pos) = camera.viewport_to_world_2d(cam_tf, cursor) else { return };

    let (tx, ty) = WorldGrid::world_to_tile(world_pos);
    let terrain = world.get(tx, ty);

    // Tile info on TAB
    if let Ok((mut text, mut transform)) = info_q.get_single_mut() {
        if keys.pressed(KeyCode::Tab) {
            if fog.is_revealed(tx, ty) || debug.fog_disabled {
                let cost = if terrain.is_passable() { format!("{}m", terrain.movement_cost()) } else { "impassable".into() };
                *text = Text2d::new(format!("{} {}", terrain.name(), cost));
            } else { *text = Text2d::new("???"); }
            let p = WorldGrid::tile_to_world(tx, ty);
            transform.translation = Vec3::new(p.x, p.y + 16.0, 10.0);
        } else { *text = Text2d::new(""); }
    }

    // Click to plan route — skip if UI is hovered
    let is_revealed = fog.is_revealed(tx, ty) || debug.fog_disabled;
    let ui_hovered = ui_hover.0;
    if mouse.just_pressed(MouseButton::Left) && terrain.is_passable() && is_revealed && !ui_hovered {
        let current_pos = (state.tile_x as usize, state.tile_y as usize);
        let has_active_route = !display_route.waypoints.is_empty();
        // Shift-click extends the current route; plain click replaces it.
        let extending = has_active_route
            && (keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight));

        // Clicking the tile you're already routing to is a no-op. Without
        // this guard a plain click would replay `/set_route` and reset
        // route_meters_walked to 0 — which would drop the fractional-tile
        // progress the player had accumulated and visually snap them back.
        if has_active_route && *display_route.waypoints.last().unwrap() == (tx, ty) { return; }

        // Plan path: from last waypoint when extending, from current tile otherwise.
        let start = if extending {
            *display_route.waypoints.last().unwrap()
        } else {
            current_pos
        };
        if start == (tx, ty) { return; }

        let mut inv_ids: Vec<String> = state.inventory.iter().map(|s| s.item_id.clone()).collect();
        // Include equipped items for biome gate checks. Using all_slots()
        // means new slots (Feet, ToeRings, future) are included automatically.
        for slot in questlib::items::EquipmentLoadout::all_slots() {
            if let Some(id) = state.equipment.get_slot(slot) {
                inv_ids.push(id.to_string());
            }
        }
        let path_result = find_path_with_items(&world, start, (tx, ty), &inv_ids);
        if path_result.is_none() {
            // Path blocked — check why and tell the player
            let target_biome = world.map.biome_at(tx, ty);
            if let Some(req) = target_biome.required_item() {
                let msg = match req {
                    "warm_cloak" => "It's too cold! You need a Warm Cloak to enter the mountains.",
                    "bog_charm" => "The swamp is cursed! You need a Bog Charm to enter.",
                    _ => "You need a special item to go there.",
                };
                notifications.pending.push(crate::dialogue::NotificationData { text: msg.to_string(), duration: 4.0 });
            }
            return;
        }
        if let Some(mut segment) = path_result {
            let marker_skip;

            if extending {
                // Append the new segment after the last waypoint. First tile of
                // the segment IS the last waypoint; drop it to avoid duplication.
                if !segment.is_empty() { segment.remove(0); }
                display_route.waypoints.extend(segment);
                // Skip already-walked tiles in the marker draw based on the last
                // server-confirmed meters (NOT interpolated — server is source of
                // truth for distance).
                marker_skip = tile_index_from_meters(&display_route.waypoints, state.route_meters, &world);
            } else {
                // Fresh route from current position — keep the player's
                // current sub-tile progress so re-routing mid-tile
                // doesn't snap them back to the last confirmed tile
                // center, AND doesn't skip forward through the new
                // route's tiles. The server's /set_route does the same
                // thing on its side; mirror that math here so the local
                // update stays consistent until the next poll confirms.
                //
                // Bug history: passing `interp.current_meters()` straight
                // into the new route applied OLD-route meters to a NEW
                // route whose first tile is `current_pos` — at meter 100
                // of [A,B,C] you're on C, but at meter 100 of [C,F,G]
                // you've walked past C and F into G. Visible as a
                // forward jump that "skipped a tile entirely" on click.
                let old_route = std::mem::replace(&mut display_route.waypoints, segment);
                let cost_to_idx = |route: &[(usize, usize)], idx: usize| -> f64 {
                    route[..idx].iter()
                        .map(|&(x, y)| world.server_tile_cost(x, y) as f64)
                        .sum()
                };
                let cur_visual = interp.current_meters();
                let partial_raw = old_route.iter().position(|&w| w == current_pos)
                    .map(|i| (cur_visual - cost_to_idx(&old_route, i)).max(0.0))
                    .unwrap_or(0.0);
                // Clamp inside the current tile so visual lerp leading the
                // server can't bleed into the new route's NEXT tile.
                let cur_cost = world.server_tile_cost(current_pos.0, current_pos.1) as f64;
                let new_meters = partial_raw.min((cur_cost - 0.01).max(0.0));
                state.route_meters = new_meters;
                interp.start_meters = new_meters;
                interp.target_meters = new_meters;
                interp.elapsed = 0.0;
                interp.duration = 0.0;
                marker_skip = tile_index_from_meters(&display_route.waypoints, new_meters, &world);
            }
            display_route.locally_modified = true;
            state.route = display_route.waypoints.clone();

            // Redraw markers (skip already-walked tiles)
            for entity in &path_markers { commands.entity(entity).despawn(); }
            draw_path_markers(&mut commands, &display_route.waypoints, marker_skip, &fog);

            // Send to server. Client submits ONLY geometry — server recomputes
            // route_meters_walked from the player's current tile in the new route.
            let route_json = questlib::route::encode_route_json(&display_route.waypoints);
            supabase::write_planned_route(&session.player_id, &route_json);
        }
    }
}

fn handle_clear_route(
    keys: Res<ButtonInput<KeyCode>>,
    session: Res<GameSession>,
    mut state: ResMut<MyPlayerState>,
    mut display_route: ResMut<DisplayRoute>,
    mut interp: ResMut<InterpolationState>,
    world: Res<WorldGrid>,
    mut commands: Commands,
    path_markers: Query<Entity, With<PathMarker>>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        // Snap tile position to where the character visually is on the route,
        // so we don't jump back to the last server-confirmed tile.
        if !state.route.is_empty() {
            let current_meters = interp.current_meters();
            let idx = tile_index_from_meters(&state.route, current_meters, &world);
            if let Some(&(tx, ty)) = state.route.get(idx) {
                state.tile_x = tx as i32;
                state.tile_y = ty as i32;
            }
        }

        display_route.waypoints.clear();
        display_route.locally_modified = true;
        state.route.clear();
        state.route_meters = 0.0;
        interp.start_meters = 0.0;
        interp.target_meters = 0.0;
        interp.elapsed = 0.0;
        interp.duration = 0.0;
        for entity in &path_markers { commands.entity(entity).despawn(); }
        supabase::write_planned_route(&session.player_id, "");
    }
}

/// Draw dashed path markers from a given tile index onward.
fn draw_path_markers(commands: &mut Commands, waypoints: &[(usize, usize)], skip_until: usize, fog: &FogOfWar) {
    let len = waypoints.len();
    if len == 0 { return; }

    let dash_len = 4.0_f32;
    let gap_len = 3.0_f32;
    let line_width = 1.5_f32;

    let start = (skip_until + 1).min(len);
    for i in start..len {
        let p1 = WorldGrid::tile_to_world(waypoints[i - 1].0, waypoints[i - 1].1);
        let p2 = WorldGrid::tile_to_world(waypoints[i].0, waypoints[i].1);
        let dx = p2.x - p1.x; let dy = p2.y - p1.y;
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len < 0.1 { continue; }
        let nx = dx / seg_len; let ny = dy / seg_len;

        let mut d = 0.0_f32;
        let mut drawing = true;
        while d < seg_len {
            if drawing {
                let end = (d + dash_len).min(seg_len);
                let cx = p1.x + nx * (d + end) * 0.5;
                let cy = p1.y + ny * (d + end) * 0.5;
                let length = end - d;
                let (w, h) = if nx.abs() > ny.abs() { (length, line_width) } else { (line_width, length) };
                let (tile_x, tile_y) = WorldGrid::world_to_tile(Vec2::new(cx, cy));
                let color = if fog.is_revealed(tile_x, tile_y) { Color::srgba(0.0, 0.0, 0.0, 0.7) } else { Color::srgba(1.0, 1.0, 1.0, 0.7) };
                commands.spawn((Sprite { color, custom_size: Some(Vec2::new(w, h)), ..default() }, Transform::from_xyz(cx, cy, 3.0), PathMarker));
                d = end + gap_len;
            } else { d += gap_len; }
            drawing = !drawing;
        }
    }

    // Flag at destination
    if len > start {
        let pos = WorldGrid::tile_to_world(waypoints[len - 1].0, waypoints[len - 1].1);
        commands.spawn((Sprite { color: Color::srgb(0.3, 0.2, 0.1), custom_size: Some(Vec2::new(1.5, 14.0)), ..default() }, Transform::from_xyz(pos.x - 3.0, pos.y + 4.0, 3.5), PathMarker));
        commands.spawn((Sprite { color: Color::srgb(0.9, 0.2, 0.1), custom_size: Some(Vec2::new(8.0, 6.0)), ..default() }, Transform::from_xyz(pos.x + 1.0, pos.y + 9.0, 3.6), PathMarker));
    }
}

// ── Camera / UI Systems (mostly unchanged) ────────────

fn handle_pan(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    mut pan: ResMut<CameraPan>,
    mut camera_q: Query<(&mut Transform, &OrthographicProjection), With<Camera2d>>,
) {
    let Ok(window) = windows.get_single() else { return };
    let Ok((mut cam, proj)) = camera_q.get_single_mut() else { return };
    if mouse.pressed(MouseButton::Right) {
        if let Some(cursor) = window.cursor_position() {
            if let Some(last) = pan.last_pos { let d = cursor - last; cam.translation.x -= d.x * proj.scale; cam.translation.y += d.y * proj.scale; }
            pan.last_pos = Some(cursor); pan.active = true;
        }
    } else { pan.last_pos = None; pan.active = false; }
}

#[derive(Resource)]
struct ZoomTarget { target: f32 }
impl Default for ZoomTarget { fn default() -> Self { Self { target: 0.4 } } }

fn handle_zoom(
    mut scroll_evr: EventReader<bevy::input::mouse::MouseWheel>,
    mut camera_q: Query<&mut OrthographicProjection, With<Camera2d>>,
    mut zoom: Local<ZoomTarget>,
    time: Res<Time>,
    journal: Res<crate::hud::journal::JournalOpen>,
) {
    let Ok(mut proj) = camera_q.get_single_mut() else { return };
    // Suppress zoom while journal is open so scrolling the journal
    // doesn't also zoom the map. Wheel events are still consumed to
    // prevent stale state leaking into the next frame.
    if journal.is_open() {
        scroll_evr.clear();
        // Still interpolate toward existing target so zoom animation
        // finishes smoothly.
        let diff = zoom.target - proj.scale;
        proj.scale += diff * (1.0 - (-6.0 * time.delta_secs()).exp());
        return;
    }
    for ev in scroll_evr.read() {
        if ev.y > 0.0 { zoom.target = (zoom.target * 0.75).max(0.15); }
        else if ev.y < 0.0 { zoom.target = (zoom.target * 1.5).min(3.0); }
    }
    let diff = zoom.target - proj.scale;
    proj.scale += diff * (1.0 - (-6.0 * time.delta_secs()).exp());
}

fn toggle_poi_labels(keys: Res<ButtonInput<KeyCode>>, mut labels: Query<&mut Visibility, With<PoiLabel>>, debug: Res<DebugOptions>) {
    let show = keys.pressed(KeyCode::Tab) || debug.show_pois;
    for mut vis in &mut labels { *vis = if show { Visibility::Visible } else { Visibility::Hidden }; }
}

fn update_fog_texture(
    mut fog: ResMut<FogOfWar>,
    debug: Res<DebugOptions>,
    fog_q: Query<&MeshMaterial2d<super::fog_shader::FogMaterial>, With<FogSprite>>,
    mut materials: ResMut<Assets<super::fog_shader::FogMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    if !fog.dirty { return; }
    fog.dirty = false;
    let Ok(mat_handle) = fog_q.get_single() else { return };
    // Replace the mask image entirely. In-place mutation of R8 image
    // data via Assets::get_mut wasn't reliably re-uploading to the
    // GPU (the material bind group cached the previous texture);
    // swapping the handle forces a rebind. Old image gets dropped
    // by Handle ref counting.
    let new_mask = images.add(create_fog_mask(&fog, &debug));
    if let Some(mat) = materials.get_mut(&mat_handle.0) {
        mat.mask = new_mask;
    }
}

fn update_camera(
    player_q: Query<&Transform, With<PlayerSprite>>,
    mut camera_q: Query<(&mut Transform, &mut OrthographicProjection), (With<Camera2d>, Without<PlayerSprite>)>,
    pan: Res<CameraPan>,
    debug: Res<DebugOptions>,
    mut initialized: Local<bool>,
) {
    let Some(ptf) = player_q.iter().next() else { return };
    let Ok((mut cam, mut proj)) = camera_q.get_single_mut() else { return };
    if !*initialized { proj.scale = 0.4; *initialized = true; }
    // F5 procedural test mode: don't auto-recenter on the player so
    // the user can pan freely to inspect / screenshot test patterns.
    if !pan.active && !debug.procedural_test_mode {
        cam.translation.x += (ptf.translation.x - cam.translation.x) * 0.05;
        cam.translation.y += (ptf.translation.y - cam.translation.y) * 0.05;
    }
    let ps = 1.0 / proj.scale;
    cam.translation.x = (cam.translation.x * ps).round() / ps;
    cam.translation.y = (cam.translation.y * ps).round() / ps;
}

fn update_other_players(
    mut commands: Commands,
    session: Res<GameSession>,
    polled: Res<PolledPlayerState>,
    time: Res<Time>,
    world: Option<Res<WorldGrid>>,
    mut images: ResMut<Assets<Image>>,
    mut atlases: ResMut<Assets<TextureAtlasLayout>>,
    mut existing: Query<(Entity, &mut OtherPlayerSprite, &mut Transform, &mut Sprite, &mut OtherPlayerAnim, &mut Visibility), Without<OtherPlayerName>>,
    mut name_q: Query<(&OtherPlayerName, &mut Transform, &mut Visibility), (Without<OtherPlayerSprite>, Without<PlayerNameTag>)>,
    font: Res<GameFont>,
    keys: Res<ButtonInput<KeyCode>>,
) {
    let Some(world) = world else { return; };
    let Ok(lock) = polled.players.lock() else { return; };

    // Get other players (not us) AND in the same location as us AND
    // in the same adventure. Overworld and interior share world-space
    // coords, so a player in a different cave must not be drawn over
    // our overworld position. Separate adventures share the same
    // world-space coords too — without the adventure filter, a player
    // in frost_quest would render on top of our chaos overworld at
    // the same (tile_x, tile_y).
    let me_row = lock.iter().find(|p| p.id == session.player_id);
    let my_loc = me_row.and_then(|p| p.location.clone());
    let my_adv = me_row.map(|p| p.adventure_id.clone()).unwrap_or_default();
    let others: Vec<_> = lock.iter()
        .filter(|p| p.id != session.player_id
            && p.location == my_loc
            && (my_adv.is_empty() || p.adventure_id == my_adv))
        .collect();

    // Hide sprites for players that are no longer co-located with us. The
    // loop below will re-show the ones still in `others`.
    let visible_ids: std::collections::HashSet<&str> = others.iter().map(|p| p.id.as_str()).collect();
    for (_, ops, _, _, _, mut vis) in &mut existing {
        if !visible_ids.contains(ops.id.as_str()) {
            *vis = Visibility::Hidden;
        }
    }
    for (ops_name, _, mut vis) in &mut name_q {
        if !visible_ids.contains(ops_name.0.as_str()) {
            *vis = Visibility::Hidden;
        }
    }

    for other in &others {
        // Use route interpolation if the other player has a route
        let tile_pos = WorldGrid::tile_to_world(
            other.map_tile_x.unwrap_or(0) as usize,
            other.map_tile_y.unwrap_or(0) as usize,
        );
        let target_pos = if let Some(ref route_json) = other.planned_route {
            if !route_json.is_empty() {
                if let Some(route) = questlib::route::parse_route_json(route_json) {
                    let meters = other.interp_meters_target.unwrap_or(
                        other.route_meters_walked.unwrap_or(0.0)
                    );
                    position_from_route_meters(&route, meters, &world)
                        .unwrap_or(tile_pos)
                } else { tile_pos }
            } else { tile_pos }
        } else { tile_pos };

        // Find existing sprite for this player
        let found = existing.iter_mut().find(|(_, ops, _, _, _, _)| ops.id == other.id);

        if let Some((_, mut ops, mut tf, mut sprite, mut anim, mut vis)) = found {
            *vis = Visibility::Visible;
            // Smooth interpolation toward target. Lerp on the unrounded
            // visual_pos so rounding for crisp pixels doesn't eat the
            // sub-pixel progress and freeze the sprite a few px shy of
            // a static target.
            let dt = time.delta_secs();
            let lerp = 1.0 - (-4.0_f32 * dt).exp();
            ops.visual_pos = ops.visual_pos.lerp(target_pos, lerp);
            if ops.visual_pos.distance_squared(target_pos) < 0.01 {
                ops.visual_pos = target_pos;
            }
            tf.translation.x = ops.visual_pos.x.round();
            tf.translation.y = ops.visual_pos.y.round();

            // Animation
            let is_walking = other.is_walking && other.current_speed_kmh > 0.1;
            let cols = anim.cols as usize;
            let idx = facing_idx(other.facing);
            sprite.flip_x = anim.facing_flip[idx];
            if is_walking {
                let speed_factor = other.current_speed_kmh.clamp(0.5, 6.0);
                anim.timer.set_duration(std::time::Duration::from_secs_f32(0.3 / speed_factor));
                anim.timer.tick(time.delta());
                if anim.timer.just_finished() {
                    anim.frame = (anim.frame % 4) + 1;
                }
                let row = anim.facing_rows[idx];
                if let Some(ref mut atlas) = sprite.texture_atlas { atlas.index = row * cols + anim.frame; }
                anim.moving = true;
            } else if anim.moving {
                anim.moving = false;
                anim.frame = 0;
                let row = anim.facing_rows[idx];
                if let Some(ref mut atlas) = sprite.texture_atlas { atlas.index = row * cols; }
            }
        } else {
            // Spawn new sprite for this player using their chosen champion.
            let champ = other.champion.as_deref().filter(|s| !s.is_empty()).unwrap_or("Zhinja");
            let info = champion_info(champ);
            let dyn_img = image::load_from_memory(info.bytes).expect("other player sprite");
            let rgba = dyn_img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let tex = images.add(Image::new(
                Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                TextureDimension::D2, rgba.into_raw(), TextureFormat::Rgba8UnormSrgb, default(),
            ));
            let layout_handle = atlases.add(champion_atlas_layout(&info));
            let pos = target_pos;
            commands.spawn((
                Sprite { image: tex, texture_atlas: Some(TextureAtlas { layout: layout_handle, index: 0 }), ..default() },
                Transform::from_xyz(pos.x.round(), pos.y.round(), 4.0),
                OtherPlayerSprite { id: other.id.clone(), visual_pos: pos },
                OtherPlayerAnim { timer: Timer::from_seconds(0.2, TimerMode::Repeating), frame: 0, moving: false, cols: info.cols, facing_rows: info.facing_rows, facing_flip: info.facing_flip },
            ));
            // Name tag — hidden by default; the TAB pass below reveals it
            // whenever the player holds TAB and the owner is co-located.
            commands.spawn((
                Text2d::new(&other.name),
                TextFont { font: font.0.clone(), font_size: 7.0, ..default() },
                TextColor(Color::srgb(0.5, 0.8, 1.0)),
                Transform::from_xyz(pos.x, pos.y + 12.0, 6.0),
                Visibility::Hidden,
                OtherPlayerName(other.id.clone()),
            ));
        }
    }

    // Update name tag positions. Visibility is gated on TAB — the label
    // only shows while the player is holding it, matching the way the
    // local player's own name tag + POI labels work. Position still
    // tracks the sprite each frame so TAB reveals the label in the
    // right place.
    let show_tab = keys.pressed(KeyCode::Tab);
    for other in &others {
        let target_pos = WorldGrid::tile_to_world(
            other.map_tile_x.unwrap_or(0) as usize,
            other.map_tile_y.unwrap_or(0) as usize,
        );
        for (name_comp, mut tf, mut vis) in name_q.iter_mut() {
            if name_comp.0 == other.id {
                *vis = if show_tab { Visibility::Visible } else { Visibility::Hidden };
                let dt = time.delta_secs();
                let lerp = 1.0 - (-6.0_f32 * dt).exp();
                tf.translation.x += (target_pos.x - tf.translation.x) * lerp;
                tf.translation.y += (target_pos.y + 12.0 - tf.translation.y) * lerp;
            }
        }
    }
}

/// Monster idle-frame period based on difficulty. World mapgen produces
/// difficulty 1-5 (slime..wendigo/necromancer); boss events go higher but
/// those aren't placed on the map. Wider spread so the visual difference
/// between a slime and a necromancer actually reads.
///   1 Slime/weak       → 0.90s (drowsy)
///   2 Goblin/Crab      → 0.60s
///   3 Archer/Skeleton  → 0.40s
///   4 Minotaur/Yeti    → 0.25s
///   5+ Wendigo/Necro   → 0.15s (very active / scary)
fn monster_idle_period(difficulty: u32) -> f32 {
    match difficulty {
        0..=1 => 0.90,
        2     => 0.60,
        3     => 0.40,
        4     => 0.25,
        _     => 0.15,
    }
}

fn animate_monsters(
    time: Res<Time>,
    mut monsters: Query<(&mut MonsterAnimation, &mut Sprite), With<MonsterSprite>>,
) {
    for (mut anim, mut sprite) in &mut monsters {
        anim.timer.tick(time.delta());
        if anim.timer.just_finished() {
            // Cycle through frames 1..cols-1 (skip frame 0 = idle pose so the
            // whole-animation pause at frame 0 is gone; all frames are "alive").
            let max_frame = anim.cols.saturating_sub(1).max(1);
            anim.frame = (anim.frame % max_frame) + 1;
        }
        if let Some(ref mut atlas) = sprite.texture_atlas {
            atlas.index = anim.frame;
        }
    }
}

fn update_chest_sprites(
    mut commands: Commands,
    state: Res<MyPlayerState>,
    mut chests: Query<(Entity, &ChestSprite, &mut Visibility), Without<MonsterSprite>>,
    mut monsters: Query<(Entity, &MonsterSprite, &mut Visibility), Without<ChestSprite>>,
) {
    if !state.initialized { return; }
    for (entity, chest, mut vis) in &mut chests {
        let chest_id = format!("chest_{}", chest.0);
        if state.opened_chests.contains(&chest_id) {
            commands.entity(entity).despawn();
        } else {
            *vis = Visibility::Visible;
        }
    }
    for (entity, monster, mut vis) in &mut monsters {
        let monster_id = format!("monster_{}", monster.0);
        if state.defeated_monsters.contains(&monster_id) {
            commands.entity(entity).despawn();
        } else {
            *vis = Visibility::Visible;
        }
    }
}

fn handle_debug_menu(
    keys: Res<ButtonInput<KeyCode>>,
    mut debug: ResMut<DebugOptions>,
    mut fog: ResMut<FogOfWar>,
    mut commands: Commands,
    font: Res<GameFont>,
    time: Res<Time>,
    session: Res<GameSession>,
    world: Option<Res<WorldGrid>>,
    existing: Query<Entity, With<DebugMenuUi>>,
    mut poi_labels: Query<&mut Visibility, With<PoiLabel>>,
) {
    if keys.just_pressed(KeyCode::F3) { debug.show_menu = !debug.show_menu; }
    if !debug.show_menu { for e in &existing { commands.entity(e).despawn_recursive(); } return; }
    if keys.just_pressed(KeyCode::Digit1) { debug.fog_disabled = !debug.fog_disabled; fog.dirty = true; }
    if keys.just_pressed(KeyCode::Digit2) { debug.show_pois = !debug.show_pois; }
    for mut vis in &mut poi_labels { *vis = if debug.show_pois { Visibility::Visible } else { Visibility::Hidden }; }
    for e in &existing { commands.entity(e).despawn_recursive(); }
    let fps = (1.0 / time.delta_secs()).round() as u32;
    // World dims surface alongside the version so a stale-build /
    // wrong-world bug shows up here without diving into the console.
    let (ww, wh) = world
        .as_ref()
        .map(|w| (w.width, w.height))
        .unwrap_or((0, 0));
    let text = format!(
        "=== DEBUG (F3) ===\nClient v{} · seed {} · {}×{} · adv {}\nFPS: {}\n1: Fog [{}]\n2: POIs [{}]\nF4: Procedural ground [{}]\nF6: Lighting [{}]\nF7: Water shader [{}]\nF8: Debug sun [{}] ({:.1}, {:.1}, {:.1})\nF9: Show normals (water + shoreline bevel) [{}]\nF10: Show heightmap [{}]\nPgUp/PgDn: Tile Z factor [{:.2}]",
        crate::version::CLIENT_VERSION,
        session.map_seed,
        ww, wh,
        if session.player_id.is_empty() { "?" } else { "loaded" },
        fps,
        if debug.fog_disabled { "OFF" } else { "ON" },
        if debug.show_pois { "ON" } else { "OFF" },
        if debug.procedural_terrain_enabled { "ON" } else { "OFF" },
        if debug.lighting_enabled { "ON" } else { "OFF" },
        if debug.water_shader_enabled { "ON" } else { "OFF" },
        if debug.debug_sun_enabled { "ON" } else { "OFF" },
        debug.debug_sun_x, debug.debug_sun_y, debug.debug_sun_z,
        if debug.debug_show_normals { "ON" } else { "OFF" },
        if debug.debug_show_heightmap { "ON" } else { "OFF" },
        debug.tile_z_factor,
    );
    commands.spawn((Text::new(text), TextFont { font: font.0.clone(), font_size: 10.0, ..default() }, TextColor(Color::srgb(1.0, 1.0, 0.0)), Node { position_type: PositionType::Absolute, top: Val::Px(10.0), left: Val::Px(10.0), ..default() }, DebugMenuUi));
}

// ── Shop markers (TAB) ────────────────────────────────

/// A single shop as returned by GET /shops.
#[derive(serde::Deserialize, Clone)]
struct ShopMarker {
    id: String,
    name: String,
    tile_x: i32,
    tile_y: i32,
}

/// Polling state for the shops list. Refetches every 15 s so that visiting
/// a new shop makes its marker appear without needing a reload.
#[derive(Resource)]
struct ShopPollState {
    timer: f32,
    latest: std::sync::Arc<std::sync::Mutex<Option<Vec<ShopMarker>>>>,
}

impl Default for ShopPollState {
    fn default() -> Self {
        Self {
            // Fire the first fetch ~2 s after load so the player isn't
            // waiting for the world to appear.
            timer: 13.0,
            latest: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

fn poll_shops(
    mut commands: Commands,
    time: Res<Time>,
    mut poll: ResMut<ShopPollState>,
    session: Res<GameSession>,
    font: Res<GameFont>,
    existing: Query<(Entity, &ShopLabel)>,
) {
    // Refresh every 15 s. Cheap: small JSON list.
    poll.timer += time.delta_secs();
    if poll.timer >= 15.0 {
        poll.timer = 0.0;
        if !session.player_id.is_empty() {
            let url = format!("/shops?player_id={}", session.player_id);
            let slot = poll.latest.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let Ok(resp) = reqwest::Client::new().get(&url).send().await else { return };
                let markers: Vec<ShopMarker> = resp.json().await.unwrap_or_default();
                if let Ok(mut g) = slot.lock() { *g = Some(markers); }
            });
        }
    }

    // Consume the latest fetched list (if any). Reconcile spawned labels
    // with the target set — spawn new ones, leave existing ones in place.
    let Ok(mut guard) = poll.latest.lock() else { return };
    let Some(markers) = guard.take() else { return };
    let existing_ids: std::collections::HashSet<&str> =
        existing.iter().map(|(_, l)| l.0.as_str()).collect();
    // Despawn any that were removed (shouldn't happen in Phase A but
    // cheap to handle so Phase B's dynamic reveal/revoke "just works").
    let target_ids: std::collections::HashSet<&str> =
        markers.iter().map(|m| m.id.as_str()).collect();
    for (e, label) in &existing {
        if !target_ids.contains(label.0.as_str()) {
            commands.entity(e).despawn_recursive();
        }
    }
    for m in markers {
        if existing_ids.contains(m.id.as_str()) { continue; }
        let pos = WorldGrid::tile_to_world(m.tile_x as usize, m.tile_y as usize);
        // Bronze-ish color; offset below the POI label so they don't overlap
        // when TAB is held on a shop-in-a-town tile.
        commands.spawn((
            Text2d::new(format!("Shop: {}", m.name)),
            TextFont { font: font.0.clone(), font_size: 7.0, ..default() },
            TextColor(Color::srgb(0.85, 0.65, 0.25)),
            Transform::from_xyz(pos.x, pos.y - 20.0, 8.0),
            Visibility::Hidden,
            ShopLabel(m.id),
        ));
    }
}

fn toggle_shop_labels(
    keys: Res<ButtonInput<KeyCode>>,
    mut labels: Query<&mut Visibility, With<ShopLabel>>,
) {
    let show = keys.pressed(KeyCode::Tab);
    let want = if show { Visibility::Visible } else { Visibility::Hidden };
    for mut vis in &mut labels {
        if *vis != want { *vis = want; }
    }
}
