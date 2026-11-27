fn line_mask(x: f32, spacing: f32, width: f32) -> f32 {
    let d = abs(fract(x / spacing + 0.5) - 0.5) * spacing;
    let aa = max(fwidth(x) * 0.5, 0.0001);

    return 1.0 - smoothstep(width - aa, width + aa, d);
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);

    // Pixel-space coordinates centered on screen.
    let pixel = (uv - vec2<f32>(0.5)) * params.resolution - params.offset;

    // Rotate the grid in pixel space.
    let angle = radians(params.rotation);
    let p = rotate_point(pixel, -angle);

    // User-configurable grid spacing in pixels.
    let spacing = params.spacing;
    let major_every = 4.0;

    let fine_width = 0.5;
    let major_width = 1.0;
    let axis_width = 1.5;

    let fine = max(
        line_mask(p.x, spacing, fine_width),
        line_mask(p.y, spacing, fine_width)
    );

    let major_spacing = spacing * major_every;

    let major = max(
        line_mask(p.x, major_spacing, major_width),
        line_mask(p.y, major_spacing, major_width)
    );

    let axis = max(
        1.0 - smoothstep(axis_width - 0.5, axis_width + 0.5, abs(p.x)),
        1.0 - smoothstep(axis_width - 0.5, axis_width + 0.5, abs(p.y))
    );

    let mask = max(
        fine * 0.35,
        max(major * 0.65, axis * 0.9)
    );

    // params.color is linear RGB already, with alpha from parse_color().
    let ink = params.color.rgb * c.a;
    let amount = clamp(mask * params.strength * params.color.a, 0.0, 1.0);

    let mixed_rgb = mix(c.rgb, ink, amount);

    return vec4(mixed_rgb, c.a);
}
