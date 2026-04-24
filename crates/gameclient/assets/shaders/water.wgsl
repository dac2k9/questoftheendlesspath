// Water ripple shader — Tier-2 live animation for the F7 overlay. v2
//
// Samples a small water-mask texture (1 px per tile, R = 1 for water)
// and discards non-water pixels. For water pixels, builds a normal from
// two noise layers scrolling at different speeds/directions, runs a
// cheap Blinn-Phong, and outputs an ADDITIVE shimmer over the existing
// atlas water tile — not replacing the tile color. Alpha blending mode
// on the material makes it composite on top.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct WaterParams {
    // `time` is the only value we actually vary per frame.
    time: f32,
    // std140 scalar → vec4 alignment pad. Bevy's AsBindGroup packs this
    // into a 16-byte uniform block.
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(2) @binding(0) var<uniform> params: WaterParams;
@group(2) @binding(1) var mask_tex: texture_2d<f32>;
@group(2) @binding(2) var mask_sampler: sampler;

// Fixed sun matching the CPU lighting overlay.
const SUN_DIR: vec3<f32> = vec3<f32>(-0.65, -0.65, 0.80);

fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    p3 = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Approximate surface normal from a noise height field's gradient.
// Uses finite differences — one sample at p, two at offsets, derive ∂h/∂x, ∂h/∂y.
fn layer_normal(uv: vec2<f32>, scale: f32, drift: vec2<f32>, t: f32) -> vec3<f32> {
    let p = uv * scale + drift * t;
    let eps: f32 = 0.08;
    let h  = value_noise(p);
    let hx = value_noise(p + vec2<f32>(eps, 0.0));
    let hy = value_noise(p + vec2<f32>(0.0, eps));
    let dx = (hx - h) / eps;
    let dy = (hy - h) / eps;
    // Amplitude factor keeps normals mostly upward (small XY tilt) so
    // waves look like ripples, not vertical cliffs.
    return vec3<f32>(-dx * 0.4, -dy * 0.4, 1.0);
}

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let uv = mesh.uv;
    // Mask: 1 = water tile, 0 = land. Sampler is NEAREST so there's
    // no bleed onto land near the shore.
    let m = textureSample(mask_tex, mask_sampler, uv).r;
    if (m < 0.5) { discard; }

    let t = params.time;
    // Two scrolling layers — different scales, opposing directions,
    // so the combined pattern never visibly repeats.
    let n1 = layer_normal(uv, 120.0, vec2<f32>( 0.08,  0.03), t);
    let n2 = layer_normal(uv, 180.0, vec2<f32>(-0.06,  0.09), t);
    let n  = normalize(n1 * 0.5 + n2 * 0.5);

    let sun = normalize(SUN_DIR);
    let diffuse = max(0.0, dot(n, sun));

    // Blinn-Phong specular with a top-down view vector.
    let view = vec3<f32>(0.0, 0.0, 1.0);
    let half_v = normalize(sun + view);
    let spec = pow(max(0.0, dot(n, half_v)), 40.0);

    // Additive shimmer: bright near spec highlights, faintly tinted by
    // diffuse. Alpha grows with both so the shimmer composites as a
    // localized bright patch rather than a uniform overlay.
    let shimmer = spec * 0.85 + diffuse * 0.10;
    let tint = vec3<f32>(1.00, 1.00, 0.92);
    return vec4<f32>(tint * shimmer, shimmer * 0.80);
}
