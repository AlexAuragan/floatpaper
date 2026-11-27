fn effect(uv: vec2<f32>) -> vec4<f32> {
    let aspect = params.resolution.x / params.resolution.y;
    let p = (uv - vec2(0.5)) * vec2(aspect, 1.0);
    let r = length(p);
    let angle = atan2(p.y, p.x);
    let t = params.phase * 6.2831853;
    let rays = pow(0.5 + 0.5 * cos(angle * 9.0 + t + sin(r * 9.0)), 5.0);
    let haze = exp(-r * r * 20.0);
    let c = vec3(0.0007, 0.0015, 0.004) + vec3(0.0035, 0.012, 0.028) * haze * (0.82 + 0.08 * sin(t) + 0.10 * rays);
    return vec4(c, 1.0);
}
