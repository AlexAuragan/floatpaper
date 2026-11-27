// Dark, wind-scoured earth and ash. Preserve weapon silhouettes using
// their darker source colors. All calculations use linear premultiplied RGBA.
fn ground_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let w = f * f * (vec2(3.0) - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), w.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), w.x), w.y);
}
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_source(uv);
    let k = clamp(params.strength, 0.0, 1.0);
    let p = rotate_point(uv * params.resolution, -radians(params.rotation)) / max(params.spacing, 4.0);
    let broad = ground_noise(p * vec2(0.55, 1.7));
    let ridges = ground_noise(p * vec2(0.9, 6.0) + vec2(19.0));
    let grit = ground_noise(p * 15.0 + vec2(31.0, 7.0));
    let luma = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722)) / max(c.a, 0.001);
    // The current battlefield SVGs use near-black weapons and lighter soil.
    // Fade the material out on the darkest pixels, avoiding bright weapon edges.
    let soil = smoothstep(0.0038, 0.0058, luma);
    let variation = clamp(0.95 + (broad - 0.5) * 1.35 + (ridges - 0.5) * 0.48
        + (grit - 0.5) * 0.28, 0.38, 1.55);
    let dust = smoothstep(0.52, 0.85, broad * 0.65 + ridges * 0.35);
    let rgb = c.rgb * mix(1.0, variation, soil * k)
        + params.color.rgb * c.a * dust * soil * k * 0.22;
    // Fine pitting and sparse worn highlights on the dark metal pixels.
    let metal = 1.0 - soil;
    let pits = ground_noise(p * 28.0 + vec2(61.0, 13.0));
    let scratches = ground_noise(p * vec2(8.0, 36.0) + vec2(79.0));
    let wear = smoothstep(0.68, 0.92, scratches) * smoothstep(0.3, 0.8, broad);
    let worn = rgb * mix(1.0, 0.80 + pits * 0.42, metal * k)
        + vec3(0.025, 0.018, 0.022) * wear * metal * k * c.a;
    return vec4(worn, c.a);
}
