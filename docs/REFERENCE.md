# Floatpaper reference

## Scene format

TOML top-level settings:

```toml
fps = 30
loop_seconds = 1200.0  # omit for non-looping scenes
seed = 42
background = "#050507"  # opaque sRGB hex
# Optional ordered global effects:
effects = [{ kind = "crt", intensity = 0.2 }]

[[layers]]
name = "background shapes"
opacity = 0.6
background = "#00000000"  # optional RGBA layer fill
# Optional ordered per-layer effects:
effects = [{ kind = "blur", intensity = 2.0 }]

[[layers.shapes]]
kind = "triangle"
count = 10
center = [0.5, 0.5]
spread = [0.4, 0.4]
size = [0.1, 0.1]
size_jitter = 0.3
color = "#ddddff"
opacity = 0.8
outline = 0.025
motion = "drift"
radius = [0.1, 0.07]
period = 80.0
spin = 0.025
pulse = 0.15
```

Layers are painted in file order, back to front. Each shape declaration generates `count` instances with deterministic random offsets, size variation and phases; use separate declarations with `count=1` to control shapes individually. Coordinates use the top-left origin and normalized screen coordinates (x and y each 0..1). Sizes are full bounding-box dimensions relative to viewport **height**, so equal x/y sizes retain their aspect ratio on wide monitors. Motion radii are in normalized screen coordinates. Shapes can drift offscreen. `wrap` moves horizontally, drawing translated edge copies. Colors accept `#RRGGBB` or `#RRGGBBAA`; layer and shape opacity multiply existing alpha. `outline=0` fills the shape; positive values produce a stroke. Shape rotation is in degrees, `spin` in signed turns per second, `phase` in cycles. `pulse` (0..1) modulates size.

Shape types: `circle`, `rectangle`, `triangle`, `polygon` (3–32 `sides`), `ray`, `svg`, `text`, `spectrum`. Motion types: `static`, `orbit`, `drift` (combined harmonics), `wrap`, `radial`, `linear`.

### Rays, directional motion and anchors

```toml
[anchors.ship]
center = [0.69, 0.63]

[[layers]]
name = "flight streaks"
[[layers.shapes]]
kind = "ray"
anchor = "ship"
count = 85
motion = "radial"
direction = "inward"
heading = 215.0
angle_spread = 155.0
radius = [0.22, 2.15]
period = 3.0
align_to_motion = true
size = [0.20, 0.008]
size_jitter = 0.65
fade = 1.0
glow = 0.85
recycle_fade = 0.18
color = "#a8d9ef"
opacity = 0.46
```

- Rays are analytic quads: local +X is the head, `fade` (0..1) attenuates the tail, and `glow` (0..1) creates a soft transverse halo inside the specified width. No extra render pass or texture. Outline is unsupported for rays.
- Radial motion uses `radius = [inner, outer]` in viewport-height units, `direction = "inward"` (default) or `"outward"`, and `heading` in clockwise degrees from screen right (90 is down). Multiple instances sample an emission cone of full angular width `angle_spread` (default 360); a single instance uses heading exactly. Existing spread offsets move individual centers independently; leave spread zero for a common convergence point.
- Linear motion uses `travel = [dx, dy]` in viewport-height units, from center minus half travel to center plus half travel. Both motions repeat each period with seeded phase offsets for multiple instances. `recycle_fade` (0..0.5, default 0.15) fades opacity at the start/end of the cycle; zero disables fading.
- `align_to_motion = true` points local +X along radial/linear velocity. Explicit rotation, spin and rotation_jitter remain additive. When aligned, omitted rotation_jitter defaults to zero instead of random rotation. To point an upright SVG along motion, add 90 degrees of rotation.
- Anchors are static named normalized-screen positions shared across layers. `anchor = "ship"` replaces the shape's center; optional `anchor_offset = [dx, dy]` adds a normalized-screen offset. Changing the anchor moves all dependent shapes. Anchors are not hierarchical or animated in this version. Individual shapes can animate relative to their anchor.
- Existing orbit/drift/wrap coordinates and behavior are unchanged; only radial/linear distances use viewport-height units for aspect-correct direction.

See `scenes/spaceship_loading.toml` for a combined SVG spacecraft, multiline Unicode HUD, and native radial rays. 

### Responsive HUD positioning

Any shape can use `origin` (top_left, top_center, top_right, center_left, center, center_right, bottom_left, bottom_center, bottom_right), `position` and `position_unit = "px"` or `"relative"`. Positive offsets from right/bottom edges point inward. `attachment` uses the same nine values and defaults to origin; it is also the rotation pivot. Pixel sizes use `size_unit = "px"`; relative sizes still use viewport height. Pixels are logical pixels in Wayland preview/live (integer scale) and output pixels in export. Do not combine origin/position with a named anchor or nondefault center.

### SVG, text and dynamic HUD

SVG shapes use `path` relative to the scene, `raster_size` (16..4096, default 1024), and optionally `preserve_color = true`. Text shapes use `text` (multiline Unicode supported), `font_size` (raster font size), `line_height` (multiplier), and optional `font_family` (defaults to monospace). Shape size controls the displayed dimensions; outlines are not supported on sprites.

Named text sources can be shared by glow and foreground layers:

```toml
[text_sources.uplink]
kind = "sequence"
frames = ["WAITING", "WAITING.", "WAITING..", "WAITING..."]
frame_seconds = 0.5
default = "STANDBY"

[text_sources.telemetry]
kind = "command"
command = ["bash", "scripts/telemetry.sh"]
interval = 5.0
timeout = 2.0
default = "CPU   --%\nGPU   --%\nTEMP  --°C"
layout = "CPU  100%\nGPU  100%\nTEMP 100°C"

[[layers]]
name = "HUD"
[[layers.shapes]]
kind = "text"
text_source = "telemetry"
origin = "top_right"
position = [50, 50]
position_unit = "px"
size = [170, 72]
size_unit = "px"
color = "#b7dce8"
```

Sequences preload their textures, support blank frames, and snap their cycle to the scene loop when configured. Commands execute once per referenced source, not per shape or monitor. Arguments are passed directly to the program; the working directory is the scene directory. Stdout must be UTF-8 and at most 16384 bytes. Timeouts, unsuccessful exits and invalid/oversized output use the default. The optional layout string reserves a fixed text canvas; output exceeding that canvas also falls back. Text rasterization occurs only when the output changes.

**Security:** command text is disabled until you explicitly run `floatpaper trust <scene>`. Authorization is stored under `$XDG_CONFIG_HOME/floatpaper/trusted-scenes.json` (default `~/.config/floatpaper/trusted-scenes.json`) in a private directory/file. Trust matches the canonical scene path and exact named command argument lists, including on hot reload; visual edits do not require reauthorization. New or changed command declarations disable command execution until trusted again and reloaded. Commands run with your user privileges, without a sandbox. Trust also requires trusting the scripts, executables and environment they use; script contents are not pinned. `floatpaper untrust <scene>` revokes authorization on next launch or reload, not immediately in an already running process. Manual pause displays defaults and stops polling; all-hidden suspension also stops polling. MP4 export never executes commands.

```sh
bash scenes/scripts/telemetry.sh
cargo run --release -- trust scenes/spaceship_telemetry.toml
cargo run --release -- preview scenes/spaceship_telemetry.toml
```

The demo script reads Linux procfs and sysfs. GPU utilization is the maximum available AMD GPU busy percentage, and temperature is the highest exposed Intel/AMD CPU package/control temperature. Missing sensors display `--`. The demo is separate from `spaceship_loading.toml`.

Effects run in order. `intensity` is effect-specific; `period` is the animation period in seconds, and `pulse` modulates intensity cyclically.

| Effect | Intensity | Notes |
|---|---|---|
| `noise` | 0..0.1 typical | Temporally periodic pixel grain in linear color |
| `crt` | 0..1 | Scanlines, curvature, vignette |
| `glitch` | 0..1 | Channel splitting and periodic horizontal band displacement |
| `blur` | pixels, e.g. 1..4 | Two-pass separable Gaussian approximation |
| `grid` | 0..1 | Rotatable pixel-space grid |
| `custom` | shader-defined | Load a WGSL effect body from a file |

Effect arguments: all effects accept `intensity`, `period`, and `pulse`; period has no visible effect on static effects unless pulse is nonzero. `rotation` is used by blur, CRT, glitch and grid (blur is two-pass when omitted, one-pass when supplied). `spacing`, `offset`, and `color` are grid-specific; custom effects can use any shared uniform. Noise ignores rotation. Unsupported non-default arguments produce warnings.

For an individual shape effect, give that shape its own layer. Effects process the entire layer texture, including transparency. This version has no keyframe editor or scripting runtime.

### Custom shaders and hot reload

Example:

```toml
effects = [{ kind = "custom", shader = "../shaders/custom_vignette.wgsl", intensity = 0.5 }]
```

Paths are relative to the scene TOML file. A custom shader defines `fn effect(uv: vec2<f32>) -> vec4<f32>`; `shaders/common.wgsl` is prepended automatically. The provided `sample_source(uv)` samples the input texture; `params.resolution`, `params.phase`, `params.strength`, `params.loop_phase`, `params.time`, `params.rotation` (degrees), `params.spacing` (pixels), `params.offset` (pixels), and `params.color` (linear straight RGBA) are available. UV origin is top-left. Inputs/outputs are **linear premultiplied RGBA**. Keep RGB at zero for fully transparent pixels. For seamless effects, use cyclic `phase` or sine/cosine of `loop_phase` rather than using `time` directly. Custom effects are responsible for their own periodicity.

During preview or live mode, the scene directory and initially referenced external shader directories are watched. Edits are debounced; a malformed config or shader should be rejected with diagnostics and the previous scene retained. The scene clock keeps running across reloads. Built-in WGSL files are embedded at compile time; copy one into a custom shader if you want to edit it live, or rebuild after editing built-ins. Adding a reference to a new external shader directory requires a restart for that directory to be watched. The watcher may react to unrelated file changes in watched directories; keep exports outside them where practical.

## Audio reactivity

Enable playback audio per scene (no CLI flag needed):

```toml
[audio]
enabled = true
sensitivity = 1.0
```

```sh
floatpaper preview scenes/audio_reactive.toml
floatpaper audio-test --sensitivity 1.0
```

Requires `parec` (Arch package `libpulse`) and a running PulseAudio-compatible server. Preview/live capture the default playback monitor, not the default microphone. The diagnostic accepts an optional `--target` source. Sensitivity is 0.25..4 (default 1). Audio is local PCM, not saved to disk. One capture worker serves all outputs. When disabled or on capture failure, reactive values are zero; capture failures are retried. Manual pause, all-hidden suspension and frozen benchmark modes stop capture. MP4 export uses zero audio signals and does not capture sound.

### Shape and effect modulation

```toml
[[layers]]
name = "audio ring"
[[layers.shapes]]
kind = "circle"
size = [0.48, 0.48]
opacity = 0.7
color = "#526d81"
[layers.shapes.audio]
signal = "bass"
size = 0.4
opacity = 0.3
color = "#e8faff"
attack = 0.005
decay = 0.08
```

The response is a normalized value v in [0,1]. Every specified property shares this response; omitted properties remain unchanged.

| Shape audio field | Meaning / range |
|---|---|
| size | Multiply base dimensions by 1 + size * v; -0.99..10 |
| opacity | Add opacity * v, clamp to 0..1; -1..1 |
| color | Interpolate linear RGBA toward target hex color; unsupported with preserve_color |
| position | Seeded displacement in normalized screen coordinates, each component -1..1 |
| spin | Add spin * v turns/sec, integrated continuously; -100..100 |

For effects use `[layers.effects.audio]` or `[effects.audio]` for global effects, with `intensity` (-100..100) added to the base strength and clamped to 0..100; the shader may clamp further.

| Response field | Meaning / default |
|---|---|
| signal | bass (<200 Hz), mid (200–2500 Hz), treble (>=2500 Hz), volume, onset, kick, onset_strength, kick_strength; default onset |
| attack | Rise time constant, seconds; 0..5, default 0.01 |
| decay | Fall time constant, seconds; 0..10, default 0.15 |
| logarithmic | Default true; ln(1+9v)/ln(10) |
| power | 0.1..8, default 1; requires logarithmic=false if nondefault |

Zero attack/decay is immediate. Broad energy signals already have a short internal smoothing stage. Onset and kick are adaptive spectral-change pulses (kick emphasizes frequencies up to 150 Hz), not a beat/tempo tracker. Strength variants multiply pulses by impact amplitude. 

Glitch defaults to `continuous = true`: displacement amplitude follows intensity without the built-in burst gate. Period controls smooth pattern variation. Set `continuous = false` for the original periodic bursts. This changes the appearance of older glitch scenes that omit the flag.

### Spectrum

```toml
[[layers]]
name = "spectrum"
[[layers.shapes]]
kind = "spectrum"
bars = 64
origin = "bottom_center"
position = [0.0, 0.02]
size = [1.2, 0.2]
color = "#71c4e8"
opacity = 0.2
gap = 0.25
attack = 0.005
decay = 0.08
```

Spectrum uses the same FFT as audio modulation, with 64 logarithmically spaced bands from 30 Hz to 16 kHz. Fewer bars take the maximum of adjacent band responses. Bars grow upwards within the maximum-size bounding box and vanish in silence. Sizes and attachment follow ordinary shape layout (relative sizes are viewport-height units). Bars default to 64 (1..64), gap to 0.25 (0..0.95), attack to 0.005s, decay to 0.08s, and logarithmic to true. These fields are directly on the spectrum shape, not an audio subtable. The spectrum does not support count other than 1, motion, rotation, spin, pulse, spread, size_jitter or an additional shape audio block. Color, opacity, outline and compositing use the normal rectangle renderer. Adjacent bass bars can share FFT bins due to the roughly 23 Hz frequency resolution. No external CAVA process is needed.

### Audio shader uniforms

Custom WGSL effects can read `params.bass`, `params.mid`, `params.treble`, `params.volume`, `params.onset`, `params.kick`, `params.onset_strength`, and `params.kick_strength`. These are analyzer signals before per-binding response shaping; strength variants are weighted pulses. Spectrum bands are not exposed to WGSL.

## Seamless looping model

Built-in shape animations and text sequences are pure functions of scene time and deterministic seeds; command sources are live inputs and use defaults for export. For a loop of duration T, an animation with requested rate r is snapped to the nearest nonzero integer number of cycles per T (zero remains zero). Thus rotations, orbital motion, drift, size pulses and built-in effect phases repeat. Very slow motions can be noticeably accelerated in short loops. Horizontal wrapping adds edge copies; extremely wide shapes can overlap their own copies. Custom shaders must obey the same periodicity contract. Export samples `t = frame / fps` for frames 0 through N-1; it does not duplicate the first frame at the end.

## Static rendering caches

Floatpaper automatically caches one contiguous run of fully static layers and, when useful, one static geometry/effect prefix in a partially animated layer. A prefix ends before the first time-dependent effect; subsequent effects run every frame. Only built-in grid, blur and CRT with no intensity pulse are considered static. Animated geometry, audio-reactive shapes/effects, spectrum shapes, text sources, noise, glitch and custom shaders are excluded conservatively.

The prefix texture is populated once with an exact GPU-side texture copy, then sampled directly by the dynamic suffix. Both caches are rebuilt on resize or scene reload. The combined extra texture storage is limited to 64 MiB per output; if only one cache fits, the existing static layer cache takes priority.

Set `FLOATPAPER_DISABLE_PREFIX_CACHE=1` to disable just prefix caching, or `FLOATPAPER_DISABLE_CACHE=1` to disable both. These are startup options and do not change scene files.

## Procedural noise

The default noise shader uses one PCG hash per temporal endpoint (two per pixel), extracting two 16-bit samples from each hash to retain the original 0.72/0.28 weighted grain distribution. Temporal interpolation and the 256-state cycle are unchanged. The exact random pattern differs from the previous shader, including in offline exports.

Set `FLOATPAPER_LEGACY_NOISE=1` at startup to use the original four-hash shader, for visual comparisons, benchmarks or reproduction of previous renders. No scene changes or additional noise textures are required.

## Final effect fusion

When the last global effect is a single-pass built-in, Floatpaper renders it directly into the output texture and performs the presentation color conversion in the same shader. The preceding effects still use RGBA16Float intermediates. Custom effects, two-pass blur, and scenes with no global effects retain the separate presentation pass.

Set `FLOATPAPER_DISABLE_FINAL_FUSION=1` to compare with the original two-pass path. The profiler reports the combined pass as, for example, `crt_present`; compare it with the sum of the old CRT and present categories. The optimization removes one intermediate FP16 rounding step and a same-resolution texture sample, so small output rounding differences are possible in both live mode and export.

## Layer compositing fusion

The last single-pass built-in effect of a layer can blend directly into the scene (or static layer cache), eliminating its temporary result and separate compositing pass. The shader multiplies all four premultiplied RGBA channels by the layer's constant opacity, independently of effect strength. Static prefix caches remain unchanged and may feed the fused effect directly. Custom effects, two-pass blur and layers without effects retain the separate compositing pass.

Set `FLOATPAPER_DISABLE_LAYER_FUSION=1` for an isolated comparison; final global effect fusion and both static caches remain enabled. Profiler categories such as `glitch_composite` replace the corresponding effect and composite passes. Removing the intermediate FP16 conversion and same-resolution sample can introduce small pixel differences.

## Sparse glitch rendering

Transparent geometry layers with exactly one fused glitch effect use a conservative per-frame scissor rectangle. The rectangle bounds all over-geometry quads (including wrapped copies and rotation), expanded for the maximum glitch displacement, chromatic split and filtering slack. The destination is loaded, not cleared; pixels outside the rectangle remain unchanged. Opaque backgrounds, multi-effect chains, prefix caches, extreme coordinates and unusable rectangles retain the full-screen path. The geometry texture still uses the full output resolution and is cleared as before.

Set `FLOATPAPER_DISABLE_SPARSE_GLITCH=1` to compare against the original full-screen path. The profiler reports restricted passes separately as `glitch_composite_sparse`. Compare their sum with the remaining `glitch_composite` category against the previous combined cost. The bounds depend on the geometry and glitch shader implementations; update them if those shaders change.

## Experimental compact intermediate textures

Set `FLOATPAPER_COMPACT_INTERMEDIATES=1` at startup to use RGBA8UnormSrgb rather than the default RGBA16Float for all intermediate render targets and static caches. GPU sRGB encoding/decoding keeps shader operations and blending in linear space. Texture storage is halved, and the existing 64 MiB per-output cache budget accounts for the selected format. The swapchain/export format is unchanged. The selected intermediate format is logged and retained across resize.

This is opt-in because 8-bit RGB and alpha quantization and clamping to [0,1] can change results, especially dark gradients, translucent edges, repeated blending and custom effects that output extended-range values. Do not use it if exact visual fidelity or prior export reproduction is required. Compare both modes with profiling enabled, then measure GPU utilization with profiling disabled.

## GPU selection (live)

Live mode defaults to `FLOATPAPER_GPU=auto`: read the compositor's default linux-dmabuf feedback (protocol v4+), resolve its main DRM device through Linux device nodes/sysfs, then select a surface-compatible Vulkan adapter with the matching PCI vendor/device IDs. This avoids hardcoding card numbers or assuming that a low-power GPU is cheaper when the compositor is on another GPU.

Overrides, for live mode only:

- `FLOATPAPER_GPU=low-power`: original wgpu low-power preference.
- `FLOATPAPER_GPU=high-performance`: wgpu high-performance preference (not an exact GPU selector).
- `FLOATPAPER_GPU=vvvv:dddd`: explicit hexadecimal PCI vendor/device ID, as displayed by `lspci -nn`.

Auto mode logs its match or a reason for falling back to the original low-power policy. Explicit PCI selection errors when no compatible adapter or multiple matching Vulkan adapters exist. wgpu 0.19 exposes PCI model IDs rather than a unique physical DRM identity, so identical GPU models cannot reliably be distinguished with this method. GPU selection happens once at startup; restart if the compositor's preferred device changes. Preview and headless export retain their original policy. Matching the compositor reduces the likelihood of cross-GPU copies, but does not guarantee zero-copy presentation or optimal performance on every driver or secondary output.

## Shared output rendering

Live outputs with matching physical buffer dimensions, integer scale and surface texture format share one renderer. Groups with two or more outputs render to a same-format GPU texture, then copy to each swapchain if COPY_DST is supported, otherwise use an unfiltered texel-load blit. A ready surface can reuse a group frame less than one requested frame interval old, without waiting for other surfaces' frame callbacks. Independent refresh schedules can therefore introduce a small difference in animation timing compared with the original independent renderers. Single-output groups render straight to the swapchain. Preview and offline export do not share frames.

Render groups are rebuilt/reconciled on output size changes, hotplug, or successful scene reload; unused groups are released. Set `FLOATPAPER_DISABLE_OUTPUT_SHARING=1` to restore independent live rendering. The profiler labels shared-mode renderers by group dimensions and format, and reports unique rendered frames (not the number of surface presentations). Shared-frame transfers are outside its measured effect-pass durations, so compare actual GPU utilization with profiling disabled too.

## Manual wallpaper pause / resume (live)

While a live instance is running, invoke any of these commands from the same Wayland session:

```sh
floatpaper pause
floatpaper resume
floatpaper toggle
floatpaper status
```

Each command prints `paused` or `running` after the render loop acknowledges it. Pause and resume are idempotent. Manual pause presents fallback text once, then retains the buffer and stops routine frame submissions, freezing the logical animation clock so resume continues without a time jump. New or reconfigured outputs can receive one frame while paused. Automatic fullscreen/DPMS suspension remains independent: manually resuming does not override a hidden output. Successful scene edits can trigger one replacement frame while paused. Preview and export are unaffected.

The control socket lives in `XDG_RUNTIME_DIR` and is scoped by the basename of `WAYLAND_DISPLAY` (default `wayland-0`). One controlled live process per display is supported; launch additional independent processes with `FLOATPAPER_DISABLE_CONTROL=1` if needed. Those processes do not respond to control commands. An advisory file lock protects the socket against conflicting instances and permits cleanup after a crash. Runtime controls require Rust 1.89 or newer for the standard library file-lock API; no new dependency is needed.

### Waybar button

Install the built binary somewhere on Waybar's PATH (or use an absolute path in the configuration). Add `custom/floatpaper` to your `modules-left`, `modules-center` or `modules-right` list, and add the following module to your Waybar JSONC config:

```jsonc
"custom/floatpaper": {
    "exec": "floatpaper status",
    "format": "Wallpaper: {}",
    "interval": 2,
    "on-click": "floatpaper toggle",
    "exec-on-event": true,
    "tooltip": false
}
```

Click to toggle between running and paused. The status is the manual pause state, not the per-monitor visibility suspension state. A missing live instance causes the CLI to return a nonzero exit status rather than falsely reporting that it is running.

## Experimental visibility suspension (Hyprland live)

Set `FLOATPAPER_SUSPEND_HIDDEN=1` to suspend wallpaper rendering and presentation independently on outputs that Hyprland reports as powered off or covered by a mapped, non-hidden true fullscreen client on the active workspace. Maximized clients, inactive workspaces and outputs with a special workspace open are not treated as hidden. Floatpaper retains the last buffer and continues its logical animation clock; it resumes at the current animation time, respecting Wayland frame callbacks. Other outputs remain independent. Preview and export are unchanged.

This is opt-in because the public IPC does not prove actual client opacity: translucent fullscreen windows or compositor transitions may expose a frozen wallpaper. It does not attempt general window occlusion detection. A worker subscribes to Hyprland's event socket and refreshes monitor/client JSON on changes (coalesced to at most one refresh per 150 ms), with a two-second reconciliation interval for missing events and DPMS changes. No IPC requests block the render loop. Missing IPC, disconnected sockets or unrecognized JSON restore normal rendering; reconnects are attempted automatically. Suspension and resume transitions are logged. Unset the variable for the original behavior or for baseline benchmarks.

## Presentation-cost benchmark (live)

Set `FLOATPAPER_BENCHMARK_MODE` to one of three explicit values:

- `render`: animate normally, but render every group to an offscreen output texture before transfer to its surfaces, including single-output groups.
- `replay`: render the scene at time zero once per group, then repeatedly present the identical frame at the configured FPS using the same transfer path as render mode.
- `hold`: render at time zero and present once per surface, then stop presenting until reconfiguration or successful scene reload.

All three modes use the same group and offscreen texture arrangement. They are intended to isolate the incremental cost of scene rendering and ongoing presentation. Every five seconds, the benchmark logs actual scene render and per-surface submission rates, not displayed FPS. Use an external GPU utilization and power monitor with the timestamp profiler disabled, identical monitor configuration, desktop contents and intermediate texture mode. Wait for startup to settle and compare steady-state readings. Do not move windows or the pointer during measurement. Stop each run before starting the next.

For an isolated transfer-path comparison, set `FLOATPAPER_FORCE_PRESENT_BLIT=1` in replay mode. This omits COPY_DST from surface configuration and uses the existing unfiltered texel-load render pass instead of a texture copy. Confirm `shared frame transfer: blit` in startup logs. Both paths present the same frozen frame; compare the same surface submission rates and GPU utilization. This also works with ordinary shared live rendering.

The render-minus-replay and replay-minus-hold differences are experimental indicators, not additive or exact GPU cost attribution: frequency, memory traffic, compositor damage tracking and cross-GPU transfers can vary. Render mode adds an offscreen transfer to singleton outputs compared with regular live mode. Replay/hold intentionally freeze the wallpaper; these are diagnostic modes, not visual-quality-preserving animation optimizations. Without the environment variable, live rendering follows its regular path.

## GPU profiling (live or preview)

Run with the optional timestamp profiler enabled:

```sh
FLOATPAPER_PROFILE=1 cargo run --release -- live scenes/cutout.toml
```

The adapter must support wgpu's TIMESTAMP_QUERY feature; otherwise startup reports an explicit error. Each output reports submitted frames per second, sample count, mean and p95 sum of measured GPU pass durations, and mean milliseconds per effect category every five seconds. One in fifteen submitted frames is sampled, with asynchronous readback; samples are skipped if the previous result is still pending. Frames with more than 512 passes are not profiled. This is not compositor presentation FPS or end-to-end latency. GPU timestamp instrumentation can itself affect scheduling, so compare utilization with profiling disabled for final benchmarks.

For an A/B test, run the same command with `FLOATPAPER_DISABLE_CACHE=1` as well (stop the first process first). Keep monitor layout, scene, and FPS fixed. Profiling is off by default and does not change scene configuration.

## Architecture

```text
src/scene.rs    TOML, validation, deterministic shape instances
src/renderer.rs wgpu geometry, reusable textures, ordered effect passes
src/desktop.rs  raw Wayland protocols, output surfaces, frame pacing, reload
src/export.rs   headless render, RGBA readback, FFmpeg encoding
src/main.rs     CLI and mpvpaper playback
src/audio.rs    playback capture, FFT, spectrum and modulation envelopes
shaders/        WGSL geometry and built-in/custom effects
scenes/         minimal smoke scene + original Pure Black-inspired demo
```

The four intermediate render textures are reused on each output: two ping-pong textures for a layer, two for the composited scene/global effects. Geometry is instanced; colors are linear and intermediate textures use RGBA16Float with premultiplied-alpha blending. A single GPU device is shared across outputs. Frame callbacks plus an FPS cap pace the renderer; optional Hyprland IPC suspension is available for fullscreen and DPMS, not arbitrary occlusion. The initial desktop adapter is selected for the first output; mixed-GPU outputs may not work.

### Deliberate first-version boundaries

- Direct `wayland-client` / `wayland-protocols-wlr` bindings instead of SCTK, to avoid a second abstraction for the two small surface roles. `calloop` drives the event loop.
- `wgpu` 0.19 API family selected for the initial implementation. It is older than current releases; upgrade after confirming the baseline compiles and renders. Commit the updated Cargo.lock after dependency changes for reproducibility.
- No automatic video cache/hash invalidation, no hybrid mode, no automatic mode switching, no fractional-scale protocol, no keyframes, no lower-resolution blur. Static-layer caching combines one contiguous static layer run per output. All effects are full-resolution and intermediate textures cost roughly 32 bytes per pixel per output (plus swapchain and other buffers). Hot reload temporarily allocates another set.
- Offline export uses synchronous CPU readback and software H.264 encoding, not a zero-copy hardware encoder. Export may be slower than real time. Playback uses an external player instead of libmpv integration.
- Power, loop playback continuity, multi-monitor/hotplug, reload and GPU compatibility are not verified. First compile, then minimal preview, then live, then export; profile after correctness.
