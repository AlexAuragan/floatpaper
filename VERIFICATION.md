# Verification record

Performed in the generation environment:

- Parsed Cargo.toml and both scene TOML files with Python's standard TOML parser. All parsed successfully. The demo contains 166 configured shape instances before wrap copies.
- Ran the exporter's exact FFmpeg encoding options on eight synthetic 64x64 RGBA frames. FFmpeg exited successfully; ffprobe confirmed eight H.264 yuv420p frames in the MP4 container. The synthetic test did not exercise the Rust renderer or GPU readback. This FFmpeg build reported the requested sRGB transfer metadata as unknown, so color matching across players is not certified.
- Reviewed the source and checked relevant crate APIs against public documentation; no Rust syntax parser or compiler was present.

Not performed: cargo check, cargo test, WGSL compilation, GPU rendering, Wayland integration, hardware-decoded playback, actual scene video encoding, performance or seam measurements. Network access from the build container was unavailable, so no Rust toolchain or dependencies could be downloaded. The archive contains source, not a binary. The Rust tests are included but unexecuted.

First test on the target machine: `cargo check` from the extracted project root. If it fails, keep the first complete compiler diagnostic.
