# Floatpaper

A small Rust / wgpu wallpaper engine for Wayland (Hyprland), with a deterministic, layered scene model and offline video export. This is a **first-pass prototype**, not a tested release. The source was prepared in an environment without Rust, a Wayland compositor or a usable GPU; Cargo compilation and GPU validation have not yet been performed. No Cargo.lock is included because dependencies could not be resolved here.

## Quick start on Arch

Install the Rust toolchain and Wayland development prerequisites:

```sh
sudo pacman -S --needed rust base-devel pkgconf wayland vulkan-icd-loader ffmpeg
```

Use the Vulkan driver appropriate for your GPU (e.g. `vulkan-radeon` for AMD, `vulkan-intel` for Intel, or the matching proprietary NVIDIA driver and `nvidia-utils`). Do not install a random driver or replace a working configuration just for Floatpaper. `vulkan-tools` provides `vulkaninfo --summary` if needed.

From the extracted project root, **first run only**:

```sh
cargo check
```

If this fails, preserve the first full compiler diagnostic. Do not attempt runtime commands until the project compiles. Once it does:

```sh
cargo test
cargo run --release -- validate scenes/minimal.toml
cargo run --release -- preview scenes/minimal.toml
```

Close the preview window normally or use Ctrl+C from the terminal. Then try the full example:

```sh
cargo run --release -- preview scenes/pure_black.toml
```

### Live desktop wallpaper

```sh
cargo run --release -- live scenes/pure_black.toml
# Or select one output by its Wayland name (hyprctl monitors lists them):
cargo run --release -- live scenes/pure_black.toml --output DP-1
```

The app creates its own non-interactive Wayland background-layer surfaces. Stop any existing wallpaper daemon yourself if its surface covers this one; Floatpaper never kills or modifies other processes or Hyprland settings. Live mode attaches to all currently connected outputs by default and handles registry add/remove events, subject to runtime testing. Integer scaling only; fractional scaling can be resampled by the compositor. Preview windows use server/compositor decoration behaviour.

### Export and video playback

A quick four-second smoke test, followed by the full 20-minute loop:

```sh
cargo run --release -- export scenes/minimal.toml -o test.mp4 --width 640 --height 360
cargo run --release -- export scenes/pure_black.toml -o pure_black.mp4 --width 1920 --height 1080
```

Requires an FFmpeg build containing `libx264`. Export defaults to the scene loop duration and FPS. Width/height must be even. You can override `--fps`, `--duration`, `--crf` (lower CRF = higher quality), and `--overwrite`. Without `--allow-non-loop`, duration must be an integer multiple of the configured scene loop and the frame count must be integral. Ctrl+C cancels the export and attempts to clean up its temporary file. Existing videos are protected unless `--overwrite` is set. No audio track is produced.

For a short preview of the full scene that need not loop, use `--duration 5 --allow-non-loop`. To create a short mathematically looping version, copy the scene and set `loop_seconds = 20.0`; periods will be resnapped (and long motions will accelerate).

Video playback is delegated to [mpvpaper](https://github.com/GhostNaN/mpvpaper), available via the AUR. Review its PKGBUILD before installing. If you already use an AUR helper, for example:

```sh
yay -S mpvpaper
cargo run --release -- play pure_black.mp4
cargo run --release -- play pure_black.mp4 --output DP-1
```

`play` starts mpvpaper in the foreground with `no-audio loop-file=inf hwdec=auto-safe panscan=1.0`. Hardware decoding depends on your driver, codec support, and mpvpaper's render path. The video can be mathematically seamless while playback still has a small decoder seek gap, compression differences or monitor-sync issues. Check visually and profile both modes before assuming video is cheaper. You can use `--hwdec no` to compare software decoding.

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

Shape types: `circle`, `rectangle`, `triangle`, `polygon` (3–32 `sides`). Motion types: `static`, `orbit`, `drift` (combined harmonics), `wrap`.

Effects run in order. `intensity` is effect-specific; `period` is the animation period in seconds, and `pulse` modulates intensity cyclically.

| Effect | Intensity | Notes |
|---|---|---|
| `noise` | 0..0.1 typical | Temporally periodic pixel grain in linear color |
| `crt` | 0..1 | Scanlines, curvature, vignette |
| `glitch` | 0..1 | Channel splitting and periodic horizontal band displacement |
| `blur` | pixels, e.g. 1..4 | Two-pass separable Gaussian approximation |
| `custom` | shader-defined | Load a WGSL effect body from a file |

For an individual shape effect, give that shape its own layer. Effects process the entire layer texture, including transparency. This version has no keyframe editor or scripting runtime.

### Custom shaders and hot reload

Example:

```toml
effects = [{ kind = "custom", shader = "../shaders/custom_vignette.wgsl", intensity = 0.5 }]
```

Paths are relative to the scene TOML file. A custom shader defines `fn effect(uv: vec2<f32>) -> vec4<f32>`; `shaders/common.wgsl` is prepended automatically. The provided `sample_source(uv)` samples the input texture; `params.resolution`, `params.phase`, `params.strength`, `params.loop_phase`, `params.time` are available. UV origin is top-left. Inputs/outputs are **linear premultiplied RGBA**. Keep RGB at zero for fully transparent pixels. For seamless effects, use cyclic `phase` or sine/cosine of `loop_phase` rather than using `time` directly. Custom effects are responsible for their own periodicity.

During preview or live mode, the scene directory and initially referenced external shader directories are watched. Edits are debounced; a malformed config or shader should be rejected with diagnostics and the previous scene retained. The scene clock keeps running across reloads. Built-in WGSL files are embedded at compile time; copy one into a custom shader if you want to edit it live, or rebuild after editing built-ins. Adding a reference to a new external shader directory requires a restart for that directory to be watched. The watcher may react to unrelated file changes in watched directories; keep exports outside them where practical.

## Seamless looping model

All built-in animations are pure functions of scene time and deterministic seeds. For a loop of duration T, an animation with requested rate r is snapped to the nearest nonzero integer number of cycles per T (zero remains zero). Thus rotations, orbital motion, drift, size pulses and built-in effect phases repeat. Very slow motions can be noticeably accelerated in short loops. Horizontal wrapping adds edge copies; extremely wide shapes can overlap their own copies. Custom shaders must obey the same periodicity contract. Export samples `t = frame / fps` for frames 0 through N-1; it does not duplicate the first frame at the end.

## Static rendering caches

Floatpaper automatically caches one contiguous run of fully static layers and, when useful, one static geometry/effect prefix in a partially animated layer. A prefix ends before the first time-dependent effect; subsequent effects run every frame. Only built-in grid, blur and CRT with no intensity pulse are considered static. Animated geometry, noise, glitch and custom shaders are excluded conservatively.

The prefix texture is populated once with an exact GPU-side texture copy, then sampled directly by the dynamic suffix. Both caches are rebuilt on resize or scene reload. The combined extra texture storage is limited to 64 MiB per output; if only one cache fits, the existing static layer cache takes priority.

Set `FLOATPAPER_DISABLE_PREFIX_CACHE=1` to disable just prefix caching, or `FLOATPAPER_DISABLE_CACHE=1` to disable both. These are startup options and do not change scene files.

## Procedural noise

The default noise shader uses one PCG hash per temporal endpoint (two per pixel), extracting two 16-bit samples from each hash to retain the original 0.72/0.28 weighted grain distribution. Temporal interpolation and the 256-state cycle are unchanged. The exact random pattern differs from the previous shader, including in offline exports.

Set `FLOATPAPER_LEGACY_NOISE=1` at startup to use the original four-hash shader, for visual comparisons, benchmarks or reproduction of previous renders. No scene changes or additional noise textures are required.

## Final effect fusion

When the last global effect is a single-pass built-in, Floatpaper renders it directly into the output texture and performs the presentation color conversion in the same shader. The preceding effects still use RGBA16Float intermediates. Custom effects, two-pass blur, and scenes with no global effects retain the separate presentation pass.

Set `FLOATPAPER_DISABLE_FINAL_FUSION=1` to compare with the original two-pass path. The profiler reports the combined pass as, for example, `crt_present`; compare it with the sum of the old CRT and present categories. The optimization removes one intermediate FP16 rounding step and a same-resolution texture sample, so small output rounding differences are possible in both live mode and export.

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
shaders/        WGSL geometry and built-in/custom effects
scenes/         minimal smoke scene + original Pure Black-inspired demo
```

The four intermediate render textures are reused on each output: two ping-pong textures for a layer, two for the composited scene/global effects. Geometry is instanced; colors are linear and intermediate textures use RGBA16Float with premultiplied-alpha blending. A single GPU device is shared across outputs. Frame callbacks plus an FPS cap pace the renderer; no desktop visibility or occlusion integration is promised. The initial desktop adapter is selected for the first output; mixed-GPU outputs may not work.

### Deliberate first-version boundaries

- Direct `wayland-client` / `wayland-protocols-wlr` bindings instead of SCTK, to avoid a second abstraction for the two small surface roles. `calloop` drives the event loop.
- `wgpu` 0.19 API family selected for the initial implementation. It is older than current releases; upgrade after confirming the baseline compiles and renders. Dependency ranges are not locked until `cargo check` resolves them. Commit the resulting Cargo.lock for reproducibility.
- No automatic video cache/hash invalidation, no hybrid mode, no automatic mode switching, no fractional-scale protocol, no keyframes, no lower-resolution blur. Static-layer caching combines one contiguous static layer run per output. All effects are full-resolution and intermediate textures cost roughly 32 bytes per pixel per output (plus swapchain and other buffers). Hot reload temporarily allocates another set.
- Offline export uses synchronous CPU readback and software H.264 encoding, not a zero-copy hardware encoder. Export may be slower than real time. Playback uses an external player instead of libmpv integration.
- Power, loop playback continuity, multi-monitor/hotplug, reload and GPU compatibility are not verified. First compile, then minimal preview, then live, then export; profile after correctness.
