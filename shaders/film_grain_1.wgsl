// Independent integer-hashed pixel grain, with a soft luminance envelope.
fn grain_hash(v: u32) -> u32 {
    var x = v;
    x = (x ^ (x >> 16u)) * 0x7feb352du;
    x = (x ^ (x >> 15u)) * 0x846ca68bu;
    return x ^ (x >> 16u);
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    let cell = vec2<u32>(floor(uv * params.resolution / max(params.spacing, 1.0)));
    let tick = u32(floor(params.phase * 120.0));
    let seed = grain_hash(cell.x ^ grain_hash(cell.y ^ grain_hash(tick + 1u)));
    let grain = f32(seed >> 8u) / 16777216.0 - 0.5;
    let luma = clamp(dot(c.rgb, vec3(0.2126, 0.7152, 0.0722)) / max(c.a, 0.00001), 0.0, 1.0);
    let envelope = sqrt(luma * (1.0 - luma));
    return vec4(max(c.rgb + vec3(grain * envelope * c.a * clamp(params.strength, 0.0, 1.0) * 0.055), vec3(0.0)), c.a);
}
