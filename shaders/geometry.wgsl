struct GeometryUniform { viewport: vec4<f32> };
@group(0) @binding(0) var<uniform> geometry: GeometryUniform;
struct VertexIn {
    @location(0) center_size: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) params: vec4<f32>,
};
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) params: vec4<f32>,
};
@vertex fn vs_main(input: VertexIn, @builtin(vertex_index) vertex: u32) -> VertexOut {
    var corners = array<vec2<f32>, 6>(vec2(-1.,-1.),vec2(1.,-1.),vec2(-1.,1.),vec2(-1.,1.),vec2(1.,-1.),vec2(1.,1.));
    let p = corners[vertex] * 1.08;
    let local = p * input.center_size.zw * 0.5;
    let c = cos(input.params.x);
    let s = sin(input.params.x);
    let rotated = vec2(c*local.x-s*local.y,s*local.x+c*local.y);
    let pos = input.center_size.xy + rotated * vec2(geometry.viewport.y / geometry.viewport.x, 1.0);
    var out: VertexOut;
    out.position = vec4(pos.x*2.0-1.0,1.0-pos.y*2.0,0.0,1.0);
    out.local = p;
    out.color = input.color;
    out.params = input.params;
    return out;
}
@fragment fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let p = input.local;
    // Derivatives are evaluated outside the shape-dependent branch.
    let ray_aa = max(fwidth(p), vec2(0.0001));
    if input.params.y > 3.5 {
        // Local +X is the ray head. Width includes the optional soft halo.
        let u = clamp(p.x * 0.5 + 0.5, 0.0, 1.0);
        let end_mask = 1.0 - smoothstep(1.0 - ray_aa.x, 1.0 + ray_aa.x, abs(p.x));
        let core_width = mix(0.88, 0.25, input.params.w);
        let core = 1.0 - smoothstep(core_width - ray_aa.y, core_width + ray_aa.y, abs(p.y));
        let soft = pow(max(0.0, 1.0 - abs(p.y)), 2.0) * input.params.w * 0.45;
        let coverage = clamp(core + soft, 0.0, 1.0) * end_mask
            * mix(1.0, u, input.params.z);
        let alpha = input.color.a * coverage;
        return vec4(input.color.rgb * alpha, alpha);
    }
    var d = length(p)-1.0;
    if input.params.y > 0.5 && input.params.y < 1.5 {
        let q = abs(p)-vec2(1.0);
        d = length(max(q,vec2(0.0))) + min(max(q.x,q.y),0.0);
    } else if input.params.y >= 1.5 {
        let sector = 6.28318530718 / input.params.z;
        let a = atan2(p.y,p.x)+1.57079632679;
        let edge = floor(a/sector)*sector + sector*0.5;
        d = cos(a-edge)*length(p)-cos(sector*0.5);
    }
    let aa = max(fwidth(d),0.0001);
    var coverage = 1.0-smoothstep(-aa,aa,d);
    if input.params.w > 0.0 {
        coverage *= smoothstep(-input.params.w-aa,-input.params.w+aa,d);
    }
    let a = input.color.a * coverage;
    return vec4(input.color.rgb*a,a);
}
