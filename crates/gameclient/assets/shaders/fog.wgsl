// Fog of war shader. Reads a small 100×80 R8 mask (0 = fogged,
// 1 = revealed) sampled with linear filter — between texel centers
// the alpha smoothly interpolates, so revealed/unrevealed tile
// boundaries become a half-tile soft fade on each side instead of
// the previous hard pixel cliff.
//
// Output: dark fog color with alpha = (1 - revealed) * max_alpha.
// max_alpha lets us tune fog opacity centrally.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct FogParams {
    // rgb = fog color, w = max alpha (1.0 for fully opaque fog).
    color: vec4<f32>,
    // Mesh-size : world-size ratio. The mesh is bigger than the
    // world so fog covers the area outside the world rectangle when
    // the camera zooms out.
    world_scale: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(2) @binding(0) var<uniform> params: FogParams;
@group(2) @binding(1) var mask_tex: texture_2d<f32>;
@group(2) @binding(2) var mask_sampler: sampler;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    // Map mesh UV (0..1 across the bigger mesh) → world UV (0..1
    // across just the world rectangle). Anything outside [0, 1]
    // is beyond the world bounds.
    let world_uv = (mesh.uv - vec2<f32>(0.5)) * params.world_scale + vec2<f32>(0.5);
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
    return vec4<f32>(params.color.rgb, fog_alpha);
}
