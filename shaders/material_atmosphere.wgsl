// Low-contrast drifting haze on the final composite. Periodic, one source sample.
// strength: haze opacity; color: haze tint; spacing: cloud scale in pixels.
fn haze_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let w = f * f * (vec2(3.0) - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), w.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), w.x), w.y);
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    let t = params.phase * 6.2831853;
    let p = uv * params.resolution / max(params.spacing, 16.0);
    let drift = vec2(cos(t), sin(t)) * 0.45;
    let n = 0.7 * haze_noise(p * vec2(0.65, 1.5) + drift)
          + 0.3 * haze_noise(p * 2.31 - drift + vec2(13.0));
    let aspect = params.resolution.x / params.resolution.y;
    let r = length((uv - vec2(0.5)) * vec2(aspect, 1.0));
    let veil = smoothstep(0.42, 0.8, n) * smoothstep(0.12, 0.75, r);
    let amount = clamp(params.strength, 0.0, 1.0) * veil;
    return vec4(mix(c.rgb, params.color.rgb * c.a, amount), c.a);
}
