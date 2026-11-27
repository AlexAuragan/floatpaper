// Analog signal interference: moving sync tear, line jitter and chroma drift.
// intensity: severity (0..1); spacing: scanline height in pixels; period: cycle.
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let k = clamp(params.strength, 0.0, 1.0);
    let tick = floor(params.phase * 96.0);
    let t = params.phase * 6.2831853;
    let row = floor(uv.y * params.resolution.y / max(params.spacing, 1.0));
    let jitter = hash(vec2(row, tick));
    let burst = smoothstep(0.64, 0.92, hash(vec2(floor(tick / 5.0), 13.0)));
    let tear = exp(-abs(uv.y - (0.5 + 0.42 * sin(t * 3.0))) * 85.0);
    let dx = ((jitter - 0.5) * step(0.91, jitter) * 0.025 + tear * sin(t * 17.0) * 0.045 * burst) * k;
    let p = clamp(uv + vec2(dx, 0.0), vec2(0.0), vec2(1.0));
    let split = vec2((0.8 + 6.0 * burst + 12.0 * tear) * k / params.resolution.x, 0.0);
    let c = sample_source(p);
    let red = sample_source(clamp(p + split, vec2(0.0), vec2(1.0)));
    let blue = sample_source(clamp(p - split, vec2(0.0), vec2(1.0)));
    let a = max(c.a, max(red.a, blue.a));
    return vec4(vec3(red.r, c.g, blue.b), a);
}
