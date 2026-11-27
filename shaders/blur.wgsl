fn effect(uv: vec2<f32>) -> vec4<f32> {
    let angle = radians(params.rotation);

    let direction = vec2<f32>(
        cos(angle),
        sin(angle)
    );

    let d = direction * params.strength / params.resolution;

    return sample_source(uv) * 0.227027
        + (
            sample_source(uv + d * 1.384615)
            + sample_source(uv - d * 1.384615)
        ) * 0.316216
        + (
            sample_source(uv + d * 3.230769)
            + sample_source(uv - d * 3.230769)
        ) * 0.070270;
}
