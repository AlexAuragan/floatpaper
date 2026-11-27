// Digital macroblock corruption: sparse rectangular shifts and quantization.
// No temporal feedback buffer; this is a datamosh-like spatial effect.
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let k = clamp(params.strength, 0.0, 1.0);
    let tick = floor(params.phase * 64.0);
    let cell_size = vec2(3.0, 1.0) * max(params.spacing, 4.0);
    let block = floor(uv * params.resolution / cell_size);
    let chance = hash(block + vec2(tick * 7.0, tick * 13.0));
    let hit = step(1.0 - 0.15 * k, chance) * step(0.0001, k);
    let shift = (hash(block + vec2(tick, 71.0)) - 0.5) * 0.17 * k * hit;
    let p = clamp(uv + vec2(shift, 0.0), vec2(0.0), vec2(1.0));
    let c = sample_source(p);
    let rgb = c.rgb / max(c.a, 0.00001);
    let crushed = floor(rgb * 7.0 + 0.5) / 7.0;
    let shuffled = mix(crushed, crushed.brg, 0.35 * k);
    return vec4(mix(c.rgb, shuffled * c.a, hit * k), c.a);
}
