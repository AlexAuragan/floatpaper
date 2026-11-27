fn effect(uv: vec2<f32>) -> vec4<f32> {
    let t = params.phase * 6.2831853;
    let horizon = exp(-pow((uv.y - 0.68) * 3.2, 2.0));
    let p = uv * vec2(3.0, 5.0);
    let clouds = 0.5 + 0.5 * sin(p.y * 4.0 + sin(p.x * 2.4 + t) + 0.35 * sin(p.x * 5.0 - t));
    let shade = mix(vec3(0.012, 0.003, 0.009), vec3(0.27, 0.025, 0.023), horizon);
    let c = shade * (0.75 + 0.25 * clouds);
    return vec4(c, 1.0);
}
