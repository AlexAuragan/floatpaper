fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c,vec3(0.0),vec3(1.0));
    return select(12.92*x,1.055*pow(x,vec3(1.0/2.4))-0.055,x>vec3(0.0031308));
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    // Non-sRGB swapchain formats require manual conversion.
    return vec4(select(c.rgb,linear_to_srgb(c.rgb),params.strength>0.5),1.0);
}
