// Fog of war shader. Reads a small 100×80 R8 mask (0 = fogged,
// 1 = revealed) sampled with linear filter — between texel centers
// the alpha smoothly interpolates, so revealed/unrevealed tile
// boundaries become a half-tile soft fade on each side instead of
// the previous hard pixel cliff.
//
// Plus a subtle shadow effect: revealed pixels look toward the sun
// and check if fog is between them and the light. If so, they're in
// the fog's shadow and get darkened. The illusion is that walls of
// unexplored fog cast soft shadows onto the explored ground in front
// of them, with the shadow direction shifting through the day as
// the sun arcs across the sky.
//
// Output: dark fog color with alpha = (1 - revealed) * max_alpha,
// plus shadow contribution on revealed pixels near fog edges.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct FogParams {
    // rgb = fog color, w = max alpha (1.0 for fully opaque fog).
    color: vec4<f32>,
    // xy = sun world px, z = day strength [0, 1], w unused.
    sun_pos: vec4<f32>,
    // x = mesh:world ratio, y = world_w_px, z = world_h_px,
    // w = effective fog height in world px (drives shadow length).
    world: vec4<f32>,
};

@group(2) @binding(0) var<uniform> params: FogParams;
@group(2) @binding(1) var mask_tex: texture_2d<f32>;
@group(2) @binding(2) var mask_sampler: sampler;

// Shadow tunables.
//
// SHADOW_SAMPLES = how many ray taps the shader fires toward the sun;
// the per-tap step is `params.world.w / SHADOW_SAMPLES` so the user
// controls effective fog height live with PageUp/PageDown without
// rebuilding the shader.
//
// SHARPNESS: we take the MAX fog density along the ray rather than
// summing per-sample contributions — a single fogged tap fully
// shadows the pixel, so the trailing edge of the shadow is a hard
// line where the ray stops finding fog. The (0.45, 0.55) smoothstep
// is tighter than the main fog edge (0.35, 0.65) so the shadow's
// own boundary stays crisp.
const SHADOW_SAMPLES: i32 = 6;
const SHADOW_MAX_ALPHA: f32 = 0.45;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let world_scale = params.world.x;
    let world_w = params.world.y;
    let world_h = params.world.z;

    // Map mesh UV (0..1 across the bigger mesh) → world UV (0..1
    // across just the world rectangle). Anything outside [0, 1]
    // is beyond the world bounds.
    let world_uv = (mesh.uv - vec2<f32>(0.5)) * world_scale + vec2<f32>(0.5);
    let in_world =
        world_uv.x >= 0.0 && world_uv.x <= 1.0 &&
        world_uv.y >= 0.0 && world_uv.y <= 1.0;

    // Outside the world: force fully fogged. Inside: read the mask.
    let raw = textureSample(mask_tex, mask_sampler, world_uv).r;
    let revealed = select(0.0, raw, in_world);

    // Smoothstep tightens the fade band — linear filter alone spreads
    // the transition across a full tile, which reads as "blurry"; the
    // (0.35, 0.65) thresholds compress most of the fade into the
    // central ~30% of one tile, keeping a soft edge but crisper.
    let revealed_sharp = smoothstep(0.35, 0.65, revealed);
    let fog_alpha = (1.0 - revealed_sharp) * params.color.a;

    // Shadow: only meaningful for revealed pixels (fogged ones are
    // already opaque), and only during day (sun above horizon).
    var shadow_strength = 0.0;
    let fog_height_px = params.world.w;
    if (params.sun_pos.z > 0.05 && in_world && revealed_sharp > 0.0 && fog_height_px > 0.0) {
        // Pixel position in world coords. World UV (0,0) is the
        // top-left of the world; +x is east, but +y is south in UV
        // while world Y goes negative as you go south — hence the
        // y-flip when converting back.
        let pixel_world = vec2<f32>(world_uv.x * world_w, -world_uv.y * world_h);
        let to_sun_world = normalize(params.sun_pos.xy - pixel_world);
        // Per-tap step length is total fog height divided by sample
        // count, so PgUp/PgDn changing fog_height_px immediately
        // reaches the rendered shadow (no shader rebuild needed).
        // y is flipped converting world offset back to UV.
        let step_px = fog_height_px / f32(SHADOW_SAMPLES);
        let step_uv = vec2<f32>(
            to_sun_world.x * step_px / world_w,
            -to_sun_world.y * step_px / world_h,
        );
        // Take the MAX fog density along the ray (not the sum) so a
        // single fogged tap fully occludes — produces a sharp trailing
        // edge instead of a soft additive gradient.
        var max_fog = 0.0;
        for (var i: i32 = 1; i <= SHADOW_SAMPLES; i = i + 1) {
            let s_uv = world_uv + step_uv * f32(i);
            let s_in =
                s_uv.x >= 0.0 && s_uv.x <= 1.0 &&
                s_uv.y >= 0.0 && s_uv.y <= 1.0;
            if (s_in) {
                let s_raw = textureSample(mask_tex, mask_sampler, s_uv).r;
                // Tighter smoothstep on the per-sample fog density so the
                // shadow's edge stays crisp even though the underlying
                // mask is bilinear-filtered.
                let s_fog = 1.0 - smoothstep(0.45, 0.55, s_raw);
                max_fog = max(max_fog, s_fog);
            }
        }
        shadow_strength = max_fog * SHADOW_MAX_ALPHA;
        // Fade with sun elevation: max contribution at noon, none at
        // horizon, none at night (already gated by the sun_pos.z > 0
        // check above, but this smooths the transition).
        shadow_strength *= clamp(params.sun_pos.z, 0.0, 1.0);
        // Only apply shadow on areas that are actually revealed —
        // multiplying by revealed_sharp avoids a visible "double
        // dose" right at the fog edge where fog_alpha is also rising.
        shadow_strength *= revealed_sharp;
    }

    let final_alpha = clamp(fog_alpha + shadow_strength, 0.0, 1.0);
    return vec4<f32>(params.color.rgb, final_alpha);
}
