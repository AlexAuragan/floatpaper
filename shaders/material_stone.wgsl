// Static screen-space stone shading. Preserves the source alpha mask.
// strength: material contrast; spacing: coarse grain size in pixels;
// rotation: light direction in degrees; color: optional reflected light tint.
fn stone_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let w = f * f * (vec2(3.0) - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), w.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), w.x), w.y);
}
fn stone_height(p: vec2<f32>) -> f32 {
    return 0.58 * stone_noise(p) + 0.28 * stone_noise(p * 2.13 + vec2(19.0))
        + 0.14 * stone_noise(p * 5.37 + vec2(7.0, 23.0));
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    let k = clamp(params.strength, 0.0, 1.0);
    let p = uv * params.resolution / max(params.spacing, 4.0);
    let angle = radians(params.rotation);
    let light_dir = vec2(cos(angle), sin(angle));
    let h = stone_height(p);
    let slope = stone_height(p + light_dir * 0.075) - h;
    let grain = stone_noise(p * 19.0 + vec2(37.0));
    let bands = stone_noise(rotate_point(p, 0.37) * vec2(0.45, 3.6));
    let material = clamp(0.77 + (h - 0.5) * 1.15 + slope * 2.1
        + (grain - 0.5) * 0.13 + (bands - 0.5) * 0.23, 0.30, 1.35);
    let aspect = params.resolution.x / params.resolution.y;
    let distance_to_light = length((uv - vec2(0.5) - params.offset / params.resolution) * vec2(aspect, 1.0));
    let bounce = exp(-distance_to_light * 3.8) * 0.10 * k;
    return vec4(c.rgb * mix(1.0, material, k) + params.color.rgb * c.a * bounce, c.a);
}
