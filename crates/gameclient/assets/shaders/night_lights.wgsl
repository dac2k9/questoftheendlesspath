// Night darkness with point lights.
//
// Covers the whole world with a dark overlay whose alpha is driven by
// the day/night cycle's night_alpha. Active lights (players, POIs at
// night) subtract radial contributions from the darkness — bright in
// the light's core, fading to zero at its radius. Result: full dark
// everywhere at midnight EXCEPT circles of illumination around each
// light source.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

const MAX_LIGHTS: u32 = 32u;

struct NightParams {
    night_alpha: f32,
    num_lights: u32,
    _pad0: f32,
    _pad1: f32,
    // Each light: xy = world position, z = radius in world px, w = reserved.
    lights: array<vec4<f32>, MAX_LIGHTS>,
};

@group(2) @binding(0) var<uniform> params: NightParams;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    if (params.night_alpha <= 0.0) {
        // Day — overlay invisible. (Saves the per-pixel loop work.)
        discard;
    }
    let pixel_world = mesh.world_position.xy;
    var darkness = params.night_alpha;
    // Lights CARVE out darkness. Each contributes up to its full
    // strength near its center, tapering with smoothstep to 0 at the
    // radius. Multiple overlapping lights stack additively, so a
    // tight group of players makes a brighter shared pool.
    for (var i: u32 = 0u; i < params.num_lights; i = i + 1u) {
        let l = params.lights[i];
        let d = distance(pixel_world, l.xy);
        let r = l.z;
        if (r <= 0.0) { continue; }
        let t = 1.0 - smoothstep(0.0, r, d);
        // 0.95 scale so even in the center there's a hint of dim —
        // keeps the art readable instead of washing out under the
        // torch.
        darkness = darkness - t * 0.95;
    }
    let a = max(0.0, darkness);
    if (a <= 0.001) { discard; }
    // Cool moonlit tint at night. The tint blends in proportional
    // to alpha (via normal alpha blending).
    return vec4<f32>(0.01, 0.02, 0.08, a);
}
