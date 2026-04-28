// Water shader — samples a pre-baked tileable normal map with two
// scrolling UV offsets, combines the two sampled normals, runs Phong
// with a point-light sun. F9 debug mode visualizes the normals as RGB
// (the classic blue-purple normal-map look).

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct WaterParams {
    time: f32,
    show_normals: f32,
    // Count of active entries in `lights`. Float (not u32) because
    // ShaderType + uniform-buffer layout plays nicer with 4x f32
    // packing than a mixed u32 / f32 struct.
    num_lights: f32,
    // Fade factor for lantern shimmer — 0 by day, ~1 at midnight.
    night_alpha: f32,
    sun_pos: vec4<f32>, // xyz = sun world position + height
    // Scene point lights (same set the darkness overlay carves with).
    // xyz = world position, w = radius. MUST match MAX_LIGHTS in
    // night_lights.rs / night_lights.wgsl.
    lights: array<vec4<f32>, 32>,
};

@group(2) @binding(0) var<uniform> params: WaterParams;
@group(2) @binding(1) var mask_tex: texture_2d<f32>;
@group(2) @binding(2) var mask_sampler: sampler;
@group(2) @binding(3) var normal_tex: texture_2d<f32>;
@group(2) @binding(4) var normal_sampler: sampler;

// Sample & unpack a normal-map pixel: rgb * 2 - 1 → (nx, ny, nz) in [-1..1].
fn sample_normal(uv: vec2<f32>) -> vec3<f32> {
    let s = textureSample(normal_tex, normal_sampler, uv).rgb;
    return s * 2.0 - 1.0;
}

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let uv = mesh.uv;
    // Mask: 0 = land (discard), ~0.25 = swamp, ~0.5 = snow/ice, ~1.0 = water.
    // Four-way encoding so this single shader handles all three
    // shimmer styles without spawning extra materials.
    let m = textureSample(mask_tex, mask_sampler, uv).r;
    if (m < 0.125) { discard; }
    let is_water = m > 0.75;
    let is_snow  = m > 0.375 && m <= 0.75;
    let is_swamp = m <= 0.375;

    // Per-biome render params. Water rolls fast with crisp glints,
    // snow is near-static with pinpoint glints, swamp is sluggish
    // (stagnant) with murky greenish reflections.
    var speed: f32;
    var spec_exp: f32;
    var spec_mult: f32;
    var diff_mult: f32;
    var tint: vec3<f32>;
    var alpha_cap: f32;
    if (is_water) {
        speed = 1.0;
        spec_exp = 80.0;
        spec_mult = 1.8;
        diff_mult = 0.08;
        tint = vec3<f32>(1.00, 0.98, 0.88); // warm-white
        alpha_cap = 0.95;
    } else if (is_swamp) {
        // Sluggish, broader/softer highlights, moss-yellow tint.
        // Diff_mult bumped up so the surface reads as a bulk liquid
        // (algae sheen) rather than just glints.
        speed = 0.05;
        spec_exp = 40.0;
        spec_mult = 0.55;
        diff_mult = 0.18;
        tint = vec3<f32>(0.65, 0.78, 0.30); // moss / algae green-yellow
        alpha_cap = 0.55;
    } else { // snow
        speed = 0.10;
        spec_exp = 220.0;
        spec_mult = 0.7;
        diff_mult = 0.02;
        tint = vec3<f32>(0.92, 0.96, 1.05); // cool-white
        alpha_cap = 0.45;
    }

    // Two sampling passes of the SAME normal-map texture, at
    // different scales + scrolling speeds. Sum + renormalize gives a
    // combined normal that never repeats visibly.
    let t = params.time;
    let uv1 = uv *  6.0 + vec2<f32>( t * 0.010 * speed,  t * 0.006 * speed);
    let uv2 = uv * 11.0 + vec2<f32>(-t * 0.0075 * speed, t * 0.014 * speed);
    let n1 = sample_normal(uv1);
    let n2 = sample_normal(uv2);
    let n  = normalize(n1 + n2);

    // Debug: render the normal directly as RGB to verify shape.
    if (params.show_normals > 0.5) {
        return vec4<f32>(n * 0.5 + 0.5, 1.0);
    }

    // ── Phong with a point-light sun ──
    // Per-pixel direction toward the sun, not directional — so moving
    // the F8 cursor visibly moves the highlight hotspot across the
    // surface like a lamp swept over the water.
    let pixel_world = mesh.world_position.xy;
    let to_sun = vec3<f32>(
        params.sun_pos.x - pixel_world.x,
        params.sun_pos.y - pixel_world.y,
        params.sun_pos.z,
    );
    let sun = normalize(to_sun);

    let view = vec3<f32>(0.0, 0.0, 1.0);
    let half_v = normalize(sun + view);

    let diffuse = max(0.0, dot(n, sun));
    let spec    = pow(max(0.0, dot(n, half_v)), spec_exp);

    // Overlay is additive — brightness grows with shimmer; alpha too.
    let shimmer = spec * spec_mult + diffuse * diff_mult;

    // ── Warm lantern shimmer from nearby player / POI lights ──
    // Each light is treated as a small lamp hovering ~30 px above the
    // water; specular highlight tightens as the ripple faces the lamp,
    // quadratic falloff inside the light's radius. Scaled by the
    // shared night_alpha so lanterns only contribute at night (the
    // sun's shimmer owns daytime).
    var lantern_rgb = vec3<f32>(0.0, 0.0, 0.0);
    let lantern_color = vec3<f32>(1.00, 0.68, 0.32);
    let lantern_height = 30.0;
    let n_lights = i32(params.num_lights);
    for (var i: i32 = 0; i < n_lights; i = i + 1) {
        let l = params.lights[i];
        let radius = l.w;
        if (radius <= 0.0) { continue; }
        let dx = l.x - pixel_world.x;
        let dy = l.y - pixel_world.y;
        let dist = sqrt(dx * dx + dy * dy);
        if (dist >= radius) { continue; }
        let to_l = normalize(vec3<f32>(dx, dy, lantern_height));
        let half_l = normalize(to_l + view);
        let d2 = max(0.0, dot(n, to_l));
        let s2 = pow(max(0.0, dot(n, half_l)), 60.0);
        let fall = 1.0 - dist / radius;
        let fall2 = fall * fall; // quadratic for a softer edge
        lantern_rgb = lantern_rgb + lantern_color * (s2 * 1.4 + d2 * 0.12) * fall2;
    }
    lantern_rgb = lantern_rgb * params.night_alpha;

    let rgb = tint * shimmer + lantern_rgb;
    // alpha_cap was set per-biome at the top so the underlying tile
    // texture still reads through (snow/swamp avoid bleaching).
    let alpha = min(max(shimmer * 0.9, (lantern_rgb.r + lantern_rgb.g + lantern_rgb.b) * 0.3), alpha_cap);
    return vec4<f32>(rgb, alpha);
}
