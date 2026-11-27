fn pcg3d(v: vec3<u32>) -> u32 {
    var x = v.x * 1664525u + 1013904223u;
    var y = v.y * 1664525u + 1013904223u;
    var z = v.z * 1664525u + 1013904223u;

    x = x + y * z;
    y = y + z * x;
    z = z + x * y;

    x = x ^ (x >> 16u);
    y = y ^ (y >> 16u);
    z = z ^ (z >> 16u);

    x = x + y * z;
    y = y + z * x;
    z = z + x * y;

    return x ^ y ^ z;
}

// One 32-bit PCG hash supplies two 16-bit uniform samples. Keep the
// original weighted distribution without computing a second full hash.
fn grain(pixel: vec2<u32>, tick: u32) -> f32 {
    let bits = pcg3d(vec3<u32>(pixel, tick));
    let a = f32(bits & 0xFFFFu) + 0.5;
    let b = f32(bits >> 16u) + 0.5;
    return (0.72 * a + 0.28 * b) * (1.0 / 65536.0);
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    let pixel = vec2<u32>(floor(uv * params.resolution));

    // Identical temporal interpolation and wrap to the original shader.
    let tick = params.phase * 256.0;
    let frame_a = u32(floor(tick)) & 255u;
    let frame_b = (frame_a + 1u) & 255u;
    let f = smoothstep(0.0, 1.0, fract(tick));

    let n = mix(grain(pixel, frame_a), grain(pixel, frame_b), f) - 0.5;
    return vec4(
        clamp(c.rgb + n * params.strength * c.a, vec3(0.0), vec3(c.a)),
        c.a
    );
}
