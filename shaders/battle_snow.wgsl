// Three aspect-correct particle grids, periodic advection, one source sample.
fn flakes(uv: vec2<f32>, cells: f32, travel: vec2<f32>, radius: f32, salt: f32) -> f32 {
    let aspect = params.resolution.x / params.resolution.y;
    let p = uv * vec2(aspect, 1.0) * cells - travel * params.phase;
    let cell = floor(p);
    let wrapped = cell - floor(cell / travel) * travel;
    let center = vec2(0.22, 0.22) + 0.56 * vec2(hash(wrapped + vec2(salt, 3.0)), hash(wrapped + vec2(7.0, salt)));
    let d = length(fract(p) - center);
    return (1.0 - smoothstep(radius * 0.45, radius, d)) * step(0.48, hash(wrapped + vec2(salt, 37.0)));
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let base = sample_source(uv);
    let far = flakes(uv, 72.0, vec2(-9.0, 24.0), 0.105, 11.0);
    let mid = flakes(uv, 42.0, vec2(-12.0, 30.0), 0.115, 23.0);
    let near = flakes(uv, 22.0, vec2(-15.0, 36.0), 0.12, 47.0);
    let alpha = clamp((far * 0.22 + mid * 0.42 + near * 0.65) * params.strength, 0.0, 1.0);
    return vec4(mix(base.rgb, vec3(0.88, 0.85, 0.84), alpha), base.a + alpha * (1.0 - base.a));
}
