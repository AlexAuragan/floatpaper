// Fine signal tearing: sparse thin lines and occasional wider tears.
// Maximum displacement is 0.015 * k * width; RGB split <= 0.002 * k * width.
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let k = clamp(params.strength, 0.0, 1.0);
    let phase = params.phase * 6.2831853;
    let angle = radians(params.rotation);
    let pixel = (uv - vec2<f32>(0.5)) * params.resolution;
    let local = rotate_point(pixel, -angle);
    let direction = vec2<f32>(cos(angle), sin(angle));
    let y = local.y / params.resolution.y + 0.5;
    let line = floor(y * 216.0);
    let tear = floor(y * 54.0);
    let blend = select(0.0, 0.5 - 0.5 * cos(phase), GLITCH_CONTINUOUS);
    let burst = select(pow(max(0.0, sin(phase)), 12.0), 1.0, GLITCH_CONTINUOUS);

    // Stable masks, smoothly varying signed shifts. A few neighboring fine
    // lines share a wider tear, while most of the image remains untouched.
    let line_mask = step(0.82, hash(vec2(line, 41.0)));
    let tear_mask = step(0.96, hash(vec2(tear, 73.0)));
    let line_shift = mix(hash(vec2(line, 11.0)), hash(vec2(line, 29.0)), blend) * 2.0 - 1.0;
    let tear_shift = mix(hash(vec2(tear, 17.0)), hash(vec2(tear, 53.0)), blend) * 2.0 - 1.0;
    let active = max(line_mask, tear_mask);
    let shift = select(line_shift * 0.006 * line_mask, tear_shift * 0.015, tear_mask > 0.0);
    let displacement = shift * k * burst * params.resolution.x;
    let split_px = active * k * burst * 0.002 * params.resolution.x;
    let axis = direction / params.resolution;
    let p = uv + axis * displacement;
    let split = axis * split_px;
    let red = sample_source(p + split);
    let green = sample_source(p);
    let blue = sample_source(p - split);
    return vec4(red.r, green.g, blue.b, max(red.a, max(green.a, blue.a)));
}
