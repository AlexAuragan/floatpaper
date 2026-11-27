// Luminance-dependent, monochromatic grain. Preserves alpha; no extra texture.
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    let cell = floor(uv * params.resolution / max(params.spacing, 1.0));
    let tick = floor(params.phase * 120.0);
    let grain = hash(cell + vec2(tick*17.0,tick*31.0)) - 0.5;
    let luma = clamp(dot(c.rgb,vec3(0.2126,0.7152,0.0722))/max(c.a,0.00001),0.0,1.0);
    let envelope = sqrt(luma*(1.0-luma));
    return vec4(max(c.rgb+vec3(grain*envelope*c.a*clamp(params.strength,0.0,1.0)*0.12),vec3(0.0)),c.a);
}
