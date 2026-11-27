fn effect(uv: vec2<f32>) -> vec4<f32> {
    let k = clamp(params.strength, 0.0, 1.0);
    let phase = params.phase * 6.2831853;
    let angle = radians(params.rotation);

    // Rotate the coordinates used to generate glitch bands.
    let pixel = (uv - vec2<f32>(0.5)) * params.resolution;
    let local = rotate_point(pixel, -angle);

    let band = floor(
        (local.y / params.resolution.y + 0.5) * 32.0
    );

    // Direction of displacement.
    let direction = vec2<f32>(
        cos(angle),
        sin(angle)
    );

    let burst = select(pow(max(0.0, sin(phase)), 12.0), 1.0, GLITCH_CONTINUOUS);
    // Smoothly vary each band's displacement over the existing periodic
    // phase, without resetting the pattern or gating its amplitude.
    let a = hash(vec2(band, 11.0)) - 0.5;
    let b = hash(vec2(band, 29.0)) - 0.5;
    let shift = select(a, mix(a, b, 0.5 - 0.5 * cos(phase)), GLITCH_CONTINUOUS);

    let displacement = (shift)
        * 0.18 * k * burst * params.resolution.x;

    let p = uv + direction * displacement / params.resolution;

    // Chromatic aberration follows the same orientation.
    let split_px = k * (0.001 + 0.008 * burst)
        * params.resolution.x;

    let split = direction * split_px / params.resolution;

    let r = sample_source(p + split);
    let g = sample_source(p);
    let b = sample_source(p - split);

    return vec4(
        r.r,
        g.g,
        b.b,
        max(r.a, max(g.a, b.a))
    );
}
