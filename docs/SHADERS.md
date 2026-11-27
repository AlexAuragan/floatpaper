# Shader library

These original WGSL effects use Floatpaper's existing custom-effect interface. Each effect is independent; variants are different visual algorithms, not replacements for the built-ins. The suffix is a stable variant identifier.

| Effect | Visual character | Suggested intensity | Spacing | Source samples |
|---|---|---:|---|---:|
| glitch_1 | Analog horizontal sync interference and RGB drift | 0.7 | scanline height, 2 px | 3 |
| glitch_2 | Sparse rectangular displacement and color corruption | 0.8 | macroblock height, 24 px | 1 |
| snow_1 | Wind-driven, independently advancing columns of soft, elongated flakes | 0.65 | density scale, 48 (larger = fewer flakes) | 1 |
| fog_1 | Domain-warped, drifting fog | 0.35 | cloud scale, 260 px | 1 |
| film_grain_1 | Fine integer-hashed, luminance-dependent animated grain | 0.65 | grain size, 1 px | 1 |
| chromatic_aberration_1 | Radial lens dispersion | 0.8 | unused | 3 |

All support the intensity uniform and a seamless phase cycle (except the time-independent lens effect). Period controls cycle duration, not a universal physical speed. Snow rotation controls the wind shear, fog rotation controls the cloud orientation, and fog/snow use color as an overlay tint. Lens offset moves the optical center in pixels. The new effects are single-pass and do not require an engine change. They operate on linear premultiplied RGBA; snow and fog can add coverage to transparent input. Apply them globally for ordinary atmospheric overlays. The lens effect can declare `static = true` when used on static layers; all other new effects are time dependent. Glitch_2 is spatial corruption only, not actual datamoshing with previous-frame feedback.

## Examples

Each `scenes/shader_lab_<name>.toml` uses the same geometry and one effect. This intentionally duplicates a tiny test fixture for isolated previews; the production scenes remain independent. The canonical `scenes/circles_of_death.toml` uses the preferred `snow_1` variant. The previous `battle_snow.wgsl` remains available for comparison by changing the effect configuration.

```toml
[[effects]]
kind = "custom"
shader = "../shaders/snow_1.wgsl"
intensity = 0.65
spacing = 48
rotation = -18
color = "#999093"
period = 24
```

## Technique references

These references informed the design; the WGSL implementations are original, not source ports.

- [KinoGlitch](https://github.com/keijiro/KinoGlitch): analog signal interference versus digital block corruption.
- [The Book of Shaders: Fractal Brownian Motion](https://thebookofshaders.com/13/): multiscale noise and domain warping.
- [SnowSystemThreeJS](https://github.com/achrefelouafi/SnowSystemThreeJS): wind, individual flutter and soft flakes.
- [GPU Gems 3: Practical Post-Process Depth of Field](https://developer.nvidia.com/gpugems/gpugems3/part-iv-image-effects/chapter-28-practical-post-process-depth-field): focus as a depth cue.
- [GPU Gems: Fire in the Vulcan Demo](https://developer.nvidia.com/gpugems/gpugems/part-i-natural-effects/chapter-6-fire-vulcan-demo): film grain as a finishing treatment.

Existing material effects and cache behavior are described in [REFERENCE.md](REFERENCE.md).

Performance depends on resolution and GPU. Source sample counts exclude arithmetic and the rest of the scene. Shader compilation and visual tuning should be verified on the target GPU.
