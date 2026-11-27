fn effect(uv: vec2<f32>) -> vec4<f32> {
    let k = clamp(params.strength, 0.0, 1.0);
    let p = uv * 2.0 - 1.0;

    // Boundary-preserving distortion.
    let curvature = 0.045 * k;
    let r2 = dot(p, p);
    let boundary = vec2<f32>(1.0) - p * p;

    let distorted = p + p * curvature * r2 * boundary;
    let warped = distorted * 0.5 + 0.5;

    let c = sample_source(warped);

    // Rotate scanlines in pixel space.
    let pixel = (uv - vec2<f32>(0.5)) * params.resolution;
    let angle = radians(params.rotation);
    let scan_pixel = rotate_point(pixel, -angle);

    let scan_position = scan_pixel.y + params.resolution.y * 0.5;

    let scan = 1.0 - 0.32 * k * (
        0.5 + 0.5 * cos(
            6.2831853 * scan_position / 3.0
        )
    );

    // Vignette.
    let vignette = 1.0 - 0.35 * k
        * smoothstep(0.2, 2.0, r2);

    return vec4(c.rgb * scan * vignette, c.a);
}
