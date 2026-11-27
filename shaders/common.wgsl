// Shared interface for all effects. Input and output use linear, premultiplied RGBA.
struct EffectUniform {
    resolution: vec2<f32>,
    phase: f32,
    strength: f32,

    loop_phase: f32,
    time: f32,
    rotation: f32,
    spacing: f32,

    offset: vec2<f32>,
    _pad: vec2<f32>,

    color: vec4<f32>,
    onset: f32,
    kick: f32,
    bass: f32,
    mid: f32,
    treble: f32,
    volume: f32,
    onset_strength: f32,
    kick_strength: f32,
};
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> params: EffectUniform;
struct ScreenVertex { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs_main(@builtin(vertex_index) i: u32) -> ScreenVertex {
    var p = array<vec2<f32>,3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0));
    var out: ScreenVertex;
    out.position = vec4(p[i],0.0,1.0);
    out.uv = vec2(p[i].x*0.5+0.5,0.5-p[i].y*0.5);
    return out;
}
fn sample_source(uv: vec2<f32>) -> vec4<f32> {
    return textureSample(source,source_sampler,uv);
}
fn hash(p: vec2<f32>) -> f32 {
    let q = fract(p*vec2(123.34,456.21));
    let r = q + vec2(dot(q,q+vec2(45.32)));
    return fract(r.x*r.y);
}
@fragment fn fs_main(input: ScreenVertex) -> @location(0) vec4<f32> {
    return effect(input.uv);
}

fn rotate_point(p: vec2<f32>, angle: f32) -> vec2<f32> {
    let c = cos(angle);
    let s = sin(angle);

    return vec2<f32>(
        c * p.x - s * p.y,
        s * p.x + c * p.y
    );
}
