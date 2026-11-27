// Smooth optical shading for pre-faceted crystal sprites.
// Preserves premultiplied alpha and existing facet colors.
// strength: contrast; spacing: optical variation scale in pixels;
// rotation: light direction; color: reflected light tint.
fn crystal_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let w = f * f * (vec2(3.0) - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), w.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), w.x), w.y);
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    let k = clamp(params.strength, 0.0, 1.0);
    let axis = vec2(cos(radians(params.rotation)), sin(radians(params.rotation)));
    let pixel = 1.0 / params.resolution;
    let p = rotate_point((uv - vec2(0.5)) * params.resolution, -radians(params.rotation))
        / max(params.spacing, 8.0);
    let broad = crystal_noise(p * vec2(1.6, 0.38) + vec2(17.0));
    let fine = crystal_noise(p * vec2(6.0, 0.72) + vec2(31.0, 9.0));
    let ribbon = pow(0.5 + 0.5 * cos(p.x * 4.0 + broad * 3.0 + p.y * 0.65), 5.0);
    let light_side = sample_source(uv + axis * pixel * 2.5).a;
    let dark_side = sample_source(uv - axis * pixel * 2.5).a;
    let rim = max(c.a - light_side, 0.0);
    let recess = max(c.a - dark_side, 0.0);
    let luminance = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
    let facet = smoothstep(0.025, 0.23, luminance / max(c.a, 0.001));
    let modulation = clamp(0.88 + (broad - 0.5) * 0.65 + (fine - 0.5) * 0.18
        + ribbon * 0.22 - recess * 0.18, 0.55, 1.32);
    let reflected = c.a * facet * (0.018 + ribbon * 0.065) + rim * 0.09;
    return vec4(c.rgb * mix(1.0, modulation, k) + params.color.rgb * reflected * k, c.a);
}
