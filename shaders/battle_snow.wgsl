// Soft, irregular snowfall. Each grid repeats exactly after integer-cell travel.
fn flakes(uv: vec2<f32>, cells: f32, travel: vec2<f32>, salt: f32) -> f32 {
    let aspect = params.resolution.x / params.resolution.y;
    let phase = params.phase * 6.2831853;
    let warp = vec2(sin(uv.y * 9.0 + phase + salt), cos(uv.x * 7.0 - phase + salt)) * 0.18;
    let p = uv * vec2(aspect, 1.0) * cells - travel * params.phase + warp;
    let cell = floor(p);
    let period = abs(travel);
    let wrapped = cell - floor(cell / period) * period;
    let h = hash(wrapped + vec2(salt, 37.0));
    let radius = mix(0.045, 0.19, hash(wrapped + vec2(17.0, salt)));
    let center = vec2(0.20) + 0.60 * vec2(hash(wrapped + vec2(salt, 3.0)), hash(wrapped + vec2(7.0, salt)));
    let d = length(fract(p) - center) / radius;
    let soft = 1.0 - smoothstep(0.0, 1.0, d);
    let density = 0.48 + 0.16 * sin(uv.x * 11.0 + uv.y * 7.0 + phase + salt);
    return soft * soft * mix(0.25, 1.0, h) * step(density, h);
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let base = sample_source(uv);
    let a = flakes(uv, 83.0, vec2(-13.0, 23.0), 11.0);
    let b = flakes(uv, 61.0, vec2(-9.0, 27.0), 23.0);
    let c = flakes(uv, 47.0, vec2(-17.0, 31.0), 47.0);
    let d = flakes(uv, 33.0, vec2(-11.0, 29.0), 71.0);
    let e = flakes(uv, 21.0, vec2(-19.0, 37.0), 97.0);
    let alpha = clamp((a * 0.15 + b * 0.20 + c * 0.24 + d * 0.27 + e * 0.30) * params.strength, 0.0, 1.0);
    return vec4(mix(base.rgb, vec3(0.68, 0.64, 0.65), alpha), base.a + alpha * (1.0 - base.a));
}
