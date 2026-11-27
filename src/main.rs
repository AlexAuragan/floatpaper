mod scene;
mod sprites;
mod renderer;
mod desktop;
mod visibility;
mod control;
mod export;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::Command};

#[derive(Parser)]
#[command(version, about)]
struct Cli { #[command(subcommand)] command: Action }

#[derive(Subcommand)]
enum Action {
    /// Check configuration and animation parameters (no GPU required).
    Validate { scene: PathBuf },
    /// Open a normal Wayland window for scene development.
    Preview { scene: PathBuf, #[arg(long, default_value_t = 960)] width: u32, #[arg(long, default_value_t = 540)] height: u32 },
    /// Draw on Wayland's background layer; defaults to all connected outputs.
    Live { scene: PathBuf, #[arg(long)] output: Option<String> },
    /// Freeze the running live wallpaper without removing it.
    Pause,
    /// Resume the running live wallpaper.
    Resume,
    /// Toggle manual pause on the running live wallpaper.
    Toggle,
    /// Print the running live wallpaper's manual pause state.
    Status,
    /// Render a deterministic H.264 video using FFmpeg.
    Export { scene: PathBuf, #[arg(short, long)] output: PathBuf,
        #[arg(long, default_value_t = 1920)] width: u32, #[arg(long, default_value_t = 1080)] height: u32,
        #[arg(long)] fps: Option<u32>, #[arg(long)] duration: Option<f64>,
        #[arg(long, default_value_t = 18)] crf: u8,
        /// Permit an export whose duration does not match a full scene loop.
        #[arg(long)] allow_non_loop: bool,
        #[arg(long)] overwrite: bool },
    /// Play a video on the background layer using mpvpaper.
    Play { video: PathBuf, #[arg(long, default_value = "ALL")] output: String,
        #[arg(long, default_value = "auto-safe")] hwdec: String },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Action::Validate { scene: path } => {
            let scene = scene::Scene::load(&path)?;
            println!("Valid scene: {} layers, {} shapes, {} fps, loop {:?} s", scene.layers.len(), scene.shape_count(), scene.fps, scene.loop_seconds);
            println!("Configuration only; use preview or export to validate WGSL on a GPU.");
            Ok(())
        }
        Action::Preview { scene, width, height } => desktop::run(&scene, desktop::Mode::Preview { width, height }),
        Action::Live { scene, output } => desktop::run(&scene, desktop::Mode::Live { output }),
        Action::Pause => control::send(control::Command::Pause),
        Action::Resume => control::send(control::Command::Resume),
        Action::Toggle => control::send(control::Command::Toggle),
        Action::Status => control::send(control::Command::Status),
        Action::Export { scene, output, width, height, fps, duration, crf, allow_non_loop, overwrite } => {
            export::run(&scene, &output, export::Options { width, height, fps, duration, crf, allow_non_loop, overwrite })
        }
        Action::Play { video, output, hwdec } => {
            let video = video.canonicalize().context("Video path does not exist")?;
            // Limit the setting to a single mpv option value (no injected options).
            if hwdec.is_empty() || !hwdec.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { bail!("Invalid hwdec value"); }
            let status = Command::new("mpvpaper").arg("-o")
                .arg(format!("no-audio loop-file=inf hwdec={hwdec} panscan=1.0"))
                .arg(output).arg(video).status().context("Cannot launch mpvpaper; install mpvpaper from the AUR")?;
            if !status.success() { bail!("mpvpaper exited with {status}"); }
            Ok(())
        }
    }
}
