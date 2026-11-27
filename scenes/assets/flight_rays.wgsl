// Sparse, looped flight streaks aimed at the spacecraft's center.
// Coordinates are in screen-height units so the streaks stay straight
// on both portrait and landscape displays.
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let aspect = params.resolution.x / params.resolution.y;
    let p = (uv - vec2(0.69, 0.63)) * vec2(aspect, 1.0);
    let radius = length(p);
    let angle = atan2(p.y, p.x);
    let angular = (angle + 3.14159265) * (112.0 / 6.28318530);
    let lane = floor(angular);
    let offset = abs(fract(angular) - 0.5);
    let random = hash(vec2(lane, 17.0));
    let random2 = hash(vec2(lane, 83.0));

    // Prefer the upper-left side of the ship, with a few peripheral rays.
    let direction = p / max(radius, 0.0001);
    let approach = smoothstep(-0.15, 0.80, dot(direction, normalize(vec2(-0.85, -0.60))));
    let lane_enabled = step(0.48, random);
    let distance_px = offset * (6.28318530 / 112.0) * radius * params.resolution.y;
    let core = 1.0 - smoothstep(0.25, 1.65, distance_px);
    let halo = 1.0 - smoothstep(0.5, 5.5, distance_px);

    // Integer phase travel keeps each lane seamless at the scene loop.
    let travel = fract(radius * (2.2 + random2 * 2.0) + params.phase + random);
    let segment = smoothstep(0.04, 0.13, travel)
        * (1.0 - smoothstep(0.48 + random2 * 0.20, 0.84, travel));
    let envelope = smoothstep(0.13, 0.28, radius);
    let a = clamp((core * 0.55 + halo * 0.12) * segment * envelope
        * lane_enabled * approach * params.strength * (0.35 + random2 * 0.65), 0.0, 1.0);
    let rgb = mix(vec3(0.11, 0.37, 0.55), vec3(0.60, 0.84, 0.95), core);
    return vec4(rgb * a, a);
}
