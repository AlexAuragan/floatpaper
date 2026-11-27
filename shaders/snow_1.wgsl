// Wind-driven falling particles in a sheared coordinate system.
// Each column has its own speed, size and lateral flutter. Integer travel
// through a periodic vertical lattice makes the effect seamlessly repeat.
fn snowfall(uv: vec2<f32>, density: f32, salt: f32, wind: f32) -> f32 {
    let p = uv * vec2(params.resolution.x / params.resolution.y, 1.0) * density;
    let shear = vec2(p.x - wind * p.y, p.y);
    let column = floor(shear.x);
    let speed = 35.0 + floor(hash(vec2(column, salt)) * 38.0);
    let yy = shear.y - params.phase * speed;
    let row = floor(yy);
    let key = vec2(column, row - floor(row / speed) * speed);
    let seed = hash(key + vec2(salt, 17.0));
    let flutter = 0.12 * sin(params.phase * 6.2831853 * 3.0 + seed * 31.0);
    let center = vec2(0.24 + 0.52 * hash(key + vec2(23.0, salt)) + flutter,
                      0.23 + 0.54 * hash(key + vec2(salt, 61.0)));
    let delta = vec2(fract(shear.x), fract(yy)) - center;
    let radius = mix(0.035, 0.105, seed * seed);
    let d = length(vec2(delta.x, delta.y * mix(0.48, 0.92, seed))) / radius;
    let flake = 1.0 - smoothstep(0.05, 1.0, d);
    return flake * flake * mix(0.18, 0.9, seed) * step(0.35, seed);
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let base = sample_source(uv);
    let wind = clamp(tan(radians(params.rotation)), -0.8, 0.8);
    let scale = 48.0 / max(params.spacing, 12.0);
    let fine = snowfall(uv, 83.0 * scale, 11.0, wind);
    let mid = snowfall(uv, 49.0 * scale, 37.0, wind);
    let close = snowfall(uv, 25.0 * scale, 79.0, wind);
    let alpha = clamp((fine * 0.30 + mid * 0.48 + close * 0.56) * params.strength * params.color.a, 0.0, 1.0);
    return vec4(base.rgb * (1.0 - alpha) + params.color.rgb * alpha, base.a + alpha * (1.0 - base.a));
}
