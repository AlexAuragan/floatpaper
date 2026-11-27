# Floatpaper

A lightweight, programmable animated wallpaper engine for Wayland (Hyprland), written in Rust and WGSL.

Build layered scenes from TOML: geometric shapes, SVGs, animated text, live system information and custom shaders. Run them directly on your desktop or export seamless MP4 loops.

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
floatpaper preview scenes/spaceship_loading.toml
floatpaper live scenes/spaceship_loading.toml
floatpaper live scenes/spaceship_loading.toml --output DP-1
```

Edit the scene while running to hot-reload it. Use `floatpaper pause`, `resume`, `toggle` or `status` to control a live wallpaper.

## Examples

| Scene | Demonstrates |
|---|---|
| [minimal](scenes/minimal.toml) | Basic shapes and animation |
| [pure_black](scenes/pure_black.toml) | Layered effects |
| [cutout](scenes/cutout.toml) | Cutouts, grids and glitch |
| [spaceship_loading](scenes/spaceship_loading.toml) | SVG, rays, anchors and HUD |
| [spaceship_telemetry](scenes/spaceship_telemetry.toml) | Responsive HUD, text sequences and live metrics |

For the live telemetry example:

```sh
floatpaper live scenes/spaceship_telemetry.toml --allow-commands
```

**Only enable commands for trusted scenes.** Commands run with your user permissions, including after hot reload. Without the flag, text sources use their defaults.

## Video export (optional)

Install FFmpeg with libx264 and, for playback, mpvpaper (AUR).

```sh
floatpaper export scenes/pure_black.toml -o wallpaper.mp4
floatpaper play wallpaper.mp4
```

MP4 cannot display real-time command output; export warns and uses defaults.

## Documentation

- [Scene format, shaders, runtime controls and performance options](docs/REFERENCE.md)
- `floatpaper --help` and `floatpaper <command> --help`

License: [MIT](LICENSE).
