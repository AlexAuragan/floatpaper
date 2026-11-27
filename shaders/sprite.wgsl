struct GeometryUniform { viewport: vec4<f32> };
@group(0) @binding(0) var<uniform> geometry: GeometryUniform;
@group(0) @binding(1) var sprite: texture_2d<f32>;
@group(0) @binding(2) var sprite_sampler: sampler;
struct VertexIn {
    @location(0) center_size: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) params: vec4<f32>,
};
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) preserve_color: f32,
};
@vertex fn vs_main(input: VertexIn, @builtin(vertex_index) vertex: u32) -> VertexOut {
    var corners = array<vec2<f32>, 6>(vec2(-1.,-1.),vec2(1.,-1.),vec2(-1.,1.),vec2(-1.,1.),vec2(1.,-1.),vec2(1.,1.));
    let p = corners[vertex];
    let local = p * input.center_size.zw * 0.5;
    let c = cos(input.params.x);
    let s = sin(input.params.x);
    let rotated = vec2(c*local.x-s*local.y,s*local.x+c*local.y);
    let pos = input.center_size.xy + rotated * vec2(geometry.viewport.y / geometry.viewport.x, 1.0);
    var out: VertexOut;
    out.position = vec4(pos.x*2.0-1.0,1.0-pos.y*2.0,0.0,1.0);
    out.uv = p * 0.5 + 0.5;
    out.color = input.color;
    out.preserve_color = input.params.w;
    return out;
}
@fragment fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let tex = textureSample(sprite, sprite_sampler, input.uv);
    let a = tex.a * input.color.a;
    if input.preserve_color > 0.5 {
        return vec4(tex.rgb * input.color.a, a);
    }
    return vec4(input.color.rgb * a, a);
}
