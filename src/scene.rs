use anyhow::{bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    f64::consts::TAU,
    fs,
    path::{Path, PathBuf},
};

fn fps_default() -> u32 {
    30
}
fn one() -> f32 {
    1.0
}
fn one_u32() -> u32 {
    1
}
fn white() -> String {
    "#ffffff".into()
}
fn black() -> String {
    "#000000".into()
}
fn transparent() -> String {
    "#00000000".into()
}
fn size_default() -> [f32; 2] {
    [0.08, 0.08]
}
fn center_default() -> [f32; 2] {
    [0.5, 0.5]
}
fn radius_default() -> [f32; 2] {
    [0.15, 0.15]
}
fn intensity_default() -> f32 {
    0.1
}
fn period_default() -> f64 {
    10.0
}
fn sides_default() -> u32 {
    5
}
fn spacing_default() -> f32 {
    48.0
}
fn effect_color_default() -> String {
    "#000000ff".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    #[serde(default = "fps_default")]
    pub fps: u32,
    #[serde(default)]
    pub loop_seconds: Option<f64>,
    #[serde(default)]
    pub seed: u64,
    #[serde(default = "black")]
    pub background: String,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub layers: Vec<Layer>,
    #[serde(default)]
    pub anchors: BTreeMap<String, Anchor>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub name: String,
    #[serde(default = "one")]
    pub opacity: f32,
    #[serde(default = "transparent")]
    pub background: String,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub shapes: Vec<Shape>,
}
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    Circle,
    Rectangle,
    Triangle,
    Polygon,
    Ray,
    Svg,
    Text,
}
impl ShapeKind {
    pub fn is_sprite(self) -> bool { matches!(self, Self::Svg | Self::Text) }
}
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Motion {
    #[default]
    Static,
    Orbit,
    Drift,
    Wrap,
    Radial,
    Linear,
}
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction { #[default] Inward, Outward }
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    #[serde(default = "center_default")] pub center: [f32; 2],
}
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit { #[default] Relative, Px }
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin { TopLeft, TopCenter, TopRight, CenterLeft, #[default] Center, CenterRight, BottomLeft, BottomCenter, BottomRight }
impl Origin {
    fn point(self) -> [f32; 2] {
        match self {
            Self::TopLeft => [0.,0.], Self::TopCenter => [0.5,0.], Self::TopRight => [1.,0.],
            Self::CenterLeft => [0.,0.5], Self::Center => [0.5,0.5], Self::CenterRight => [1.,0.5],
            Self::BottomLeft => [0.,1.], Self::BottomCenter => [0.5,1.], Self::BottomRight => [1.,1.],
        }
    }
}
fn angle_spread_default() -> f64 { 360.0 }
fn travel_default() -> [f32; 2] { [1.0, 0.0] }
fn recycle_fade_default() -> f32 { 0.15 }
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeComposite {
    #[default]
    Over,
    Cutout,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shape {
    pub kind: ShapeKind,
    #[serde(default = "one_u32")]
    pub count: u32,
    #[serde(default = "center_default")]
    pub center: [f32; 2],
    #[serde(default)] pub origin: Option<Origin>,
    #[serde(default)] pub attachment: Option<Origin>,
    #[serde(default)] pub position: Option<[f32; 2]>,
    #[serde(default)] pub position_unit: Unit,
    #[serde(default)] pub size_unit: Unit,
    #[serde(default)] pub anchor: Option<String>,
    #[serde(default)] pub anchor_offset: [f32; 2],
    #[serde(default)]
    pub spread: [f32; 2],
    #[serde(default = "size_default")]
    pub size: [f32; 2],
    #[serde(default)]
    pub size_jitter: f32,
    #[serde(default = "white")]
    pub color: String,
    #[serde(default = "one")]
    pub opacity: f32,
    /// Outline width as a fraction of shape radius; zero means filled.
    #[serde(default)]
    pub outline: f32,
    #[serde(default = "sides_default")]
    pub sides: u32,
    #[serde(default)]
    pub motion: Motion,
    #[serde(default = "radius_default")]
    pub radius: [f32; 2],
    #[serde(default)] pub direction: Direction,
    #[serde(default)] pub heading: f64,
    #[serde(default = "angle_spread_default")] pub angle_spread: f64,
    #[serde(default = "travel_default")] pub travel: [f32; 2],
    #[serde(default)] pub align_to_motion: bool,
    #[serde(default = "recycle_fade_default")] pub recycle_fade: f32,
    #[serde(default)] pub fade: f32,
    #[serde(default)] pub glow: f32,
    /// Requested period in seconds; snapped to a whole number of cycles for looping scenes.
    #[serde(default = "period_default")]
    pub period: f64,
    #[serde(default)]
    pub phase: f64,
    /// Signed turns per second; snapped to whole turns per scene loop.
    #[serde(default)]
    pub spin: f64,
    #[serde(default)]
    pub rotation: f64,
    /// Maximum random angle offset in degrees; omitted preserves the original random rotation.
    #[serde(default)]
    pub rotation_jitter: Option<f64>,
    #[serde(default)]
    pub pulse: f32,
    #[serde(default)]
    pub composite: ShapeComposite,
    #[serde(default)] pub path: Option<PathBuf>,
    #[serde(default)] pub text: Option<String>,
    #[serde(default)] pub font_family: Option<String>,
    #[serde(default = "font_size_default")] pub font_size: f32,
    #[serde(default = "line_height_default")] pub line_height: f32,
    #[serde(default = "raster_size_default")] pub raster_size: u32,
    #[serde(default)] pub preserve_color: bool,
}
fn font_size_default() -> f32 { 64.0 }
fn line_height_default() -> f32 { 1.2 }
fn raster_size_default() -> u32 { 1024 }
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    pub kind: EffectKind,
    #[serde(default = "intensity_default")]
    pub intensity: f32,
    #[serde(default = "period_default")]
    pub period: f64,
    #[serde(default)]
    pub pulse: f32,
    #[serde(default)]
    pub shader: Option<PathBuf>,
    #[serde(default)]
    pub offset: [f32; 2],
    #[serde(default, alias = "orientation")]
    pub rotation: Option<f32>,
    #[serde(default = "spacing_default")]
    pub spacing: f32,
    #[serde(default = "effect_color_default")]
    pub color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Noise,
    Crt,
    Glitch,
    Blur,
    Grid,
    Custom,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Instance {
    pub center_size: [f32; 4],
    pub color: [f32; 4],
    pub params: [f32; 4], // radians, kind (0 circle / 1 box / 2 polygon / 3 sprite / 4 ray), kind-specific data
}

#[derive(Debug, Clone, Default)]
pub struct LayerInstances {
    pub over: Vec<Instance>,
    pub cutout: Vec<Instance>,
}

impl Scene {
    pub fn load(path: &Path) -> Result<Self> {
        let source =
            fs::read_to_string(path).with_context(|| format!("Reading {}", path.display()))?;
        let scene: Self = toml::from_str(&source).context("Parsing scene TOML")?;
        scene.validate(path.parent().unwrap_or(Path::new(".")))?;
        Ok(scene)
    }
    pub fn shape_count(&self) -> usize {
        self.layers
            .iter()
            .flat_map(|l| &l.shapes)
            .map(|s| s.count as usize)
            .sum()
    }
    pub fn validate(&self, base: &Path) -> Result<()> {
        if !(1..=240).contains(&self.fps) {
            bail!("fps must be 1..=240");
        }
        if let Some(t) = self.loop_seconds {
            positive(t, "loop_seconds")?;
        }
        if self.shape_count() > 100_000 {
            bail!("At most 100000 shapes per scene");
        }
        if self.layers.len() > 64 {
            bail!("At most 64 layers");
        }
        parse_color(&self.background)?;
        if parse_color(&self.background)?[3] != 1.0 {
            bail!("Scene background must be opaque");
        }
        validate_effects(&self.effects, base)?;
        for (name, anchor) in &self.anchors {
            if name.trim().is_empty() || !anchor.center.iter().all(|v| v.is_finite() && v.abs() <= 100.0) { bail!("Invalid anchor {name}"); }
        }
        for layer in &self.layers {
            unit(layer.opacity, "layer.opacity")?;
            parse_color(&layer.background)?;
            validate_effects(&layer.effects, base)?;
            for shape in &layer.shapes {
                parse_color(&shape.color)?;
                match shape.kind {
                    ShapeKind::Svg => {
                        let path = base.join(shape.path.as_ref().context("SVG shape requires path")?);
                        if !path.is_file() { bail!("SVG asset not found: {}", path.display()); }
                        if !(16..=4096).contains(&shape.raster_size) { bail!("raster_size must be in 16..=4096"); }
                        if shape.text.is_some() || shape.font_family.is_some() { bail!("SVG shape cannot use text fields"); }
                    }
                    ShapeKind::Text => {
                        let text = shape.text.as_ref().context("Text shape requires text")?;
                        if text.trim().is_empty() || text.len() > 16384 { bail!("Text must contain 1..=16384 bytes and visible content"); }
                        if !(8.0..=256.0).contains(&shape.font_size) || !(0.5..=3.0).contains(&shape.line_height) { bail!("Invalid text font_size or line_height"); }
                        if shape.path.is_some() || shape.preserve_color { bail!("Text shapes use a tinted glyph mask, not path or preserve_color"); }
                    }
                    _ => if shape.path.is_some() || shape.text.is_some() || shape.font_family.is_some() || shape.preserve_color { bail!("Sprite fields require svg or text shape"); },
                }
                if shape.kind.is_sprite() && shape.outline != 0.0 { bail!("Sprite shapes do not support analytic outline"); }
                unit(shape.opacity, "shape.opacity")?;
                unit(shape.fade, "shape.fade")?;
                unit(shape.glow, "shape.glow")?;
                if !(0.0..=0.5).contains(&shape.recycle_fade) { bail!("recycle_fade must be in [0,0.5]"); }
                if !(0.0..=360.0).contains(&shape.angle_spread) || !shape.heading.is_finite() || shape.heading.abs() > 1e6 { bail!("Invalid motion heading or angle_spread"); }
                if !shape.travel.iter().chain(&shape.anchor_offset).all(|v| v.is_finite() && v.abs() <= 100.0) { bail!("Invalid travel or anchor_offset"); }
                if let Some(name) = &shape.anchor {
                    if !self.anchors.contains_key(name) { bail!("Unknown shape anchor: {name}"); }
                } else if shape.anchor_offset != [0.0, 0.0] { bail!("anchor_offset requires anchor"); }
                if !matches!(shape.kind, ShapeKind::Ray) && (shape.fade != 0.0 || shape.glow != 0.0) { bail!("fade and glow require ray shape"); }
                if matches!(shape.kind, ShapeKind::Ray) && shape.outline != 0.0 { bail!("Ray shapes do not support outline"); }
                if matches!(shape.motion, Motion::Radial) && !(shape.radius[0] >= 0.0 && shape.radius[1] > shape.radius[0]) { bail!("Radial radius must be [inner, outer] with 0 <= inner < outer"); }
                if matches!(shape.motion, Motion::Linear) && shape.travel == [0.0, 0.0] { bail!("Linear travel must be nonzero"); }
                if shape.align_to_motion && !matches!(shape.motion, Motion::Radial | Motion::Linear) { bail!("align_to_motion requires radial or linear motion"); }
                unit(shape.outline, "shape.outline")?;
                unit(shape.size_jitter, "shape.size_jitter")?;
                unit(shape.pulse, "shape.pulse")?;
                positive(shape.period, "shape.period")?;
                if shape.count == 0 || !(3..=32).contains(&shape.sides) {
                    bail!("Invalid shape count or polygon sides");
                }
                if shape.position.is_some() || shape.origin.is_some() {
                    if shape.anchor.is_some() || shape.center != center_default() { bail!("origin/position cannot be combined with anchor or nondefault center"); }
                }
                if let Some(p) = shape.position {
                    let max = if matches!(shape.position_unit, Unit::Px) { 100000.0 } else { 100.0 };
                    if !p.iter().all(|v| v.is_finite() && v.abs() <= max) { bail!("Invalid shape position"); }
                }
                let max_size = if matches!(shape.size_unit, Unit::Px) { 100000.0 } else { 10.0 };
                if !shape
                    .size
                    .iter()
                    .all(|v| v.is_finite() && *v > 0.0 && *v <= max_size)
                {
                    bail!("Shape sizes must be finite and positive, within the selected unit limit");
                }
                if !shape
                    .center
                    .iter()
                    .chain(&shape.spread)
                    .chain(&shape.radius)
                    .all(|v| v.is_finite() && v.abs() <= 100.0)
                {
                    bail!("Invalid shape coordinates");
                }
                if ![shape.phase, shape.spin, shape.rotation]
                    .iter()
                    .all(|v| v.is_finite() && v.abs() <= 1e6)
                {
                    bail!("Invalid shape phase/spin/rotation");
                }
                if let Some(jitter) = shape.rotation_jitter {
                    if !(0.0..=360.0).contains(&jitter) {
                        bail!("rotation_jitter must be in [0,360] degrees");
                    }
                }
            }
        }
        Ok(())
    }
    pub fn time(&self, t: f64) -> f64 {
        match self.loop_seconds {
            Some(d) => t.rem_euclid(d),
            None => t,
        }
    }
    pub fn instances(&self, t: f64) -> Vec<LayerInstances> {
        self.instances_for_aspect(t, 1.0)
    }
    pub fn instances_for_aspect(&self, t: f64, aspect: f32) -> Vec<LayerInstances> {
        self.instances_for_viewport(t, aspect, 1.0, 1.0)
    }
    pub fn instances_for_viewport(&self, t: f64, width: f32, height: f32, pixel_scale: f32) -> Vec<LayerInstances> {
        assert!(width > 0.0 && height > 0.0 && pixel_scale > 0.0);
        let aspect = width / height;
        let t = self.time(t);
        self.layers
            .iter()
            .enumerate()
            .map(|(li, layer)| {
                let mut out = LayerInstances::default();
                for (si, shape) in layer.shapes.iter().enumerate() {
                    let color = parse_color(&shape.color).expect("validated color");
                    let origin = shape.origin.unwrap_or_default();
                    let attachment = shape.attachment.unwrap_or(origin).point();
                    let size = match shape.size_unit { Unit::Relative => shape.size, Unit::Px => [shape.size[0]*pixel_scale/height, shape.size[1]*pixel_scale/height] };
                    let center = if shape.position.is_some() || shape.origin.is_some() {
                        let p = origin.point();
                        let mut offset = shape.position.unwrap_or([0.,0.]);
                        if matches!(shape.position_unit, Unit::Px) { offset[0] *= pixel_scale/width; offset[1] *= pixel_scale/height; }
                        // Positive offsets point inward from right/bottom edges.
                        [p[0] + offset[0] * if p[0] == 1. { -1. } else { 1. }, p[1] + offset[1] * if p[1] == 1. { -1. } else { 1. }]
                    } else { match &shape.anchor {
                        Some(name) => { let a = &self.anchors[name]; [a.center[0] + shape.anchor_offset[0], a.center[1] + shape.anchor_offset[1]] },
                        None => shape.center,
                    }};
                    for i in 0..shape.count {
                        let key = self.seed
                            ^ (li as u64).wrapping_mul(0xD6E8FEB86659FD93)
                            ^ (si as u64).wrapping_mul(0xA0761D6478BD642F)
                            ^ (i as u64).wrapping_mul(0xE7037ED1A0B428DB);
                        let r =
                            |n: u64| random(key.wrapping_add(n.wrapping_mul(0x9E3779B97F4A7C15)));
                        let mut x = center[0] + shape.spread[0] * (2.0 * r(1) - 1.0) as f32;
                        let mut y = center[1] + shape.spread[1] * (2.0 * r(2) - 1.0) as f32;
                        let phase = (shape.phase + if shape.count > 1 { r(3) } else { 0.0 }) * TAU;
                        let rate = cyclic_rate(1.0 / shape.period, self.loop_seconds);
                        let a = TAU * rate * t + phase;
                        let mut motion_angle = 0.0;
                        let mut motion_alpha = 1.0;
                        let progress = (rate * t + phase / TAU).rem_euclid(1.0) as f32;
                        match shape.motion {
                            Motion::Static => {}
                            Motion::Orbit => {
                                x += shape.radius[0] * a.cos() as f32;
                                y += shape.radius[1] * a.sin() as f32;
                            }
                            Motion::Drift => {
                                x += shape.radius[0]
                                    * (0.72 * a.sin() + 0.28 * (3.0 * a + r(4) * TAU).sin()) as f32;
                                y += shape.radius[1]
                                    * (0.7 * (2.0 * a + r(5) * TAU).cos() + 0.3 * (5.0 * a).sin())
                                        as f32;
                            }
                            Motion::Radial => {
                                let heading = (shape.heading + if shape.count > 1 { (r(8) - 0.5) * shape.angle_spread } else { 0.0 }).to_radians();
                                let inward = matches!(shape.direction, Direction::Inward);
                                let u = if inward { 1.0 - progress } else { progress };
                                let distance = shape.radius[0] + (shape.radius[1] - shape.radius[0]) * u;
                                x += distance * heading.cos() as f32 / aspect;
                                y += distance * heading.sin() as f32;
                                motion_angle = heading + if inward { TAU * 0.5 } else { 0.0 };
                                motion_alpha = recycle_envelope(progress, shape.recycle_fade);
                            }
                            Motion::Linear => {
                                x += shape.travel[0] * (progress - 0.5) / aspect;
                                y += shape.travel[1] * (progress - 0.5);
                                motion_angle = f64::from(shape.travel[1]).atan2(f64::from(shape.travel[0]));
                                motion_alpha = recycle_envelope(progress, shape.recycle_fade);
                            }
                            Motion::Wrap => {
                                // Travel horizontally; shader draws edge copies for a continuous wrap.
                                x = (x + (rate * t) as f32).rem_euclid(1.0);
                                y += shape.radius[1] * a.sin() as f32;
                            }
                        }
                        let jitter = 1.0 + shape.size_jitter * (2.0 * r(6) - 1.0) as f32;
                        let scale = jitter.max(0.01)
                            * (1.0 - shape.pulse * 0.5 + shape.pulse * 0.5 * a.sin() as f32);
                        let alpha = color[3] * shape.opacity * motion_alpha;
                        let angle_offset = if shape.count > 1 {
                            match shape.rotation_jitter {
                                Some(jitter) => jitter * (2.0 * r(7) - 1.0) / 360.0,
                                None if shape.align_to_motion => 0.0,
                                None => r(7), // preserve the original scene behavior
                            }
                        } else {
                            0.0
                        };
                        let angle = (shape.rotation / 360.0
                            + cyclic_rate(shape.spin, self.loop_seconds) * t
                            + angle_offset)
                            * TAU + if shape.align_to_motion { motion_angle } else { 0.0 };
                        let (kind, sides) = match shape.kind {
                            ShapeKind::Circle => (0., 3.),
                            ShapeKind::Rectangle => (1., 4.),
                            ShapeKind::Triangle => (2., 3.),
                            ShapeKind::Polygon => (2., shape.sides as f32),
                            ShapeKind::Ray => (4., shape.fade),
                            // Shape index identifies the immutable texture within this layer.
                            ShapeKind::Svg | ShapeKind::Text => (3., si as f32),
                        };
                        let sx = size[0] * scale;
                        let sy = size[1] * scale;
                        let dx = (0.5 - attachment[0]) * sx;
                        let dy = (0.5 - attachment[1]) * sy;
                        // Rotate around the selected attachment, not the quad center.
                        let (sin, cos) = angle.sin_cos();
                        x += (dx * cos as f32 - dy * sin as f32) / aspect;
                        y += dx * sin as f32 + dy * cos as f32;
                        let instance = Instance {
                            center_size: [x, y, sx, sy],
                            color: [color[0], color[1], color[2], alpha],
                            params: [angle.rem_euclid(TAU) as f32, kind, sides, if matches!(shape.kind, ShapeKind::Ray) { shape.glow } else if shape.preserve_color { 1.0 } else { shape.outline }],
                        };
                        match shape.composite {
                            ShapeComposite::Over => out.over.push(instance),
                            ShapeComposite::Cutout => out.cutout.push(instance),
                        }
                        if matches!(shape.motion, Motion::Wrap) {
                            for dx in [-1.0, 1.0] {
                                let mut copy = instance;
                                copy.center_size[0] += dx;
                                match shape.composite {
                                    ShapeComposite::Over => out.over.push(copy),
                                    ShapeComposite::Cutout => out.cutout.push(copy),
                                }
                            }
                        }
                    }
                }
                out
            })
            .collect()
    }
}

fn recycle_envelope(progress: f32, fade: f32) -> f32 {
    if fade == 0.0 { return 1.0; }
    let p = (progress.min(1.0 - progress) / fade).clamp(0.0, 1.0);
    p * p * (3.0 - 2.0 * p)
}

pub fn cyclic_rate(rate: f64, duration: Option<f64>) -> f64 {
    match duration {
        Some(d) if rate != 0.0 => (rate.abs() * d).round().max(1.0).copysign(rate) / d,
        _ => rate,
    }
}
pub fn effect_values(e: &Effect, t: f64, duration: Option<f64>) -> (f32, f32) {
    let phase = (t * cyclic_rate(1.0 / e.period, duration)).rem_euclid(1.0);
    let strength = e.intensity * (1.0 - e.pulse * 0.5 + e.pulse * 0.5 * (phase * TAU).sin() as f32);
    (strength, phase as f32)
}
fn random(mut x: u64) -> f64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^= x >> 31;
    (x >> 11) as f64 / ((1u64 << 53) as f64)
}
fn positive(x: f64, field: &str) -> Result<()> {
    if !x.is_finite() || !(0.001..=1e7).contains(&x) {
        bail!("{field} must be finite and in [0.001, 10000000]");
    }
    Ok(())
}
fn unit(x: f32, field: &str) -> Result<()> {
    if !(0.0..=1.0).contains(&x) {
        bail!("{field} must be in [0,1]");
    }
    Ok(())
}
fn validate_effects(effects: &[Effect], base: &Path) -> Result<()> {
    if effects.len() > 16 {
        bail!("At most 16 effects per layer/global chain");
    }
    for e in effects {
        if !e.intensity.is_finite() || !(0.0..=100.0).contains(&e.intensity) {
            bail!("Effect intensity must be in [0,100]");
        }
        positive(e.period, "effect.period")?;
        unit(e.pulse, "effect.pulse")?;
        match (&e.kind, &e.shader) {
            (EffectKind::Custom, Some(p)) => {
                fs::read_to_string(base.join(p))
                    .with_context(|| format!("Reading custom shader {}", base.join(p).display()))?;
            }
            (EffectKind::Custom, None) => bail!("Custom effect requires shader path"),
            (_, Some(_)) => bail!("shader path is only valid for kind=custom"),
            _ => {}
        }
        if !e.offset.iter().all(|v| v.is_finite()) {
            bail!("Effect offset must contain finete values")
        }
        if let Some(rotation) = e.rotation {
            if !rotation.is_finite() {
                bail!("Effect rotation must be finite");
            }
        }
        if !e.spacing.is_finite() || e.spacing <= 0.0 {
            bail!("Effect spacing must be > 0");
        }

        parse_color(&e.color)?;
    }
    Ok(())
}
pub fn parse_color(s: &str) -> Result<[f32; 4]> {
    let h = s.strip_prefix('#').context("Color must begin with #")?;
    if !h.is_ascii() || ![6, 8].contains(&h.len()) {
        bail!("Expected #RRGGBB or #RRGGBBAA: {s}");
    }
    let mut c = [1.0; 4];
    for i in 0..h.len() / 2 {
        c[i] = u8::from_str_radix(&h[2 * i..2 * i + 2], 16)
            .with_context(|| format!("Invalid color {s}"))? as f32
            / 255.0;
    }
    for v in &mut c[..3] {
        *v = if *v <= 0.04045 {
            *v / 12.92
        } else {
            ((*v + 0.055) / 1.055).powf(2.4)
        };
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn colors_are_linear() {
        assert!((parse_color("#80808080").unwrap()[0] - 0.21586).abs() < 0.0001);
        assert!(parse_color("#ééé").is_err());
    }
    #[test]
    fn rate_snaps_to_integer_cycles() {
        assert_eq!(cyclic_rate(0.03, Some(100.)), 0.03);
        assert_eq!(cyclic_rate(-0.001, Some(100.)), -0.01);
        assert_eq!(cyclic_rate(0., Some(100.)), 0.);
    }
    #[test]
    fn example_scenes_validate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for name in ["minimal.toml", "pure_black.toml", "cutout.toml", "spaceship_loading.toml"] {
            Scene::load(&root.join("scenes").join(name)).unwrap();
        }
    }
    #[test]
    fn cutouts_are_separate_and_loop() {
        // Keep the test independent of the editable wallpaper examples.
        let s: Scene = toml::from_str(r#"
loop_seconds = 30.0
seed = 7

[[layers]]
name = "background"
[[layers.shapes]]
kind = "triangle"
count = 8

[[layers]]
name = "cutouts"
[[layers.shapes]]
kind = "rectangle"
composite = "cutout"
count = 18
motion = "orbit"
period = 10.0
rotation = 10.0
rotation_jitter = 8.0
"#).unwrap();
        s.validate(Path::new(".")).unwrap();
        let a = s.instances(0.0);
        let b = s.instances(s.loop_seconds.unwrap());
        assert_eq!(a[0].over.len(), 8);
        assert_eq!(a[1].cutout.len(), 18);
        assert!(a[1].over.is_empty());
        assert!(a[0].cutout.is_empty());
        for instance in &a[1].cutout {
            let degrees = instance.params[0].to_degrees();
            assert!((2.0..=18.0).contains(&degrees));
        }
        assert_eq!(
            bytemuck::cast_slice::<Instance, u8>(&a[1].cutout),
            bytemuck::cast_slice::<Instance, u8>(&b[1].cutout)
        );
    }
    #[test]
    fn radial_and_linear_motion_align_in_pixel_space() {
        let s: Scene = toml::from_str(r#"
loop_seconds = 10.0
[anchors.ship]
center = [0.7, 0.6]
[[layers]]
name = "rays"
[[layers.shapes]]
kind = "ray"
anchor = "ship"
motion = "radial"
heading = 180.0
radius = [0.1, 0.9]
period = 10.0
align_to_motion = true
fade = 1.0
[[layers.shapes]]
kind = "ray"
motion = "linear"
travel = [0.4, 0.2]
period = 10.0
align_to_motion = true
"#).unwrap();
        s.validate(Path::new(".")).unwrap();
        let a = s.instances_for_aspect(2.5, 2.0);
        let b = s.instances_for_aspect(5.0, 2.0);
        let ray = a[0].over[0];
        assert!((ray.center_size[0] - 0.4).abs() < 1e-6);
        assert!((ray.center_size[1] - 0.6).abs() < 1e-6);
        assert!((ray.params[0].cos() - 1.0).abs() < 1e-6);
        assert!(b[0].over[0].center_size[0] > ray.center_size[0]);
        let l = a[0].over[1];
        assert!((l.params[0] - 0.5f32.atan()).abs() < 1e-6);
        assert_eq!(s.instances_for_aspect(0.0, 2.0)[0].over[0].color[3], 0.0);
        let end = s.instances_for_aspect(12.5, 2.0);
        assert_eq!(bytemuck::cast_slice::<Instance,u8>(&a[0].over), bytemuck::cast_slice::<Instance,u8>(&end[0].over));
    }
    #[test]
    fn pixel_layout_keeps_edge_margins_at_different_resolutions_and_scales() {
        let s: Scene = toml::from_str(r#"
[[layers]]
name = "HUD"
[[layers.shapes]]
kind = "rectangle"
origin = "top_right"
position = [50.0, 40.0]
position_unit = "px"
size = [240.0, 60.0]
size_unit = "px"
"#).unwrap();
        s.validate(Path::new(".")).unwrap();
        for (w,h,scale) in [(1920.,1080.,1.),(1440.,2560.,1.),(3840.,2160.,2.)] {
            let a = s.instances_for_viewport(0.,w,h,scale)[0].over[0].center_size;
            assert!((a[0]*w + a[2]*h/2. - (w-50.*scale)).abs() < 0.001);
            assert!((a[1]*h - a[3]*h/2. - 40.*scale).abs() < 0.001);
            assert!((a[2]*h - 240.*scale).abs() < 0.001);
        }
    }
    #[test]
    fn scene_repeats() {
        let s: Scene = toml::from_str(include_str!("../scenes/pure_black.toml")).unwrap();
        let a = s.instances(0.0);
        let b = s.instances(s.loop_seconds.unwrap());
        for (a, b) in a.iter().zip(b.iter()) {
            assert_eq!(
                bytemuck::cast_slice::<Instance, u8>(&a.over),
                bytemuck::cast_slice::<Instance, u8>(&b.over)
            );
            assert_eq!(
                bytemuck::cast_slice::<Instance, u8>(&a.cutout),
                bytemuck::cast_slice::<Instance, u8>(&b.cutout)
            );
        }
        let a = s.instances(0.125);
        let b = s.instances(s.loop_seconds.unwrap() + 0.125);
        for (a, b) in a.iter().zip(b.iter()) {
            assert_eq!(
                bytemuck::cast_slice::<Instance, u8>(&a.over),
                bytemuck::cast_slice::<Instance, u8>(&b.over)
            );
            assert_eq!(
                bytemuck::cast_slice::<Instance, u8>(&a.cutout),
                bytemuck::cast_slice::<Instance, u8>(&b.cutout)
            );
        }
    }
}
