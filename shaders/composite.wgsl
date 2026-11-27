fn effect(uv: vec2<f32>) -> vec4<f32> {
    return sample_source(uv)*params.strength;
}
