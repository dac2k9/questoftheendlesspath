// Procedural ground — per-tile autotile with 8-neighbor lookup.
//
// For each fragment we determine which tile it belongs to and which
// quadrant of that tile (NW / NE / SW / SE). We then look at the 3
// outer neighbors meeting at that quadrant's outside corner. Only
// when ALL THREE outer neighbors share the SAME different-from-self
// biome do we draw a curve — that's an "outside corner" in autotile
// terms, the case where 3 neighbors converge to round our corner.
// Every other configuration renders flat (own tile content), so
// straight coastlines stay straight, mid-edge jogs don't become
// spurious curves, and only true outside corners get rounded.
//
// Trade-off vs the previous 2×2-cell approach: we lose the soft
// speckle at boundaries. Boundaries between dissimilar tiles are now
// pixel-art-hard. That's the same look the baked tilemap had before
// procedural mode, just with curved outside corners added.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct GroundParams {
    world_w: f32,
    world_h: f32,
    tile_px: f32,
    // 1.0 → flat-color test grid mode. Bypasses baked map sampling
    // and renders biome IDs directly via biome_color() so each
    // pattern in the F5 test layout is visually unambiguous.
    test_mode: f32,
    // xyz = sun world px (z = sun height), w unused.
    sun_pos: vec4<f32>,
    // xyz = warm/cool sky tint applied to sun-facing slopes.
    sun_tint: vec4<f32>,
    // 1.0 → run the in-shader Phong pass. F6 toggles via the client.
    lighting_enabled: f32,
    // Multiplier on the heightmap gradient when deriving the normal.
    // Linked to tile_z_factor (PgUp/PgDn) so the lighting strength
    // scales with the same knob that lifts polygon Z.
    height_amp: f32,
    ambient: f32,
    max_alpha: f32,
    show_normals: f32,
    show_heightmap: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(2) @binding(0) var<uniform> params: GroundParams;
@group(2) @binding(1) var biome_tex: texture_2d<f32>;
@group(2) @binding(2) var biome_sampler: sampler;
@group(2) @binding(3) var ground_tex: texture_2d<f32>;
@group(2) @binding(4) var ground_sampler: sampler;
@group(2) @binding(5) var overlays_tex: texture_2d<f32>;
@group(2) @binding(6) var overlays_sampler: sampler;
@group(2) @binding(7) var height_tex: texture_2d<f32>;
@group(2) @binding(8) var height_sampler: sampler;
@group(2) @binding(9) var water_dist_tex: texture_2d<f32>;
@group(2) @binding(10) var water_dist_sampler: sampler;

const SHORELINE_BEVEL_WIDTH_PX: f32 = 8.0;
const SHORELINE_MAX_ANGLE: f32 = 1.20;

fn compute_normal(uv: vec2<f32>) -> vec3<f32> {
    let step = vec2<f32>(1.0 / 100.0, 1.0 / 80.0);
    let hl = textureSample(height_tex, height_sampler, uv - vec2<f32>(step.x, 0.0)).r;
    let hr = textureSample(height_tex, height_sampler, uv + vec2<f32>(step.x, 0.0)).r;
    let hd = textureSample(height_tex, height_sampler, uv - vec2<f32>(0.0, step.y)).r;
    let hu = textureSample(height_tex, height_sampler, uv + vec2<f32>(0.0, step.y)).r;
    let dx = hr - hl;
    let dy = hu - hd;
    return normalize(vec3<f32>(-dx * params.height_amp, -dy * params.height_amp, 1.0));
}

fn sample_water_dist_px(uv: vec2<f32>) -> f32 {
    return textureSample(water_dist_tex, water_dist_sampler, uv).r * 255.0;
}

fn apply_shoreline_bevel(uv: vec2<f32>, base: vec3<f32>) -> vec3<f32> {
    let d_here = sample_water_dist_px(uv);
    if (d_here <= 0.0 || d_here >= SHORELINE_BEVEL_WIDTH_PX) {
        return base;
    }
    let world_px = vec2<f32>(params.world_w * params.tile_px, params.world_h * params.tile_px);
    let step = vec2<f32>(1.0 / world_px.x, 1.0 / world_px.y);
    let dl = sample_water_dist_px(uv - vec2<f32>(step.x, 0.0));
    let dr = sample_water_dist_px(uv + vec2<f32>(step.x, 0.0));
    let du = sample_water_dist_px(uv - vec2<f32>(0.0, step.y));
    let dd = sample_water_dist_px(uv + vec2<f32>(0.0, step.y));
    let grad_uv = vec2<f32>(dr - dl, dd - du);
    let len = length(grad_uv);
    if (len < 1e-5) { return base; }
    let to_water_uv = -grad_uv / len;
    let to_water_world = vec2<f32>(to_water_uv.x, -to_water_uv.y);
    let t_lin = 1.0 - d_here / SHORELINE_BEVEL_WIDTH_PX;
    let t = t_lin * t_lin;
    let theta = SHORELINE_MAX_ANGLE * t;
    let v2 = vec3<f32>(to_water_world.x, to_water_world.y, 0.0);
    let perp = v2 - dot(base, v2) * base;
    let perp_len = length(perp);
    if (perp_len < 1e-5) { return base; }
    let perp_n = perp / perp_len;
    return normalize(base * cos(theta) + perp_n * sin(theta));
}

// Apply Phong shading to the base biome color. Same math the old
// terrain_lighting overlay used: heightmap-derived normal, shoreline
// bevel, n·l with sky-tint highlight on bright slopes and a dark
// alpha on shaded slopes. Output is the modulated rgb.
fn apply_lighting(base_rgb: vec3<f32>, uv: vec2<f32>, world_pos: vec2<f32>) -> vec3<f32> {
    if (params.lighting_enabled < 0.5 || params.height_amp < 0.0001) {
        return base_rgb;
    }
    var n = compute_normal(uv);
    n = apply_shoreline_bevel(uv, n);
    let to_sun = vec3<f32>(
        params.sun_pos.x - world_pos.x,
        params.sun_pos.y - world_pos.y,
        params.sun_pos.z,
    );
    let sun = normalize(to_sun);
    let n_dot_l = max(0.0, dot(n, sun));
    let lit = params.ambient + n_dot_l * (1.0 - params.ambient);
    let flat_dot = max(0.0, sun.z);
    let flat_lit = params.ambient + flat_dot * (1.0 - params.ambient);
    let shade = max(0.0, flat_lit - lit);
    let bright = max(0.0, lit - flat_lit);
    if (shade >= bright) {
        let alpha = shade * params.max_alpha;
        return mix(base_rgb, vec3<f32>(0.0, 0.0, 0.02), alpha);
    }
    let alpha = bright * params.max_alpha * 0.6;
    return mix(base_rgb, params.sun_tint.rgb, alpha);
}

fn sample_biome_id(tx: f32, ty: f32) -> i32 {
    let cx = clamp(tx, 0.0, params.world_w - 1.0);
    let cy = clamp(ty, 0.0, params.world_h - 1.0);
    let uv = vec2<f32>((cx + 0.5) / params.world_w, (cy + 0.5) / params.world_h);
    let raw = textureSample(biome_tex, biome_sampler, uv).r;
    return i32(round(raw * 255.0));
}

// Flat color per biome ID. Used by F5 test-grid mode so each biome
// reads as an unambiguous block of color, no actual tilemap content.
fn biome_color(id: i32) -> vec3<f32> {
    if (id == 0) { return vec3<f32>(0.30, 0.55, 0.72); }   // Water
    if (id == 1) { return vec3<f32>(0.18, 0.40, 0.62); }   // DeepWater
    if (id == 2) { return vec3<f32>(0.30, 0.38, 0.28); }   // Swamp
    if (id == 3) { return vec3<f32>(0.85, 0.78, 0.55); }   // Desert / sand
    if (id == 4) { return vec3<f32>(0.55, 0.70, 0.40); }   // Grassland
    if (id == 5) { return vec3<f32>(0.42, 0.55, 0.30); }   // Forest
    if (id == 6) { return vec3<f32>(0.32, 0.45, 0.25); }   // DenseForest
    if (id == 7) { return vec3<f32>(0.55, 0.50, 0.45); }   // Mountain
    if (id == 8) { return vec3<f32>(0.95, 0.96, 0.99); }   // Snow / ice
    return vec3<f32>(1.0, 0.0, 1.0); // sentinel pink
}

// Curve radius. 0.35 = ~5px cut-off triangle per corner, visible
// without being dramatic. 0.45 is too aggressive for normal play.
const CURVE_RADIUS: f32 = 0.35;
const CURVE_RADIUS_SQ: f32 = CURVE_RADIUS * CURVE_RADIUS;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let px = mesh.world_position.x;
    let py = mesh.world_position.y;

    // Tile this fragment falls into. Tile centers sit at integer
    // multiples of TILE_PX in world coords, so rounding gives the
    // owning tile's coords.
    let tile_xf = round(px / params.tile_px);
    let tile_yf = round(-py / params.tile_px);

    // Local position within the tile, [0, 1] with (0,0) = NW corner.
    let lx = (px / params.tile_px - tile_xf) + 0.5;
    let ly = (-py / params.tile_px - tile_yf) + 0.5;

    // Direction sign for "outer neighbor" of this fragment's quadrant.
    // NW quadrant (lx < 0.5, ly < 0.5) → look NW (-1, -1).
    // NE → (+1, -1), SW → (-1, +1), SE → (+1, +1).
    let nx = select(-1.0, 1.0, lx >= 0.5);
    let ny = select(-1.0, 1.0, ly >= 0.5);

    // Sample self + all 8 neighbors. Used both for the outside-corner
    // test (which looks at the 3 outer neighbors of the fragment's
    // quadrant) and for the same-biome count below.
    let self_id = sample_biome_id(tile_xf, tile_yf);
    let n_id  = sample_biome_id(tile_xf,        tile_yf - 1.0);
    let s_id  = sample_biome_id(tile_xf,        tile_yf + 1.0);
    let w_id  = sample_biome_id(tile_xf - 1.0,  tile_yf);
    let e_id  = sample_biome_id(tile_xf + 1.0,  tile_yf);
    let nw_id = sample_biome_id(tile_xf - 1.0,  tile_yf - 1.0);
    let ne_id = sample_biome_id(tile_xf + 1.0,  tile_yf - 1.0);
    let sw_id = sample_biome_id(tile_xf - 1.0,  tile_yf + 1.0);
    let se_id = sample_biome_id(tile_xf + 1.0,  tile_yf + 1.0);

    // Pick the 3 neighbors at the corner from the right quadrant.
    let cardinal_x_id = select(w_id, e_id, nx > 0.0);
    let cardinal_y_id = select(n_id, s_id, ny > 0.0);
    let diagonal_id   = select(
        select(nw_id, ne_id, nx > 0.0),
        select(sw_id, se_id, nx > 0.0),
        ny > 0.0,
    );

    // Outside-corner test: ALL 3 outer neighbors share a single
    // biome that's NOT our own.
    let outside_corner = (cardinal_x_id == cardinal_y_id)
                      && (cardinal_x_id == diagonal_id)
                      && (cardinal_x_id != self_id);

    // Rounding rule combines two cases:
    //
    // (a) WATER LEAKS INTO ANY CORNER. If the surrounding biome at
    //     this outside corner is water (or deep-water), self rounds
    //     regardless of biome or same_count — water flows around any
    //     convex corner sticking out into it (grass / sand / etc.).
    //
    // (b) FLUID-SELF SMALL FEATURE. A small fluid body (water /
    //     snow / swamp) rounds its OWN outer corners as long as
    //     same_count <= 3. Lets isolated ponds and 2×2 clusters
    //     read as soft natural features.
    //
    // Anything else stays square — most importantly, a non-fluid
    // self with a non-water surrounding biome doesn't round (this
    // is what avoids ice/snow leaking into road columns).
    //
    // Biome IDs match procedural_ground.rs::generate_biome_texture:
    //   0 Water · 1 DeepWater · 2 Swamp · 8 Snow → fluid
    //   3 Desert · 4 Grassland · 5 Forest · 6 DenseForest · 7 Mountain → rigid
    let same_count =
        select(0, 1, n_id  == self_id) +
        select(0, 1, s_id  == self_id) +
        select(0, 1, w_id  == self_id) +
        select(0, 1, e_id  == self_id) +
        select(0, 1, nw_id == self_id) +
        select(0, 1, ne_id == self_id) +
        select(0, 1, sw_id == self_id) +
        select(0, 1, se_id == self_id);
    let self_is_fluid =
        self_id == 0 || self_id == 1 || self_id == 2 || self_id == 8;
    // Water, deep-water, and swamp all "flow" — they round any
    // adjacent biome's convex corner. Snow does not (otherwise ice
    // leaks into road columns).
    let surrounding_flows =
        cardinal_x_id == 0 || cardinal_x_id == 1 || cardinal_x_id == 2;
    let self_is_small_feature =
        surrounding_flows || (self_is_fluid && same_count <= 3);

    // Self renders as a rounded square. The corner-rounding arc is a
    // quarter-circle centered on the INSET point (r away from each
    // edge that touches the corner). Points outside this inset-circle
    // AND in the corner zone fall in the small triangle that gets
    // replaced by the neighbor's biome.
    //
    // (Previous version centered the arc on the tile corner itself
    // — that bowed the boundary outward and ate a much larger arc
    // out of self, making isolated tiles look like circles instead
    // of softened squares.)
    let corner_x = select(1.0, 0.0, nx < 0.0);
    let corner_y = select(1.0, 0.0, ny < 0.0);
    let inset_x = corner_x - nx * CURVE_RADIUS;
    let inset_y = corner_y - ny * CURVE_RADIUS;
    let dx = lx - inset_x;
    let dy = ly - inset_y;
    let dist_sq = dx * dx + dy * dy;

    // Corner-zone test: are both lx and ly within `r` of the tile
    // corner? Outside this band, we're along a straight edge or in
    // the tile interior — render self regardless.
    let in_corner_zone =
        abs(lx - corner_x) < CURVE_RADIUS &&
        abs(ly - corner_y) < CURVE_RADIUS;

    var sample_uv = mesh.uv;
    if (outside_corner && self_is_small_feature && in_corner_zone && dist_sq > CURVE_RADIUS_SQ) {
        // All 3 outer neighbors share the same biome AND self is a
        // small feature (not embedded in a larger body) — round the
        // corner. Sample diagonally; the diagonal tile is the same
        // biome as the cardinals by the outside_corner test, so any
        // direction works and diagonal preserves tile-variant flow.
        sample_uv = mesh.uv + vec2<f32>(nx / params.world_w, ny / params.world_h);
    }

    // F10 debug: heightmap as grayscale. Wins over everything else.
    if (params.show_heightmap > 0.5) {
        let h = textureSample(height_tex, height_sampler, mesh.uv).r;
        return vec4<f32>(h, h, h, 1.0);
    }
    // F9 debug: per-pixel normal as RGB.
    if (params.show_normals > 0.5) {
        var n = compute_normal(mesh.uv);
        n = apply_shoreline_bevel(mesh.uv, n);
        return vec4<f32>(n.x * 0.5 + 0.5, n.y * 0.5 + 0.5, n.z * 0.5 + 0.5, 1.0);
    }

    if (params.test_mode > 0.5) {
        // F5 test-grid mode: render the biome at the (potentially
        // shifted) sample position as a flat color. Sample the biome
        // texture DIRECTLY at sample_uv — going through
        // sample_biome_id() would add +0.5 to a fractional texel
        // coord and shift the result by 1 tile.
        let raw = textureSample(biome_tex, biome_sampler, sample_uv).r;
        let id = i32(round(raw * 255.0));
        return vec4<f32>(biome_color(id), 1.0);
    }

    let ground = textureSample(ground_tex, ground_sampler, sample_uv);
    let ov = textureSample(overlays_tex, overlays_sampler, mesh.uv);
    let rgb = ground.rgb * (1.0 - ov.a) + ov.rgb * ov.a;
    let lit = apply_lighting(rgb, mesh.uv, mesh.world_position.xy);
    return vec4<f32>(lit, 1.0);
}
