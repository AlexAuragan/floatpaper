// Example custom effect. The shared interface is prepended by the engine.
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    let p = uv*2.0-1.0;
    let amount = clamp(params.strength,0.0,1.0);
    return vec4(c.rgb*(1.0-amount*smoothstep(0.1,1.7,dot(p,p))),c.a);
}
