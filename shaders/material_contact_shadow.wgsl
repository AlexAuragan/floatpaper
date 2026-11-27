// Inner-edge occlusion on an alpha-masked layer; not a cast shadow onto other layers.
// strength: shadow amount; spacing: radius in pixels; rotation: direction of light.
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    let angle = radians(params.rotation);
    let axis = vec2(cos(angle), sin(angle));
    let cross_axis = vec2(-axis.y, axis.x);
    let px = max(params.spacing, 1.0) / params.resolution;
    let near_a = sample_source(uv + axis * px * 0.35).a;
    let far_a = sample_source(uv + axis * px).a;
    let left_a = sample_source(uv + cross_axis * px * 0.65).a;
    let right_a = sample_source(uv - cross_axis * px * 0.65).a;
    let occlusion = clamp(c.a - (near_a * 0.35 + far_a * 0.35 + left_a * 0.15 + right_a * 0.15), 0.0, 1.0);
    return vec4(c.rgb * (1.0 - clamp(params.strength, 0.0, 1.0) * occlusion), c.a);
}
