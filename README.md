# Floatpaper

A lightweight, programmable animated wallpaper engine for Wayland (Hyprland), written in Rust and WGSL.

Build layered scenes from TOML: geometric shapes, SVGs, animated text, live system information, audio-reactive visuals and custom shaders. Run them directly on your desktop or export seamless MP4 loops.

## Install (Arch Linux)

Requires Rust 1.89 or newer and a working Vulkan driver.

```sh
sudo pacman -S --needed rust base-devel pkgconf wayland vulkan-icd-loader
git clone https://github.com/AlexAuragan/floatpaper.git
cd floatpaper
cargo install --path .
```

Install the Vulkan driver for your GPU (e.g. `vulkan-radeon` or `vulkan-intel`) if needed. Ensure `~/.cargo/bin` is on your PATH.

## Run

```sh
floatpaper preview scenes/spaceship.toml
floatpaper live scenes/spaceship.toml
floatpaper live scenes/spaceship.toml --output DP-1
```

Edit the scene while running to hot-reload it. Use `floatpaper pause`, `resume`, `toggle` or `status` to control a live wallpaper.

## Examples

| Scene | Demonstrates |
|---|---|
| [circles_of_death](scenes/circles_of_death.toml) | Crimson battlefield, rotating cog and procedural snowfall |
| [crystallized](scenes/crystallized.toml) | Layered crystal cavern and floating faceted diamond |
| [minimal](scenes/minimal.toml) | Basic shapes and animation |
| [pure_black](scenes/pure_black.toml) | Layered effects |
| [cutout](scenes/cutout.toml) | Cutouts, grids and glitch |
| [cutout_reactive](scenes/cutout_reactive.toml) | Kick-driven glitch accents |
| [pure_black_reactive](scenes/pure_black_reactive.toml) | Kick-driven glitch and bass-driven motion/rotation |
| [spaceship](scenes/spaceship.toml) | Standalone sci-fi flight HUD; suitable for video export |
| [spaceship_reactive](scenes/spaceship_reactive.toml) | Live ASCII telemetry, output volume and reactive orbital glow |
| [audio_reactive](scenes/audio_reactive.toml) | Audio-driven glow, glitch, rotation and spectrum |

For the new reactive spacecraft, install `wireplumber` for `wpctl` (PipeWire output volume) and `libpulse` for audio capture:

```sh
floatpaper trust scenes/spaceship_reactive.toml
floatpaper preview scenes/spaceship_reactive.toml
floatpaper live scenes/spaceship_reactive.toml
```

The standalone `spaceship.toml` requires no command trust or audio capture and can be exported to MP4 for playback on other platforms. Floatpaper itself remains a Wayland application.

**Only trust scenes and scripts you have reviewed.** Commands run with your user permissions. Trust is stored per canonical scene path and exact command names/arguments; changed declarations require reauthorization. Scripts and programs invoked by an approved command are not pinned or sandboxed. Use `floatpaper untrust <scene>` to revoke approval, then restart or reload the running scene. Without trust, command text uses defaults.

## Audio reactivity (optional)

Install `libpulse` for `parec` and use a running PulseAudio-compatible server (such as PipeWire with pipewire-pulse).

```sh
floatpaper preview scenes/audio_reactive.toml
floatpaper live scenes/audio_reactive.toml
```

Playback audio is captured when the scene sets `[audio] enabled = true`. It is processed locally, not recorded to disk. Audio-reactive properties and spectrum bars are described in the reference.

## Video export (optional)

Install FFmpeg with libx264 and, for playback, mpvpaper (AUR).

```sh
floatpaper export scenes/pure_black.toml -o wallpaper.mp4
floatpaper play wallpaper.mp4
```

MP4 cannot display live command output or audio reactivity; export warns and uses base parameters and text defaults.

## Documentation

- [Scene format, shaders, runtime controls and performance options](docs/REFERENCE.md)
- `floatpaper --help` and `floatpaper <command> --help`

License: [MIT](LICENSE).
