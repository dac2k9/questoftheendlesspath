// Terrain lighting shader — GPU port of the former CPU F6 overlay.
//
// Samples a blurred per-tile heightmap, computes a per-pixel normal
// from gradient, runs Phong with the day/night cycle's live sun
// position, and outputs a DARKENING overlay (black RGB + alpha)
// so the whole world responds to sun direction as it moves across
// the sky. Never brightens past the base atlas — only darkens
// slopes facing away from the light.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct TerrainParams {
    time: f32,
    ambient: f32,
    max_alpha: f32,
    show_normals: f32,
    // xyz = sun world position + height, w = unused
    sun_pos: vec4<f32>,
    // xyz = highlight color for sun-facing slopes, w = unused.
    sun_tint: vec4<f32>,
    // 1.0 → render the raw heightmap as grayscale (F10 debug).
    show_heightmap: f32,
    // Multiplier on the heightmap gradient when deriving normals.
    // Higher = more dramatic slope shading. Live-tunable via
    // PageUp/PageDown on the client.
    height_amp: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(2) @binding(0) var<uniform> params: TerrainParams;
@group(2) @binding(1) var height_tex: texture_2d<f32>;
@group(2) @binding(2) var height_sampler: sampler;
@group(2) @binding(3) var water_dist_tex: texture_2d<f32>;
@group(2) @binding(4) var water_dist_sampler: sampler;

// Distance-field texture is WORLD_W*TILE_PX × WORLD_H*TILE_PX = 1600×1280,
// one byte per pixel encoding distance-to-water in pixels (clamped 0..255).
const WORLD_PX: vec2<f32> = vec2<f32>(1600.0, 1280.0);

// Shoreline bevel — single slope from flat (inland edge) to maximum
// tilt toward water (right at the shore). Models "land gradually
// rolls down into the water". Wider than the original 2.5 px so the
// gradient reads as a soft ramp instead of a one-pixel outline.
//
// (We tried a beach-berm two-slope variant but the inland uphill side
// produced a bright band the user's reference image didn't have.)
const SHORELINE_BEVEL_WIDTH_PX: f32 = 8.0;
const SHORELINE_MAX_ANGLE: f32 = 1.20; // ~69° — strong tilt at the water edge for a clearly read slope

// Sobel-ish central difference for normal derivation. Samples taken
// one heightmap-pixel to each side, so the gradient captures the
// proper slope at the current UV. The heightmap is 100×80 — UV
// spacing per texel is (1/100, 1/80).
fn compute_normal(uv: vec2<f32>) -> vec3<f32> {
    let step = vec2<f32>(1.0 / 100.0, 1.0 / 80.0);
    let hl = textureSample(height_tex, height_sampler, uv - vec2<f32>(step.x, 0.0)).r;
    let hr = textureSample(height_tex, height_sampler, uv + vec2<f32>(step.x, 0.0)).r;
    let hd = textureSample(height_tex, height_sampler, uv - vec2<f32>(0.0, step.y)).r;
    let hu = textureSample(height_tex, height_sampler, uv + vec2<f32>(0.0, step.y)).r;
    let dx = hr - hl;
    let dy = hu - hd;
    // AMP tuned for biome-height deltas (water=0 to mountain=1.0).
    // Higher = more pronounced slope shading. Driven by a uniform
    // (params.height_amp) so the user can tune live via PgUp/PgDn.
    let amp = params.height_amp;
    return normalize(vec3<f32>(-dx * amp, -dy * amp, 1.0));
}

// Sample the per-pixel water-distance texture (R8, normalized 0..1 in
// the shader where 1.0 == 255 pixels). Returns distance in pixels.
fn sample_water_dist_px(uv: vec2<f32>) -> f32 {
    return textureSample(water_dist_tex, water_dist_sampler, uv).r * 255.0;
}

// If the pixel is within SHORELINE_BEVEL_WIDTH_PX of water, override
// the heightmap-derived normal with one tilted toward the water. The
// direction TO water comes from the gradient of the distance field
// (normals naturally point from low distance to high distance; we
// negate to point toward water).
fn apply_shoreline_bevel(uv: vec2<f32>, base: vec3<f32>) -> vec3<f32> {
    let d_here = sample_water_dist_px(uv);
    if (d_here <= 0.0 || d_here >= SHORELINE_BEVEL_WIDTH_PX) {
        return base;
    }
    // Gradient via single-pixel neighbors (step = 1 / texture_size).
    let step = vec2<f32>(1.0 / WORLD_PX.x, 1.0 / WORLD_PX.y);
    let dl = sample_water_dist_px(uv - vec2<f32>(step.x, 0.0));
    let dr = sample_water_dist_px(uv + vec2<f32>(step.x, 0.0));
    let du = sample_water_dist_px(uv - vec2<f32>(0.0, step.y));
    let dd = sample_water_dist_px(uv + vec2<f32>(0.0, step.y));
    // Gradient of the distance field IN UV SPACE — points AWAY from
    // water (water has d=0, inland has d>0). UV.y is flipped vs world.y
    // in Bevy's Rectangle mesh (UV origin at top-left, world y grows
    // upward), so we un-flip the y component when converting to the
    // world-space direction we'll stuff into the normal.
    let grad_uv = vec2<f32>(dr - dl, dd - du);
    let len = length(grad_uv);
    if (len < 1e-5) { return base; }
    let to_water_uv = -grad_uv / len;
    let to_water_world = vec2<f32>(to_water_uv.x, -to_water_uv.y);

    // Quadratic ramp on `t` so the steepening is concentrated near
    // the water edge — gentle approach inland, sharp drop right at
    // the shore.
    let t_lin = 1.0 - d_here / SHORELINE_BEVEL_WIDTH_PX;
    let t = t_lin * t_lin;
    let theta = SHORELINE_MAX_ANGLE * t;

    // Rotate the BASE normal toward the water direction by `theta`.
    // Critical: this composes with the underlying terrain normal
    // instead of replacing it, so a hill/mountain near the shore
    // keeps its slope and just gets an *added* tilt toward water at
    // the bevel band. Without this, every coastline read as a flat
    // pre-tilted strip regardless of the land behind it (visible in
    // F9 normal-map view as a uniform purple band on every shore).
    //
    // The rotation axis is perpendicular to both `base` and the water
    // direction — derived via Gram-Schmidt to stay in the (base,
    // water-dir) plane.
    let v2 = vec3<f32>(to_water_world.x, to_water_world.y, 0.0);
    let perp = v2 - dot(base, v2) * base;
    let perp_len = length(perp);
    if (perp_len < 1e-5) {
        // base normal is already aligned with water direction (very
        // tilted heightmap that coincidentally points at water) — no
        // unique rotation axis. Skip the bevel rather than divide-by-0.
        return base;
    }
    let perp_n = perp / perp_len;
    return normalize(base * cos(theta) + perp_n * sin(theta));
}

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let uv = mesh.uv;
    // Debug: show the raw heightmap as grayscale (water=0/black,
    // mountain=1/white). Opaque — we're deliberately obscuring the
    // world to inspect the bake. Wins over `show_normals` so you
    // can toggle straight from one to the other.
    if (params.show_heightmap > 0.5) {
        let h = textureSample(height_tex, height_sampler, uv).r;
        return vec4<f32>(h, h, h, 1.0);
    }
    // Base normal from heightmap. Shoreline override layers on top
    // where water is nearby — gives the sharp 5-px bevel we had in
    // the CPU version while still letting mountains / hills get
    // proper slope shading elsewhere.
    var n = compute_normal(uv);
    n = apply_shoreline_bevel(uv, n);

    // Debug: show the normal as RGB, opaque. Flat (0,0,1) → (0.5,0.5,1.0)
    // purplish blue; the shoreline bevel tilts the XY components so the
    // 2.5-px coastal band reads as a vivid hue ring around every piece
    // of water. Return early — we don't want Phong on top of this.
    if (params.show_normals > 0.5) {
        return vec4<f32>(n.x * 0.5 + 0.5, n.y * 0.5 + 0.5, n.z * 0.5 + 0.5, 1.0);
    }

    // Per-pixel light direction toward the current sun/moon position.
    // Treating the light as a point light with the water surface at
    // z=0 — same convention as water_shader.
    let pixel_world = mesh.world_position.xy;
    let to_sun = vec3<f32>(
        params.sun_pos.x - pixel_world.x,
        params.sun_pos.y - pixel_world.y,
        params.sun_pos.z,
    );
    let sun = normalize(to_sun);

    let n_dot_l = max(0.0, dot(n, sun));
    let lit = params.ambient + n_dot_l * (1.0 - params.ambient);

    // Baseline lighting for a flat (0,0,1) surface under the same sun.
    // Slopes darker than flat get darkened (shadow side); slopes
    // brighter than flat get tinted toward the sun color (hillshade).
    // Flat land (gradient ≈ 0) produces zero alpha on both branches
    // so plains stay neutral — only real height deltas show up.
    let flat_dot = max(0.0, sun.z);
    let flat_lit = params.ambient + flat_dot * (1.0 - params.ambient);
    let shade  = max(0.0, flat_lit - lit);
    let bright = max(0.0, lit - flat_lit);

    if (shade >= bright) {
        let alpha = shade * params.max_alpha;
        return vec4<f32>(0.0, 0.0, 0.02, alpha);
    }
    // Highlight scaled a touch lower than shadow so the sunny side
    // doesn't wash out the pixel-art color underneath.
    let alpha = bright * params.max_alpha * 0.6;
    return vec4<f32>(params.sun_tint.rgb, alpha);
}
