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

fn random3(v: vec3<u32>) -> f32 {
    return f32(pcg3d(v)) * (1.0 / 4294967295.0);
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    let pixel = vec2<u32>(floor(uv * params.resolution));

    // Keep the loop seamless.
    let tick = params.phase * 256.0;
    let frame_a = u32(floor(tick));
    let frame_b = (frame_a + 1u) & 255u;
    let f = smoothstep(0.0, 1.0, fract(tick));

    // Mix two decorrelated hashes to reduce visible structure.
    let n0 = 0.72 * random3(vec3<u32>(pixel, frame_a))
           + 0.28 * random3(vec3<u32>(
                pixel.y * 3u + 11u,
                pixel.x * 5u + 17u,
                frame_a ^ 0x9E3779B9u
             ));

    let n1 = 0.72 * random3(vec3<u32>(pixel, frame_b))
           + 0.28 * random3(vec3<u32>(
                pixel.y * 3u + 11u,
                pixel.x * 5u + 17u,
                frame_b ^ 0x9E3779B9u
             ));

    let n = mix(n0, n1, f) - 0.5;
    return vec4(
        clamp(c.rgb + n * params.strength * c.a, vec3(0.0), vec3(c.a)),
        c.a
    );
}
