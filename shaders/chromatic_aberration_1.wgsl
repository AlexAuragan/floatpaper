// Radial lens dispersion, negligible at the center, strongest at corners.
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let p = uv-vec2(0.5)-params.offset/params.resolution;
    let aspect = params.resolution.x/params.resolution.y;
    let r2 = dot(p*vec2(aspect,1.0),p*vec2(aspect,1.0));
    let delta = p * r2 * clamp(params.strength,0.0,1.0) * 0.025;
    let c = sample_source(uv);
    let red = sample_source(clamp(uv+delta,vec2(0.0),vec2(1.0)));
    let blue = sample_source(clamp(uv-delta,vec2(0.0),vec2(1.0)));
    return vec4(vec3(red.r,c.g,blue.b),max(c.a,max(red.a,blue.a)));
}
