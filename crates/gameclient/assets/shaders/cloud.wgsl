// Cloud lighting — per-pixel normal derived from the cloud texture's
// own alpha gradient ("fluid border" technique). Soft puffy 3D look
// from a flat 2D sprite, with the sun lighting different sides.
// Inspired by fluid_fragment.fp the user shared: the alpha channel
// itself acts as a heightmap, and 4-direction sample differences give
// a tilt vector that we lift into a world-space normal.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct CloudParams {
    // rgb = sky-tint color (warm-white noon, amber dusk, cool blue night),
    // a   = base alpha multiplier (per-cloud density).
    tint: vec4<f32>,
    // xyz = sun world position, w = unused.
    sun_pos: vec4<f32>,
};

@group(2) @binding(0) var<uniform> params: CloudParams;
@group(2) @binding(1) var cloud_tex: texture_2d<f32>;
@group(2) @binding(2) var cloud_sampler: sampler;

// Cloud textures are 192×96 (see ambient.rs CLOUD_TEX_W/H). Hardcoded
// here because hooking up another uniform binding for two constants
// isn't worth the boilerplate. Bump this if the texture size changes.
const TEX_SIZE: vec2<f32> = vec2<f32>(192.0, 96.0);

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let uv = mesh.uv;
    let center = textureSample(cloud_tex, cloud_sampler, uv);
    // Hard-discard fully transparent texels — no normal to compute,
    // and avoids tinting blank fragments at the cloud's bbox edges.
    if (center.a < 0.01) { discard; }

    // 4-direction sample for the alpha gradient. Step = 1 texel in UV.
    let d = vec2<f32>(1.0) / TEX_SIZE;
    let a_n = textureSample(cloud_tex, cloud_sampler, uv - vec2<f32>(0.0, d.y)).a;
    let a_s = textureSample(cloud_tex, cloud_sampler, uv + vec2<f32>(0.0, d.y)).a;
    let a_w = textureSample(cloud_tex, cloud_sampler, uv - vec2<f32>(d.x, 0.0)).a;
    let a_e = textureSample(cloud_tex, cloud_sampler, uv + vec2<f32>(d.x, 0.0)).a;

    // The alpha falloff at cloud edges encodes "thickness" — high in
    // the dense middle, low at the wisps. Its gradient gives the slope
    // direction outward from dense → thin. AMP tuned so soft-fbm
    // edges produce a noticeable tilt without going horizontal.
    let amp = 6.0;
    let nx = (a_w - a_e) * amp;
    let ny_uv = (a_n - a_s) * amp;
    // Center alpha drives "puffy height" so the brightest top of the
    // cloud reads dome-like rather than flat. The +0.4 floor keeps
    // soft-edge fragments from collapsing the normal to (nx, ny, 0).
    let nz = center.a * 1.2 + 0.4;
    // UV.y grows downward; world.y grows upward — flip when packing
    // into a world-space normal so sun.y direction matches geometry.
    let normal = normalize(vec3<f32>(nx, -ny_uv, nz));

    // Lambertian + soft ambient fill. No specular — clouds are matte;
    // a tight highlight would read as wet/glossy and break the puffy
    // illusion. The 0.55 ambient keeps the shadow side from going
    // pitch black, which would clash with the rest of the lighting
    // (clouds are translucent in real life).
    let pixel_world = mesh.world_position.xy;
    let to_sun = normalize(vec3<f32>(
        params.sun_pos.x - pixel_world.x,
        params.sun_pos.y - pixel_world.y,
        params.sun_pos.z,
    ));
    let ambient = 0.55;
    let n_dot_l = max(0.0, dot(normal, to_sun));
    let lit = ambient + n_dot_l * (1.0 - ambient);

    let rgb = params.tint.rgb * lit;
    let alpha = center.a * params.tint.a;
    return vec4<f32>(rgb, alpha);
}
