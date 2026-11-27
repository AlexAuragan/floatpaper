// Domain-warped, low-frequency fog. All motion is periodic.
fn fog_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let w = f*f*(vec2(3.0)-2.0*f);
    return mix(mix(hash(i),hash(i+vec2(1.0,0.0)),w.x),
               mix(hash(i+vec2(0.0,1.0)),hash(i+vec2(1.0,1.0)),w.x),w.y);
}
fn fog_field(p: vec2<f32>) -> f32 {
    return 0.62*fog_noise(p)+0.26*fog_noise(p*2.03+vec2(9.0))+0.12*fog_noise(p*4.17+vec2(31.0));
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    let t = params.phase * 6.2831853;
    let p = rotate_point(uv*params.resolution/max(params.spacing,16.0),-radians(params.rotation));
    let drift = vec2(cos(t),sin(t))*0.55;
    let q = vec2(fog_noise(p+drift),fog_noise(p+vec2(19.0,8.0)-drift));
    let n = fog_field(p+q*2.0+drift);
    let a = clamp(params.strength*params.color.a*smoothstep(0.40,0.78,n),0.0,1.0);
    return vec4(c.rgb*(1.0-a)+params.color.rgb*a,c.a+a*(1.0-c.a));
}
