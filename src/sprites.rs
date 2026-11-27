//! Rasterize immutable SVG and shaped text assets at renderer creation.
use crate::scene::{Shape, ShapeKind};
use anyhow::{bail, Context, Result};
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap};
use resvg::{tiny_skia, usvg};
use std::{fs, io::Read, path::Path};

pub const MAX_SIDE: u32 = 4096;
const MAX_SVG_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    // Straight-alpha sRGB bytes; converted to linear premultiplied before upload.
    pub rgba: Vec<u8>,
}

impl Raster {
    fn blank(width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            bail!("Sprite dimensions {width}x{height} exceed 1..={MAX_SIDE}");
        }
        Ok(Self { width, height, rgba: vec![0; width as usize * height as usize * 4] })
    }
    pub fn premultiplied_linear_srgb(&mut self) {
        // Store linear-premultiplied RGB encoded as sRGB, with linear alpha.
        // Hardware decoding happens before filtering, preventing dark fringes.
        // A lookup table replaces up to three nonlinear conversions per pixel.
        let decode: [f32; 256] = std::array::from_fn(|i| {
            let v = i as f32 / 255.;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        });
        let encode_white: [u8; 256] = std::array::from_fn(|i| {
            let linear = i as f32 / 255.;
            let encoded = if linear <= 0.0031308 { linear * 12.92 } else { 1.055 * linear.powf(1./2.4) - 0.055 };
            (encoded * 255.).round().clamp(0.,255.) as u8
        });
        for p in self.rgba.chunks_exact_mut(4) {
            if p[3] == 0 { p[..3].fill(0); continue; }
            if p[3] == 255 { continue; }
            if p[..3] == [255,255,255] { let v = encode_white[p[3] as usize]; p[..3].fill(v); continue; }
            let a = p[3] as f32 / 255.;
            for c in &mut p[..3] {
                let linear = decode[*c as usize] * a;
                let encoded = if linear <= 0.0031308 { linear * 12.92 } else { 1.055 * linear.powf(1./2.4) - 0.055 };
                *c = (encoded * 255.).round().clamp(0.,255.) as u8;
            }
        }
    }
}

pub fn rasterize(shape: &Shape, base: &Path, fonts: &mut Option<FontSystem>) -> Result<Raster> {
    match shape.kind {
        ShapeKind::Svg => {
            let path = base.join(shape.path.as_ref().context("SVG requires path")?);
            let file = fs::File::open(&path).with_context(|| format!("Open SVG {}", path.display()))?;
            if !file.metadata()?.is_file() { bail!("SVG asset must be a regular file"); }
            let mut data = Vec::new();
            file.take(MAX_SVG_BYTES + 1).read_to_end(&mut data)?;
            if data.len() as u64 > MAX_SVG_BYTES { bail!("SVG source exceeds 16 MiB"); }
            let mut options = usvg::Options::default();
            options.resources_dir = path.parent().map(Path::to_path_buf);
            // SVG text uses system fonts; vector-only SVGs also work without them.
            options.fontdb_mut().load_system_fonts();
            let tree = usvg::Tree::from_data(&data, &options)
                .with_context(|| format!("Parse SVG {}", path.display()))?;
            let size = tree.size();
            let scale = shape.raster_size as f32 / size.width().max(size.height());
            let width = (size.width() * scale).ceil().max(1.) as u32;
            let height = (size.height() * scale).ceil().max(1.) as u32;
            let mut out = Raster::blank(width, height)?;
            let mut pixmap = tiny_skia::Pixmap::new(width, height).context("Allocate SVG raster")?;
            resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
            // tiny-skia gives premultiplied sRGB. Undo that premultiplication
            // before converting to linear-premultiplied storage.
            for (src, dst) in pixmap.data().chunks_exact(4).zip(out.rgba.chunks_exact_mut(4)) {
                dst[3] = src[3];
                if src[3] != 0 {
                    for i in 0..3 {
                        dst[i] = ((src[i] as u32 * 255 + src[3] as u32 / 2) / src[3] as u32).min(255) as u8;
                    }
                }
            }
            Ok(out)
        }
        ShapeKind::Text => {
            let fonts = fonts.get_or_insert_with(FontSystem::new);
            let family = shape.font_family.as_deref().map(Family::Name).unwrap_or(Family::Monospace);
            let text = shape.text.as_deref().context("Text shape requires text")?;
            let line_height = shape.font_size * shape.line_height;
            let mut buffer = Buffer::new(fonts, Metrics::new(shape.font_size, line_height));
            buffer.set_size(fonts, None, None);
            buffer.set_wrap(fonts, Wrap::None);
            buffer.set_text(fonts, text, Attrs::new().family(family), Shaping::Advanced);
            buffer.shape_until_scroll(fonts, false);
            // Include the full advance width, leading spaces and blank rows;
            // reserve extra room for glyph bearings/ascenders/descenders.
            let padding = shape.font_size.ceil() as i32;
            let mut width = 0f32;
            let mut height = text.split('\n').count() as f32 * line_height;
            for run in buffer.layout_runs() {
                width = width.max(run.line_w);
                height = height.max(run.line_top + run.line_height);
                if run.glyphs.iter().any(|g| g.glyph_id == 0) {
                    bail!("Missing glyph in text shape; install a font covering the requested Unicode characters");
                }
            }
            let w = width.ceil() as u32 + 2 * padding as u32;
            let h = height.ceil() as u32 + 2 * padding as u32;
            let mut out = Raster::blank(w, h)?;
            let mut cache = SwashCache::new();
            let mut clipped = false;
            buffer.draw(fonts, &mut cache, Color::rgb(255,255,255), |x,y,_,_,color| {
                let x = x + padding;
                let y = y + padding;
                if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                    if color.a() != 0 { clipped = true; }
                    return;
                }
                let i = ((y as u32 * w + x as u32) * 4) as usize;
                // Composite overlapping glyphs (combining marks) in alpha-mask mode.
                let old = out.rgba[i+3] as u32;
                let a = color.a() as u32;
                out.rgba[i..i+3].fill(255);
                out.rgba[i+3] = (a + (old * (255-a) + 127)/255) as u8;
            });
            if clipped { bail!("Text glyph extends outside allocated sprite"); }
            if !text.trim().is_empty() && !out.rgba.chunks_exact(4).any(|p| p[3] != 0) {
                bail!("Text rendered no visible glyphs; check installed fonts");
            }
            // Trim the safety padding, retaining the original layout box and
            // any actual glyph overhang (including spaces and blank rows).
            let mut left = padding as u32;
            let mut top = padding as u32;
            let mut right = (padding as u32 + width.ceil() as u32).max(left+1);
            let mut bottom = (padding as u32 + height.ceil() as u32).max(top+1);
            for y in 0..h {
                for x in 0..w {
                    if out.rgba[((y*w+x)*4+3) as usize] != 0 {
                        left = left.min(x); top = top.min(y);
                        right = right.max(x+1); bottom = bottom.max(y+1);
                    }
                }
            }
            let mut trimmed = Raster::blank(right-left, bottom-top)?;
            for y in top..bottom {
                let from = ((y*w+left)*4) as usize;
                let to = ((y-top)*trimmed.width*4) as usize;
                let len = trimmed.width as usize*4;
                trimmed.rgba[to..to+len].copy_from_slice(&out.rgba[from..from+len]);
            }
            Ok(trimmed)
        }
        _ => bail!("Only SVG and text shapes can be rasterized"),
    }
}

pub fn fit_canvas(raster: Raster, width: u32, height: u32) -> Result<Raster> {
    if raster.width > width || raster.height > height {
        bail!("Text raster {}x{} exceeds fixed canvas {width}x{height}", raster.width, raster.height);
    }
    if raster.width == width && raster.height == height { return Ok(raster); }
    let mut canvas = Raster::blank(width, height)?;
    for y in 0..raster.height as usize {
        let from = y * raster.width as usize * 4;
        let to = y * width as usize * 4;
        let len = raster.width as usize * 4;
        canvas.rgba[to..to+len].copy_from_slice(&raster.rgba[from..from+len]);
    }
    Ok(canvas)
}

// Use one common canvas per sequence, so a short or blank frame never
// stretches to fill the shape or shifts its neighbors.
pub fn sequence_rasters(shape: &Shape, frames: &[&str], base: &Path, fonts: &mut Option<FontSystem>) -> Result<Vec<Raster>> {
    let mut rasters = Vec::new();
    let mut width = 1;
    let mut height = 1;
    let mut bytes = 0usize;
    for text in frames {
        let mut frame = shape.clone();
        frame.text = Some((*text).to_owned());
        let raster = rasterize(&frame, base, fonts)?;
        width = width.max(raster.width);
        height = height.max(raster.height);
        bytes += raster.rgba.len();
        if bytes > MAX_BYTES { bail!("Sequence raster budget exceeds 64 MiB"); }
        rasters.push(raster);
    }
    if width as usize * height as usize * 4 * rasters.len() > MAX_BYTES {
        bail!("Padded sequence textures exceed 64 MiB");
    }
    rasters.into_iter().map(|raster| {
        if raster.width == width && raster.height == height { return Ok(raster); }
        let mut canvas = Raster::blank(width, height)?;
        for y in 0..raster.height as usize {
            let from = y * raster.width as usize * 4;
            let to = y * width as usize * 4;
            let len = raster.width as usize * 4;
            canvas.rgba[to..to+len].copy_from_slice(&raster.rgba[from..from+len]);
        }
        Ok(canvas)
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn svg_raster_and_linear_premultiplication() {
        let shape: Shape = toml::from_str("kind = \"svg\"\npath = \"assets/ship.svg\"\nraster_size = 128").unwrap();
        let mut raster = rasterize(&shape, &Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes"), &mut None).unwrap();
        assert_eq!(raster.width.max(raster.height), 128);
        assert!(raster.rgba.chunks_exact(4).any(|p| p[3] == 0));
        assert!(raster.rgba.chunks_exact(4).any(|p| p[3] == 255));
        raster.premultiplied_linear_srgb();
        assert!(raster.rgba.chunks_exact(4).all(|p| p[3] != 0 || p[..3] == [0,0,0]));
    }
}
