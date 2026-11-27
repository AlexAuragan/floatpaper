use crate::scene::{self, Effect, EffectKind, Instance, Scene};
use anyhow::{anyhow, bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use std::{borrow::Cow, collections::BTreeMap, fs, ops::Range, path::Path, sync::mpsc, time::Instant};
use wgpu::util::DeviceExt;

const DEFAULT_INTERNAL: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const COMPACT_INTERNAL: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn internal_bytes(format: wgpu::TextureFormat) -> u64 {
    match format {
        DEFAULT_INTERNAL => 8,
        COMPACT_INTERNAL => 4,
        _ => unreachable!("unsupported intermediate format"),
    }
}
const COMMON: &str = include_str!("../shaders/common.wgsl");
const MAX_PASSES: usize = 2200; // 64 layers * (16 double-pass effects + composite) + global + present
const BLEND: wgpu::BlendState = wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING;
const CUTOUT_BLEND: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: wgpu::Adapter,
}
impl Gpu {
    pub fn instance() -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor::default())
    }
    pub fn new(instance: &wgpu::Instance, surface: Option<&wgpu::Surface<'_>>) -> Result<Self> {
        Self::with_preference(instance, surface, wgpu::PowerPreference::LowPower)
    }

    pub fn with_preference(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        power_preference: wgpu::PowerPreference,
    ) -> Result<Self> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference,
            compatible_surface: surface,
            force_fallback_adapter: false,
        }))
        .context("No compatible GPU adapter. Check Vulkan/Mesa drivers")?;
        Self::from_adapter(adapter)
    }

    // wgpu 0.19 exposes vendor/device IDs but not a DRM render-node identity.
    // Restrict to Vulkan, for which these are PCI IDs, and reject ambiguity.
    pub fn matching_pci_adapter(
        instance: &wgpu::Instance,
        surface: &wgpu::Surface<'_>,
        vendor: u32,
        device: u32,
    ) -> Result<wgpu::Adapter> {
        let mut matches = instance.enumerate_adapters(wgpu::Backends::VULKAN)
            .into_iter().filter(|adapter| {
                let info = adapter.get_info();
                info.vendor == vendor && info.device == device
                    && adapter.is_surface_supported(surface)
            });
        let adapter = matches.next().with_context(|| format!(
            "No surface-compatible Vulkan adapter with PCI ID {vendor:04x}:{device:04x}"
        ))?;
        if matches.next().is_some() {
            bail!("Ambiguous Vulkan adapter PCI ID {vendor:04x}:{device:04x}");
        }
        Ok(adapter)
    }

    pub fn from_adapter(adapter: wgpu::Adapter) -> Result<Self> {
        eprintln!(
            "GPU: {} ({:?}, {:?})",
            adapter.get_info().name,
            adapter.get_info().backend,
            adapter.get_info().device_type
        );
        let limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());
        let required_features = if std::env::var_os("FLOATPAPER_PROFILE").is_some() {
            if !adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
                bail!("GPU profiling requested, but the selected adapter lacks TIMESTAMP_QUERY");
            }
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("floatpaper"),
                required_features,
                required_limits: limits,
            },
            None,
        ))
        .context("Creating wgpu device")?;
        Ok(Self {
            device,
            queue,
            adapter,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniform {
    resolution: [f32; 2],
    phase: f32,
    strength: f32,

    loop_phase: f32,
    time: f32,
    rotation: f32,
    spacing: f32,

    offset: [f32; 2],
    pad: [f32; 2],

    color: [f32; 4],
    audio: [f32; 4],
    audio_extra: [f32; 4],
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}
impl Target {
    fn new(
        gpu: &Gpu,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        uniforms: &wgpu::Buffer,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Self {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("intermediate layer"),
            size: extent(width, height),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("postprocess input"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: uniforms,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<Uniform>() as u64),
                    }),
                },
            ],
        });
        Self {
            texture,
            view,
            bind,
        }
    }
}

// A single bounded cache per renderer: combine the most expensive contiguous
// run of static layers over transparent black, then blend it in layer order.
struct StaticLayerCache {
    range: Range<usize>,
    target: Target,
    valid: bool,
}

fn geometry_is_static(layer: &scene::Layer) -> bool {
    layer.shapes.iter().all(|shape| {
        !matches!(shape.kind, scene::ShapeKind::Spectrum)
            && matches!(shape.motion, scene::Motion::Static)
            && shape.spin == 0.0
            && shape.pulse == 0.0
            && shape.text_source.is_none()
            && shape.audio.is_none()
    })
}

fn effect_is_static(effect: &Effect) -> bool {
    effect.pulse == 0.0 && effect.audio.is_none()
        && matches!(effect.kind, EffectKind::Grid | EffectKind::Blur | EffectKind::Crt)
}

fn layer_is_static(layer: &scene::Layer) -> bool {
    geometry_is_static(layer) && layer.effects.iter().all(effect_is_static)
}

fn effect_pass_count(effect: &Effect) -> usize {
    if effect.kind == EffectKind::Blur && effect.rotation.is_none() {
        2
    } else {
        1
    }
}

// Keep the existing combined cache budget, not a per-texture budget.
const CACHE_BUDGET: u64 = 64 * 1024 * 1024;

fn cache_fits(width: u32, height: u32, textures: u64, format: wgpu::TextureFormat) -> bool {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(internal_bytes(format) * textures))
        .is_some_and(|bytes| bytes <= CACHE_BUDGET)
}

fn static_prefix(scene: &Scene) -> Option<(usize, usize)> {
    let mut best = None;
    let mut best_score = 0;
    for (index, layer) in scene.layers.iter().enumerate() {
        if !geometry_is_static(layer) {
            continue;
        }
        // Stop at the first dynamic effect; a later static effect still
        // depends on the changing input produced by that effect.
        let count = layer.effects.iter().take_while(|e| effect_is_static(e)).count();
        if count == 0 || count == layer.effects.len() {
            continue;
        }
        let score = 1 + layer.effects[..count].iter().map(effect_pass_count).sum::<usize>();
        if score > best_score {
            best = Some((index, count));
            best_score = score;
        }
    }
    best
}

struct StaticPrefixCache {
    layer: usize,
    effects: usize,
    target: Target,
    valid: bool,
}

impl StaticPrefixCache {
    fn new(
        gpu: &Gpu,
        scene: &Scene,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        uniform: &wgpu::Buffer,
        width: u32,
        height: u32,
        has_layer_cache: bool,
        format: wgpu::TextureFormat,
    ) -> Option<Self> {
        let textures = if has_layer_cache { 2 } else { 1 };
        if std::env::var_os("FLOATPAPER_DISABLE_CACHE").is_some()
            || std::env::var_os("FLOATPAPER_DISABLE_PREFIX_CACHE").is_some()
            || !cache_fits(width, height, textures, format)
        {
            return None;
        }
        let (layer, effects) = static_prefix(scene)?;
        eprintln!("Static effect prefix cache: layer {} ({}) first {} effects ({}x{})",
            layer, scene.layers[layer].name, effects, width, height);
        Some(Self {
            layer,
            effects,
            target: Target::new(gpu, layout, sampler, uniform, width, height, format),
            valid: false,
        })
    }
}

fn static_run(scene: &Scene) -> Option<Range<usize>> {
    let mut best = None;
    let mut best_score = 0;
    let mut start = 0;
    let mut score = 0;
    for (i, layer) in scene.layers.iter().enumerate() {
        if !layer_is_static(layer) {
            start = i + 1;
            score = 0;
            continue;
        }
        // Count geometry, compositing and full-screen effect passes.
        score += 2 + layer.effects.iter().map(effect_pass_count).sum::<usize>();
        // A single unfiltered layer is not worth a full-size cached texture.
        if score > 2 && score > best_score {
            best = Some(start..i + 1);
            best_score = score;
        }
    }
    best
}

impl StaticLayerCache {
    fn new(
        gpu: &Gpu,
        scene: &Scene,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        uniform: &wgpu::Buffer,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Option<Self> {
        // Opt-out for visual/performance comparisons; cap extra allocation at
        // 64 MiB per output, accounting for the selected texture format.
        if std::env::var_os("FLOATPAPER_DISABLE_CACHE").is_some()
            || !cache_fits(width, height, 1, format)
        {
            return None;
        }
        let range = static_run(scene)?;
        eprintln!("Static layer cache: {}..{} ({}x{})", range.start, range.end, width, height);
        Some(Self {
            range,
            target: Target::new(gpu, layout, sampler, uniform, width, height, format),
            valid: false,
        })
    }
}

// Profile one in 15 submitted frames. Readback is asynchronous and never waits
// for the GPU; if the previous sample is still pending, skip the next sample.
const PROFILE_PASSES: usize = 512;
const PROFILE_INTERVAL: u64 = 15;
struct FrameProfiler {
    label: String,
    query: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    pending: Option<mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
    labels: Vec<&'static str>,
    recording: bool,
    truncated: bool,
    failed: bool,
    submitted: u64,
    window_frames: u64,
    window_start: Instant,
    samples: Vec<f64>,
    categories: BTreeMap<&'static str, f64>,
}

impl FrameProfiler {
    fn new(gpu: &Gpu, label: String) -> Self {
        let size = (PROFILE_PASSES * 16) as u64;
        let query = gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("Floatpaper pass timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: (PROFILE_PASSES * 2) as u32,
        });
        let resolve = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Floatpaper timestamp resolve"),
            size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Floatpaper timestamp readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            label, query, resolve, readback, pending: None, labels: Vec::new(),
            recording: false, truncated: false, failed: false, submitted: 0,
            window_frames: 0, window_start: Instant::now(), samples: Vec::new(),
            categories: BTreeMap::new(),
        }
    }

    fn prepare(&mut self, gpu: &Gpu) {
        if let Some(receiver) = self.pending.as_ref() {
            gpu.device.poll(wgpu::Maintain::Poll);
            match receiver.try_recv() {
                Ok(Ok(())) => {
                    let count = self.labels.len();
                    let mapped = self.readback.slice(..(count * 16) as u64).get_mapped_range();
                    let period = f64::from(gpu.queue.get_timestamp_period()) / 1_000_000.0;
                    let mut total = 0.0;
                    for (label, pair) in self.labels.iter().zip(mapped.chunks_exact(16)) {
                        let start = u64::from_ne_bytes(pair[..8].try_into().expect("timestamp start"));
                        let end = u64::from_ne_bytes(pair[8..].try_into().expect("timestamp end"));
                        let ms = end.wrapping_sub(start) as f64 * period;
                        total += ms;
                        *self.categories.entry(*label).or_default() += ms;
                    }
                    drop(mapped);
                    self.readback.unmap();
                    self.samples.push(total);
                    self.pending = None;
                }
                Ok(Err(e)) => {
                    eprintln!("GPU profiler {}: readback failed: {e}", self.label);
                    self.pending = None;
                    self.failed = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    eprintln!("GPU profiler {}: readback callback disconnected", self.label);
                    self.pending = None;
                    self.failed = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        self.recording = !self.failed && self.pending.is_none()
            && (self.submitted + 1) % PROFILE_INTERVAL == 0;
        if self.recording {
            self.labels.clear();
            self.truncated = false;
        }
    }

    fn timestamps(&mut self, label: &'static str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        if !self.recording {
            return None;
        }
        if self.labels.len() == PROFILE_PASSES {
            self.truncated = true;
            return None;
        }
        let index = (self.labels.len() * 2) as u32;
        self.labels.push(label);
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.query,
            beginning_of_pass_write_index: Some(index),
            end_of_pass_write_index: Some(index + 1),
        })
    }

    fn finish(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if !self.recording {
            return;
        }
        if self.truncated || self.labels.is_empty() {
            eprintln!("GPU profiler {}: sample discarded (pass count outside supported range)", self.label);
            self.recording = false;
            return;
        }
        let count = (self.labels.len() * 2) as u32;
        encoder.resolve_query_set(&self.query, 0..count, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback, 0, u64::from(count) * 8);
    }

    // Called only after the command buffer has been submitted.
    fn submitted(&mut self, width: u32, height: u32) {
        self.submitted += 1;
        self.window_frames += 1;
        if self.recording {
            let (tx, rx) = mpsc::channel();
            self.readback.slice(..(self.labels.len() * 16) as u64)
                .map_async(wgpu::MapMode::Read, move |result| {
                    // The renderer may have been dropped during a scene reload.
                    let _ = tx.send(result);
                });
            self.pending = Some(rx);
            self.recording = false;
        }
        let elapsed = self.window_start.elapsed().as_secs_f64();
        if elapsed < 5.0 {
            return;
        }
        let fps = self.window_frames as f64 / elapsed;
        let n = self.samples.len();
        if n > 0 {
            let mean = self.samples.iter().sum::<f64>() / n as f64;
            self.samples.sort_by(f64::total_cmp);
            let p95 = self.samples[((n as f64 * 0.95).ceil() as usize).saturating_sub(1)];
            let mut categories: Vec<_> = self.categories.iter().collect();
            categories.sort_by(|a, b| b.1.total_cmp(a.1));
            let detail = categories.iter().map(|(label, sum)| {
                format!("{label}={:.3}", **sum / n as f64)
            }).collect::<Vec<_>>().join(" ");
            eprintln!(
                "PROFILE {} {}x{} submit_fps={fps:.2} samples={n} gpu_pass_sum_ms={mean:.3} p95_ms={p95:.3} | {detail}",
                self.label, width, height,
            );
        } else {
            eprintln!("PROFILE {} {}x{} submit_fps={fps:.2} samples=0 (no completed GPU samples)", self.label, width, height);
        }
        self.window_start = Instant::now();
        self.window_frames = 0;
        self.samples.clear();
        self.categories.clear();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scissor {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn sparse_glitch_eligible(layer: &scene::Layer, clear: wgpu::Color, terminal: bool) -> bool {
    // Only a transparent geometry layer followed by one fused glitch effect.
    // Other effects or a nontransparent background may fill the entire screen.
    terminal && clear.a == 0.0 && layer.effects.len() == 1
        && layer.effects[0].kind == EffectKind::Glitch
}

fn glitch_scissor(instances: &[Instance], width: u32, height: u32, strength: f32) -> Option<Scissor> {
    if instances.is_empty() || width == 0 || height == 0 || !strength.is_finite() {
        return None;
    }
    let w = f64::from(width);
    let h = f64::from(height);
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for instance in instances {
        let [x, y, sx, sy] = instance.center_size.map(f64::from);
        // Extreme coordinates could lose too much precision in the vertex
        // transform; keep the original full-screen path for those cases.
        if [x, y, sx, sy].iter().any(|v| !v.is_finite() || v.abs() > 16.0) {
            return None;
        }
        // geometry.wgsl expands the quad by 1.08 before rotation.
        // The circumradius bounds every rotation and the entire rasterized
        // quad, including antialiasing. Both axes scale with viewport height.
        let radius = 1.08 * 0.5 * sx.hypot(sy) * h;
        min_x = min_x.min(x * w - radius);
        max_x = max_x.max(x * w + radius);
        min_y = min_y.min(y * h - radius);
        max_y = max_y.max(y * h + radius);
    }

    // glitch.wgsl: absolute band displacement <= 0.09*k*width;
    // chromatic split <= 0.009*k*width. Bound both axes regardless of
    // orientation or phase, and include filtering/rasterization slack.
    let pad = 0.099 * f64::from(strength.clamp(0.0, 1.0)) * w + 4.0;
    let x0 = (min_x - pad).floor().clamp(0.0, w) as u32;
    let y0 = (min_y - pad).floor().clamp(0.0, h) as u32;
    let x1 = (max_x + pad).ceil().clamp(0.0, w) as u32;
    let y1 = (max_y + pad).ceil().clamp(0.0, h) as u32;
    // None means full-screen fallback, never an invalid zero-size scissor.
    if x0 >= x1 || y0 >= y1 || (x0 == 0 && y0 == 0 && x1 == width && y1 == height) {
        return None;
    }
    Some(Scissor { x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
}

struct EffectPipeline {
    config: Effect,
    passes: Vec<wgpu::RenderPipeline>,
    terminal: bool,
}
struct LayerPipeline {
    effects: Vec<EffectPipeline>,
    clear: wgpu::Color,
}
// Sprite bind groups retain their textures. Assets are immutable for the
// lifetime of a renderer; resize changes only the geometry viewport.
struct CommandTexture {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
    last: Option<String>,
    valid: bool,
}
struct SpriteGeometry {
    over: wgpu::RenderPipeline,
    cutout: wgpu::RenderPipeline,
    assets: BTreeMap<(usize, usize), Vec<wgpu::BindGroup>>,
    commands: BTreeMap<(usize, usize), CommandTexture>,
    fonts: Option<cosmic_text::FontSystem>,
}
impl SpriteGeometry {
    fn new(gpu: &Gpu, scene: &Scene, base: &Path, viewport: &wgpu::Buffer,
        sampler: &wgpu::Sampler, format: wgpu::TextureFormat) -> Result<Option<Self>> {
        if !scene.layers.iter().any(|l| l.shapes.iter().any(|s| s.kind.is_sprite())) {
            return Ok(None);
        }
        let d = &gpu.device;
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite geometry interface"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0, visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false, min_binding_size: wgpu::BufferSize::new(16) },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sprite geometry"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("../shaders/sprite.wgsl"))),
        });
        let buffers = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Instance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &wgpu::vertex_attr_array![0=>Float32x4,1=>Float32x4,2=>Float32x4],
        }];
        let over = pipeline(d, "sprite over", &shader, &layout, format, Some(BLEND), &buffers);
        let cutout = pipeline(d, "sprite cutout", &shader, &layout, format, Some(CUTOUT_BLEND), &buffers);
        let mut assets = BTreeMap::new();
        let mut commands = BTreeMap::new();
        let mut fonts = None;
        let mut bytes = 0usize;
        for (li, layer) in scene.layers.iter().enumerate() {
            for (si, shape) in layer.shapes.iter().enumerate() {
                if !shape.kind.is_sprite() { continue; }
                let rasters = if let Some(name) = &shape.text_source {
                    crate::sprites::sequence_rasters(shape, &scene.text_sources[name].frames(), base, &mut fonts)
                } else {
                    crate::sprites::rasterize(shape, base, &mut fonts).map(|r| vec![r])
                }.with_context(|| format!("Rasterize layer '{}' shape {si}", layer.name))?;
                let mut bindings = Vec::with_capacity(rasters.len());
                let command = shape.text_source.as_ref().is_some_and(|name| scene.text_sources[name].is_command());
                for (frame_index, mut raster) in rasters.into_iter().enumerate() {
                bytes += raster.rgba.len();
                if bytes > crate::sprites::MAX_BYTES { bail!("Sprite textures exceed 64 MiB per renderer"); }
                check_size(gpu, raster.width, raster.height)?;
                raster.premultiplied_linear_srgb();
                let size = extent(raster.width, raster.height);
                let texture = d.create_texture(&wgpu::TextureDescriptor {
                    label: Some("sprite asset"), size, mip_level_count: 1, sample_count: 1,
                    dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                gpu.queue.write_texture(
                    wgpu::ImageCopyTexture { texture: &texture, mip_level: 0,
                        origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    &raster.rgba,
                    wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(raster.width * 4), rows_per_image: Some(raster.height) },
                    size,
                );
                let view = texture.create_view(&Default::default());
                let bind = d.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("sprite asset binding"), layout: &layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: viewport.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
                    ],
                });
                bindings.push(bind);
                if command && frame_index == 1 {
                    commands.insert((li,si), CommandTexture { texture, width: raster.width, height: raster.height, last: None, valid: false });
                }
                }
                assets.insert((li, si), bindings);
            }
        }
        eprintln!("Sprite assets: {} ({:.2} MiB)", assets.len(), bytes as f64 / 1048576.);
        Ok(Some(Self { over, cutout, assets, commands, fonts }))
    }
}

// Preserve the original analytic draw batching and split only when the
// texture changes. Over instances retain declaration order; cutouts follow.
fn sprite_key(instance: &Instance) -> Option<usize> {
    if instance.params[1] == 3.0 { Some(instance.params[2] as usize) } else { None }
}
fn geometry_runs(flat: &[Instance], range: Range<u32>) -> Vec<(Option<usize>, Range<u32>)> {
    let mut runs = Vec::new();
    let mut start = range.start;
    while start < range.end {
        let key = sprite_key(&flat[start as usize]);
        let mut end = start + 1;
        while end < range.end && sprite_key(&flat[end as usize]) == key { end += 1; }
        runs.push((key, start..end));
        start = end;
    }
    runs
}

pub struct Renderer {
    scene: Scene,
    width: u32,
    height: u32,
    output_format: wgpu::TextureFormat,
    pixel_scale: f32,
    text_paused: bool,
    geometry: wgpu::RenderPipeline,
    geometry_cutout: wgpu::RenderPipeline,
    sprites: Option<SpriteGeometry>,
    geo_bind: wgpu::BindGroup,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    uniform: wgpu::Buffer,
    stride: usize,
    targets: [Target; 4], // 0,1 layer ping-pong; 2,3 scene ping-pong
    static_cache: Option<StaticLayerCache>,
    prefix_cache: Option<StaticPrefixCache>,
    composite: wgpu::RenderPipeline,
    present: wgpu::RenderPipeline,
    layers: Vec<LayerPipeline>,
    global: Vec<EffectPipeline>,
    background: wgpu::Color,
    uniform_bytes: Vec<u8>,
    profiler: Option<FrameProfiler>,
    sparse_glitch: bool,
}

impl Renderer {
    pub fn new(
        gpu: &Gpu,
        scene: Scene,
        base: &Path,
        width: u32,
        height: u32,
        output_format: wgpu::TextureFormat,
    ) -> Result<Self> {
        check_size(gpu, width, height)?;
        let d = &gpu.device;
        // Capture WGSL/pipeline errors explicitly, particularly during hot reload.
        d.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = (|| -> Result<Self> {
            let stride = align_up(
                std::mem::size_of::<Uniform>(),
                d.limits().min_uniform_buffer_offset_alignment as usize,
            );
            let uniform = d.create_buffer(&wgpu::BufferDescriptor {
                label: Some("postprocess parameters"),
                size: (stride * MAX_PASSES) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let geometry_uniform = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("viewport"),
                contents: bytemuck::cast_slice(&[width as f32, height as f32, 0., 0.]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("effect interface"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: wgpu::BufferSize::new(
                                std::mem::size_of::<Uniform>() as u64
                            ),
                        },
                        count: None,
                    },
                ],
            });
            let geo_layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("geometry interface"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                }],
            });
            let geo_bind = d.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("viewport binding"),
                layout: &geo_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: geometry_uniform.as_entire_binding(),
                }],
            });
            let sampler = d.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("linear clamp"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            let internal_format = if std::env::var_os("FLOATPAPER_COMPACT_INTERMEDIATES").is_some() {
                COMPACT_INTERNAL
            } else {
                DEFAULT_INTERNAL
            };
            eprintln!("Intermediate texture format: {internal_format:?}");
            let targets = std::array::from_fn(|_| {
                Target::new(gpu, &layout, &sampler, &uniform, width, height, internal_format)
            });
            let geo_shader = d.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("geometry"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                    "../shaders/geometry.wgsl"
                ))),
            });
            let geometry = pipeline(
                d,
                "geometry",
                &geo_shader,
                &geo_layout,
                internal_format,
                Some(BLEND),
                &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x4,1=>Float32x4,2=>Float32x4],
                }],
            );
            let geometry_cutout = pipeline(
                d,
                "geometry cutout",
                &geo_shader,
                &geo_layout,
                internal_format,
                Some(CUTOUT_BLEND),
                &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x4,1=>Float32x4,2=>Float32x4],
                }],
            );
            let sprites = SpriteGeometry::new(gpu, &scene, base, &geometry_uniform, &sampler, internal_format)?;
            let composite = effect_pipeline(
                d,
                &layout,
                "composite",
                include_str!("../shaders/composite.wgsl"),
                internal_format,
                Some(BLEND),
            );
            let present = effect_pipeline(
                d,
                &layout,
                "present",
                include_str!("../shaders/present.wgsl"),
                output_format,
                None,
            );
            let layers = scene
                .layers
                .iter()
                .map(|l| {
                    Ok(LayerPipeline {
                        effects: compile_effects(
                            d, &layout, &l.effects, base, internal_format,
                            if std::env::var_os("FLOATPAPER_DISABLE_LAYER_FUSION").is_some() {
                                None
                            } else {
                                Some(FinalPass::Composite(l.opacity))
                            },
                        )?,
                        clear: clear_color(&l.background)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let final_output = if std::env::var_os("FLOATPAPER_DISABLE_FINAL_FUSION").is_some() {
                None
            } else {
                Some(FinalPass::Present(output_format))
            };
            let global = compile_effects(d, &layout, &scene.effects, base, internal_format, final_output)?;
            let background = clear_color(&scene.background)?;
            let instance_capacity = (scene.shape_count() * 3).max(1);
            let instances = d.create_buffer(&wgpu::BufferDescriptor {
                label: Some("shape instances"),
                size: (instance_capacity * std::mem::size_of::<Instance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let static_cache = StaticLayerCache::new(gpu, &scene, &layout, &sampler, &uniform, width, height, internal_format);
            let prefix_cache = StaticPrefixCache::new(
                gpu, &scene, &layout, &sampler, &uniform, width, height, static_cache.is_some(), internal_format,
            );
            Ok(Self {
                scene,
                width,
                height,
                output_format,
                pixel_scale: 1.0,
                text_paused: false,
                geometry,
                geometry_cutout,
                sprites,
                geo_bind,
                instances,
                instance_capacity,
                uniform,
                stride,
                targets,
                static_cache,
                prefix_cache,
                composite,
                present,
                layers,
                global,
                background,
                uniform_bytes: Vec::with_capacity(stride * MAX_PASSES),
                profiler: None,
                sparse_glitch: std::env::var_os("FLOATPAPER_DISABLE_SPARSE_GLITCH").is_none(),
            })
        })();
        let validation = pollster::block_on(d.pop_error_scope());
        if let Some(e) = validation {
            return Err(anyhow!("GPU pipeline validation: {e}"));
        }
        result
    }
    pub fn set_text_paused(&mut self, paused: bool) { self.text_paused = paused; }
    pub fn update_command_text(&mut self, gpu: &Gpu, values: &crate::text_commands::Values) {
        let Some(sprites) = &mut self.sprites else { return; };
        // Shared source + font + canvas yields identical pixels for glow and foreground.
        // Retain only the most recent raster to bound temporary CPU storage.
        let mut previous: Option<((String, Option<String>, u32, u32, u32, u32), crate::sprites::Raster)> = None;
        for (&(li,si), asset) in &mut sprites.commands {
            let shape = &self.scene.layers[li].shapes[si];
            let name = shape.text_source.as_ref().expect("command source");
            let value = values.get(name);
            if asset.last.as_ref() == value { continue; }
            asset.last = value.cloned();
            asset.valid = false;
            let Some(value) = value else { continue; };
            let mut shape = shape.clone();
            shape.text = Some(value.clone());
            let key = (name.clone(), shape.font_family.clone(), shape.font_size.to_bits(), shape.line_height.to_bits(), asset.width, asset.height);
            if previous.as_ref().is_none_or(|(k, _)| *k != key) {
                previous = None;
                match crate::sprites::rasterize(&shape, Path::new("."), &mut sprites.fonts)
                    .and_then(|r| crate::sprites::fit_canvas(r, asset.width, asset.height)) {
                    Ok(mut raster) => {
                        raster.premultiplied_linear_srgb();
                        previous = Some((key, raster));
                    }
                    Err(e) => eprintln!("Text command {name}: cannot display output: {e:#}; using default (increase layout if needed)"),
                }
            }
            if let Some((_, raster)) = &previous {
                    gpu.queue.write_texture(
                        wgpu::ImageCopyTexture { texture: &asset.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                        &raster.rgba,
                        wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(asset.width * 4), rows_per_image: Some(asset.height) },
                        extent(asset.width, asset.height),
                    );
                    asset.valid = true;
            }
        }
    }
    pub fn set_pixel_scale(&mut self, scale: u32) {
        assert!(scale > 0);
        if self.pixel_scale != scale as f32 {
            self.pixel_scale = scale as f32;
            if let Some(cache) = &mut self.static_cache { cache.valid = false; }
            if let Some(cache) = &mut self.prefix_cache { cache.valid = false; }
        }
    }
    pub fn enable_profile(&mut self, gpu: &Gpu, label: String) {
        if gpu.device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            self.profiler = Some(FrameProfiler::new(gpu, label));
        }
    }

    pub fn profile_submitted(&mut self) {
        if let Some(profiler) = &mut self.profiler {
            profiler.submitted(self.width, self.height);
        }
    }

    pub fn render(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        t: f64,
        audio: &crate::audio::Frame,
    ) {
        if let Some(profiler) = &mut self.profiler {
            profiler.prepare(gpu);
        }
        let t = self.scene.time(t);
        let layer_instances = self.scene.instances_with_audio(t, self.width as f32, self.height as f32, self.pixel_scale, audio);
        // Pack both instance groups into one buffer, recording each layer's draw ranges.
        let mut flat = Vec::new();
        let ranges: Vec<_> = layer_instances
            .into_iter()
            .map(|layer| {
                let start = flat.len() as u32;
                flat.extend(layer.over);
                let middle = flat.len() as u32;
                flat.extend(layer.cutout);
                let end = flat.len() as u32;
                (start..middle, middle..end)
            })
            .collect();
        assert!(flat.len() <= self.instance_capacity);
        if !flat.is_empty() {
            gpu.queue
                .write_buffer(&self.instances, 0, bytemuck::cast_slice(&flat));
        }
        self.uniform_bytes.clear();
        let common = Uniform {
            resolution: [self.width as f32, self.height as f32],
            phase: 0.0,
            strength: 1.0,
            loop_phase: self.scene.loop_seconds.map_or(0.0, |d| (t / d) as f32),
            time: t as f32,
            rotation: 0.0,
            spacing: 48.0,
            offset: [0.0; 2],
            pad: [0.0; 2],
            color: [0.0, 0.0, 0.0, 1.0],
            audio: [audio.signals.onset,audio.signals.kick,audio.signals.bass,audio.signals.mid],
            audio_extra: [audio.signals.treble,audio.signals.volume,audio.signals.onset*audio.signals.onset_strength,audio.signals.kick*audio.signals.kick_strength],
        };
        let mut passes = Passes {
            encoder,
            bytes: &mut self.uniform_bytes,
            stride: self.stride,
            common,
            audio,
            profiler: self.profiler.as_mut().filter(|p| p.recording),
        };
        passes.clear(&self.targets[2].view, self.background, "scene_clear");
        for (i, layer) in self.layers.iter().enumerate() {
            let cached = self.static_cache.as_ref().filter(|cache| cache.range.contains(&i));
            if cached.map_or(true, |cache| !cache.valid) {
                let destination = match cached {
                    Some(cache) => {
                        if i == cache.range.start {
                            passes.clear(&cache.target.view, wgpu::Color::TRANSPARENT, "cache_clear");
                        }
                        &cache.target.view
                    }
                    None => &self.targets[2].view,
                };
                let prefix = self.prefix_cache.as_ref().filter(|cache| cache.layer == i);
                if prefix.map_or(true, |cache| !cache.valid) {
                    let mut pass = passes.begin(
                        &self.targets[0].view,
                        wgpu::LoadOp::Clear(layer.clear),
                        "geometry",
                    );
                    let (over, cutout) = &ranges[i];
                    if self.sprites.is_none() {
                        // Original fast path for scenes without texture-backed shapes.
                        if !over.is_empty() {
                            pass.set_pipeline(&self.geometry);
                            pass.set_bind_group(0, &self.geo_bind, &[]);
                            pass.set_vertex_buffer(0, self.instances.slice(..));
                            pass.draw(0..6, over.clone());
                        }
                        if !cutout.is_empty() {
                            pass.set_pipeline(&self.geometry_cutout);
                            pass.set_bind_group(0, &self.geo_bind, &[]);
                            pass.set_vertex_buffer(0, self.instances.slice(..));
                            pass.draw(0..6, cutout.clone());
                        }
                    } else if let Some(sprites) = &self.sprites {
                        pass.set_vertex_buffer(0, self.instances.slice(..));
                        for (range, is_cutout) in [(over, false), (cutout, true)] {
                            for (key, run) in geometry_runs(&flat, range.clone()) {
                                if let Some(si) = key {
                                    pass.set_pipeline(if is_cutout { &sprites.cutout } else { &sprites.over });
                                    let frame = self.scene.layers[i].shapes[si].text_source.as_ref()
                                        .map_or(0, |name| self.scene.text_sources[name].frame(t, self.scene.loop_seconds, self.text_paused));
                                    let frame = if !self.text_paused && sprites.commands.get(&(i, si)).is_some_and(|asset| asset.valid) { 1 } else { frame };
                                    pass.set_bind_group(0, &sprites.assets[&(i, si)][frame], &[]);
                                } else {
                                    pass.set_pipeline(if is_cutout { &self.geometry_cutout } else { &self.geometry });
                                    pass.set_bind_group(0, &self.geo_bind, &[]);
                                }
                                pass.draw(0..6, run);
                            }
                        }
                    }
                }
                let (effects, terminal) = match layer.effects.split_last() {
                    Some((last, rest)) if last.terminal => (rest, Some(last)),
                    _ => (layer.effects.as_slice(), None),
                };
                let current = if let Some(cache) = prefix {
                    if !cache.valid {
                        let input = passes.chain(
                            &self.targets[0],
                            &self.targets[0],
                            &self.targets[1],
                            &effects[..cache.effects],
                            t,
                            self.scene.loop_seconds,
                        );
                        // Exact GPU-side copy: no blending, filtering or CPU readback.
                        passes.encoder.copy_texture_to_texture(
                            wgpu::ImageCopyTexture {
                                texture: &input.texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::ImageCopyTexture {
                                texture: &cache.target.texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            extent(self.width, self.height),
                        );
                    }
                    // Sample the immutable cached prefix directly. Only the
                    // remaining effects write to the two scratch textures.
                    passes.chain(
                        &cache.target,
                        &self.targets[0],
                        &self.targets[1],
                        &effects[cache.effects..],
                        t,
                        self.scene.loop_seconds,
                    )
                } else {
                    passes.chain(
                        &self.targets[0],
                        &self.targets[0],
                        &self.targets[1],
                        effects,
                        t,
                        self.scene.loop_seconds,
                    )
                };
                if let Some(last) = terminal {
                    let (strength, phase) = scene::effect_values(&last.config, t, self.scene.loop_seconds, audio);
                    let scissor = if self.sparse_glitch && prefix.is_none()
                        && sparse_glitch_eligible(&self.scene.layers[i], layer.clear, last.terminal)
                    {
                        let over = &ranges[i].0;
                        glitch_scissor(&flat[over.start as usize..over.end as usize],
                            self.width, self.height, strength)
                    } else {
                        None
                    };
                    passes.draw_region(
                        if scissor.is_some() { "glitch_composite_sparse" } else { composite_effect_label(last.config.kind) },
                        &last.passes[0],
                        current,
                        destination,
                        wgpu::LoadOp::Load,
                        strength,
                        phase,
                        last.config.offset,
                        last.config.rotation.unwrap_or(0.0),
                        last.config.spacing,
                        scene::parse_color(&last.config.color).expect("validated effect color"),
                        scissor,
                    );
                } else {
                    passes.draw(
                        "composite",
                        &self.composite,
                        current,
                        destination,
                        wgpu::LoadOp::Load,
                        self.scene.layers[i].opacity,
                        0.0,
                        [0.0; 2],
                        0.0,
                        48.0,
                        [0.0, 0.0, 0.0, 1.0],
                    );
                }
            }
            // Blend the whole cached run exactly once at its original position.
            if let Some(cache) = cached {
                if i + 1 == cache.range.end {
                    passes.draw(
                        "cached_composite",
                        &self.composite,
                        &cache.target,
                        &self.targets[2].view,
                        wgpu::LoadOp::Load,
                        1.0,
                        0.0,
                        [0.0; 2],
                        0.0,
                        48.0,
                        [0.0, 0.0, 0.0, 1.0],
                    );
                }
            }
        }
        if let Some(cache) = &mut self.static_cache {
            cache.valid = true;
        }
        if let Some(cache) = &mut self.prefix_cache {
            cache.valid = true;
        }
        if let Some((last, prefix)) = self.global.split_last().filter(|(last, _)| last.terminal) {
            let current = passes.chain(
                &self.targets[2],
                &self.targets[2],
                &self.targets[3],
                prefix,
                t,
                self.scene.loop_seconds,
            );
            let (strength, phase) = scene::effect_values(&last.config, t, self.scene.loop_seconds, audio);
            passes.draw(
                fused_effect_label(last.config.kind),
                &last.passes[0],
                current,
                output,
                wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                strength,
                phase,
                last.config.offset,
                last.config.rotation.unwrap_or(0.0),
                last.config.spacing,
                scene::parse_color(&last.config.color).expect("validated effect color"),
            );
        } else {
            let current = passes.chain(
                &self.targets[2],
                &self.targets[2],
                &self.targets[3],
                &self.global,
                t,
                self.scene.loop_seconds,
            );
            passes.draw(
                "present",
                &self.present,
                current,
                output,
                wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                if self.output_format.is_srgb() {
                    0.0
                } else {
                    1.0
                },
                0.0,
                [0.0; 2],
                0.0,
                48.0,
                [0.0, 0.0, 0.0, 1.0],
            );
        }
        if let Some(profiler) = passes.profiler.as_deref_mut() {
            profiler.finish(passes.encoder);
        }
        gpu.queue
            .write_buffer(&self.uniform, 0, &self.uniform_bytes);
    }
}
struct Passes<'a> {
    encoder: &'a mut wgpu::CommandEncoder,
    bytes: &'a mut Vec<u8>,
    stride: usize,
    common: Uniform,
    audio: &'a crate::audio::Frame,
    profiler: Option<&'a mut FrameProfiler>,
}

impl Passes<'_> {
    fn begin<'b>(
        &'b mut self,
        view: &'b wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
        label: &'static str,
    ) -> wgpu::RenderPass<'b> {
        let timestamp_writes = self.profiler.as_deref_mut().and_then(|p| p.timestamps(label));
        self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes,
            occlusion_query_set: None,
        })
    }

    fn clear(&mut self, view: &wgpu::TextureView, color: wgpu::Color, label: &'static str) {
        drop(self.begin(view, wgpu::LoadOp::Clear(color), label));
    }

    fn draw(
        &mut self,
        label: &'static str,
        pipeline: &wgpu::RenderPipeline,
        input: &Target,
        output: &wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
        strength: f32,
        phase: f32,
        effect_offset: [f32; 2],
        effect_rotation: f32,
        effect_spacing: f32,
        effect_color: [f32; 4],
    ) {
        self.draw_region(label, pipeline, input, output, load, strength, phase,
            effect_offset, effect_rotation, effect_spacing, effect_color, None);
    }

    fn draw_region(
        &mut self,
        label: &'static str,
        pipeline: &wgpu::RenderPipeline,
        input: &Target,
        output: &wgpu::TextureView,
        load: wgpu::LoadOp<wgpu::Color>,
        strength: f32,
        phase: f32,
        effect_offset: [f32; 2],
        effect_rotation: f32,
        effect_spacing: f32,
        effect_color: [f32; 4],
        scissor: Option<Scissor>,
    ) {
        let offset = self.bytes.len();

        assert!(offset / self.stride < MAX_PASSES);

        self.bytes.resize(offset + self.stride, 0);

        let u = Uniform {
            strength,
            phase,
            offset: effect_offset,
            rotation: effect_rotation,
            spacing: effect_spacing,
            color: effect_color,
            ..self.common
        };

        self.bytes[offset..offset + std::mem::size_of::<Uniform>()]
            .copy_from_slice(bytemuck::bytes_of(&u));

        let mut pass = self.begin(output, load, label);
        if let Some(rect) = scissor {
            pass.set_scissor_rect(rect.x, rect.y, rect.width, rect.height);
        }

        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &input.bind, &[offset as u32]);
        pass.draw(0..3, 0..1);
    }

    fn chain<'b>(
        &mut self,
        input: &'b Target,
        a: &'b Target,
        b: &'b Target,
        effects: &[EffectPipeline],
        t: f64,
        duration: Option<f64>,
    ) -> &'b Target {
        let mut current = input;

        for effect in effects {
            let (strength, phase) = scene::effect_values(&effect.config, t, duration, self.audio);

            let effect_color =
                scene::parse_color(&effect.config.color).expect("validated effect color");

            let rotation = effect.config.rotation.unwrap_or(0.0);

            let rotations = [rotation, rotation + 90.0];

            let pass_count = effect_pass_count(&effect.config);

            for pipeline in &effect.passes {
                for &angle in &rotations[..pass_count] {
                    let next = if std::ptr::eq(current, a) { b } else { a };

                    self.draw(
                        effect_label(effect.config.kind),
                        pipeline,
                        current,
                        &next.view,
                        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        strength,
                        phase,
                        effect.config.offset,
                        angle,
                        effect.config.spacing,
                        effect_color,
                    );

                    current = next;
                }
            }
        }

        current
    }
}

fn effect_label(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Noise => "noise",
        EffectKind::Crt => "crt",
        EffectKind::Glitch => "glitch",
        EffectKind::Blur => "blur",
        EffectKind::Grid => "grid",
        EffectKind::Custom => "custom",
    }
}

fn clear_color(s: &str) -> Result<wgpu::Color> {
    let c = scene::parse_color(s)?;
    Ok(wgpu::Color {
        r: (c[0] * c[3]) as f64,
        g: (c[1] * c[3]) as f64,
        b: (c[2] * c[3]) as f64,
        a: c[3] as f64,
    })
}
// Fuse only a known single-pass built-in. Custom shader bodies retain
// their original compilation and execution path, as do two-pass blurs.
fn final_effect_eligible(effect: &Effect) -> bool {
    effect.kind != EffectKind::Custom && effect_pass_count(effect) == 1
}

fn fused_effect_label(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Noise => "noise_present",
        EffectKind::Crt => "crt_present",
        EffectKind::Glitch => "glitch_present",
        EffectKind::Blur => "blur_present",
        EffectKind::Grid => "grid_present",
        EffectKind::Custom => unreachable!("custom effects are not fused"),
    }
}

fn fused_effect_body(body: &str, format: wgpu::TextureFormat) -> String {
    let effect = body.replacen("fn effect(", "fn final_effect(", 1);
    let (present_helper, _) = include_str!("../shaders/present.wgsl")
        .split_once("fn effect(")
        .expect("built-in presentation shader must define effect");
    // Bake the format choice into the shader instead of overloading strength:
    // the final effect still needs its real intensity parameter.
    let rgb = if format.is_srgb() { "c.rgb" } else { "linear_to_srgb(c.rgb)" };
    format!(
        "{effect}\n{present_helper}\nfn effect(uv: vec2<f32>) -> vec4<f32> {{\n    let c = final_effect(uv);\n    return vec4<f32>({rgb}, 1.0);\n}}"
    )
}

#[derive(Clone, Copy)]
enum FinalPass {
    Present(wgpu::TextureFormat),
    Composite(f32),
}

fn composite_effect_label(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Noise => "noise_composite",
        EffectKind::Crt => "crt_composite",
        EffectKind::Glitch => "glitch_composite",
        EffectKind::Blur => "blur_composite",
        EffectKind::Grid => "grid_composite",
        EffectKind::Custom => unreachable!("custom effects are not fused"),
    }
}

fn composite_effect_body(body: &str, opacity: f32) -> String {
    let effect = body.replacen("fn effect(", "fn layer_effect(", 1);
    // Layer opacity is constant until the scene is reloaded. Keep the
    // effect's strength uniform independent, and scale all RGBA channels.
    // Debug formatting emits a valid float literal even for 0 and 1.
    format!(
        "{effect}\nfn effect(uv: vec2<f32>) -> vec4<f32> {{\n    return layer_effect(uv) * {opacity:?};\n}}"
    )
}

fn compile_effects(
    d: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    effects: &[Effect],
    base: &Path,
    internal_format: wgpu::TextureFormat,
    final_output: Option<FinalPass>,
) -> Result<Vec<EffectPipeline>> {
    effects
        .iter()
        .enumerate()
        .map(|(index, e)| {
            let bodies: Vec<(&str, Cow<'_, str>)> = match e.kind {
                EffectKind::Noise => vec![(
                    "noise",
                    Cow::Borrowed(if std::env::var_os("FLOATPAPER_LEGACY_NOISE").is_some() {
                        include_str!("../shaders/noise.wgsl")
                    } else {
                        include_str!("../shaders/noise_fast.wgsl")
                    }),
                )],
                EffectKind::Crt => {
                    vec![("crt", Cow::Borrowed(include_str!("../shaders/crt.wgsl")))]
                }
                EffectKind::Glitch => vec![(
                    "glitch",
                    Cow::Owned(format!("const GLITCH_CONTINUOUS: bool = {};\n{}", e.continuous, include_str!("../shaders/glitch.wgsl"))),
                )],
                EffectKind::Blur => {
                    vec![("blur", Cow::Borrowed(include_str!("../shaders/blur.wgsl")))]
                }
                EffectKind::Grid => {
                    vec![("grid", Cow::Borrowed(include_str!("../shaders/grid.wgsl")))]
                }
                EffectKind::Custom => {
                    let p = base.join(e.shader.as_ref().context("Custom effect missing shader")?);
                    vec![(
                        "custom",
                        Cow::Owned(
                            fs::read_to_string(&p)
                                .with_context(|| format!("Reading {}", p.display()))?,
                        ),
                    )]
                }
            };
            if let Some(final_pass) = final_output.filter(|_| {
                index + 1 == effects.len() && final_effect_eligible(e) && bodies.len() == 1
            }) {
                let (body, label, format, blend) = match final_pass {
                    FinalPass::Present(format) => {
                        let label = fused_effect_label(e.kind);
                        eprintln!("Final effect fusion: {label}");
                        (fused_effect_body(&bodies[0].1, format), label, format, None)
                    }
                    FinalPass::Composite(opacity) => {
                        let label = composite_effect_label(e.kind);
                        eprintln!("Layer effect fusion: {label} (opacity={opacity})");
                        (composite_effect_body(&bodies[0].1, opacity), label, internal_format, Some(BLEND))
                    }
                };
                return Ok(EffectPipeline {
                    config: e.clone(),
                    passes: vec![effect_pipeline(d, layout, label, &body, format, blend)],
                    terminal: true,
                });
            }
            Ok(EffectPipeline {
                config: e.clone(),
                passes: bodies
                    .iter()
                    .map(|(label, body)| effect_pipeline(d, layout, label, body, internal_format, None))
                    .collect(),
                terminal: false,
            })
        })
        .collect()
}
fn effect_pipeline(
    d: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    label: &str,
    body: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Owned(format!("{COMMON}\n{body}"))),
    });
    pipeline(d, label, &shader, layout, format, blend, &[])
}
fn pipeline(
    d: &wgpu::Device,
    label: &str,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    buffers: &[wgpu::VertexBufferLayout<'_>],
) -> wgpu::RenderPipeline {
    let pipeline_layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[layout],
        push_constant_ranges: &[],
    });
    d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: "vs_main",
            buffers,
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
    })
}
pub fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}
pub fn align_up(x: usize, a: usize) -> usize {
    (x + a - 1) / a * a
}
pub fn check_size(gpu: &Gpu, width: u32, height: u32) -> Result<()> {
    let max = gpu.device.limits().max_texture_dimension_2d;
    if width == 0 || height == 0 || width > max || height > max {
        bail!("Invalid dimensions {width}x{height}; GPU limit {max}");
    }
    Ok(())
}

#[cfg(test)]
mod static_cache_tests {
    use super::*;

    fn sample_scene() -> Scene {
        toml::from_str(r##"
[[layers]]
name = "moving"
[[layers.shapes]]
kind = "triangle"
motion = "drift"

[[layers]]
name = "static_grid"
effects = [{kind = "grid"}]
[[layers.shapes]]
kind = "rectangle"
composite = "cutout"

[[layers]]
name = "static_blur"
effects = [{kind = "blur"}]

[[layers]]
name = "animated_glitch"
effects = [{kind = "glitch", pulse = 0.0}]

[[layers]]
name = "static_crt"
effects = [{kind = "crt"}]
"##).unwrap()
    }

    #[test]
    fn caches_best_contiguous_run_without_crossing_dynamic_layers() {
        assert_eq!(static_run(&sample_scene()), Some(1..3));
    }

    #[test]
    fn static_classification_is_conservative() {
        let mut s = sample_scene();
        assert!(!layer_is_static(&s.layers[0]));
        assert!(layer_is_static(&s.layers[1]));
        assert!(!layer_is_static(&s.layers[3]));
        assert!(layer_is_static(&s.layers[4]));
        s.layers[1].shapes[0].spin = 0.1;
        assert!(!layer_is_static(&s.layers[1]));
        s.layers[1].shapes[0].spin = 0.0;
        s.layers[1].shapes[0].pulse = 0.1;
        assert!(!layer_is_static(&s.layers[1]));
        s.layers[2].effects[0].pulse = 0.1;
        assert!(!layer_is_static(&s.layers[2]));
        s.layers[4].effects[0].kind = EffectKind::Custom;
        assert!(!layer_is_static(&s.layers[4]));
        s.layers[4].effects[0].kind = EffectKind::Noise;
        assert!(!layer_is_static(&s.layers[4]));
        assert_eq!(static_run(&s), None);
    }

    #[test]
    fn prefix_stops_at_first_dynamic_effect_and_skips_animated_geometry() {
        let mut s = sample_scene();
        let mut grid = s.layers[1].effects[0].clone();
        let blur = s.layers[2].effects[0].clone();
        let glitch = s.layers[3].effects[0].clone();
        s.layers[3].effects = vec![grid.clone(), blur.clone(), glitch.clone(), grid.clone()];
        s.layers[0].effects = s.layers[3].effects.clone();
        assert_eq!(static_prefix(&s), Some((3, 2)));
        s.layers[3].effects[0].pulse = 0.2;
        assert_eq!(static_prefix(&s), None);
        grid.kind = EffectKind::Custom;
        s.layers[3].effects = vec![grid, blur, glitch];
        assert_eq!(static_prefix(&s), None);
    }

    #[test]
    fn prefix_selects_most_expensive_run_and_respects_cache_budget() {
        let mut s = sample_scene();
        let grid = s.layers[1].effects[0].clone();
        let blur = s.layers[2].effects[0].clone();
        let glitch = s.layers[3].effects[0].clone();
        s.layers[1].effects = vec![grid, glitch.clone()];
        s.layers[2].effects = vec![blur, glitch];
        assert_eq!(static_prefix(&s), Some((2, 1)));
        assert!(cache_fits(1440, 2560, 2, DEFAULT_INTERNAL));
        assert!(cache_fits(3840, 2160, 1, DEFAULT_INTERNAL));
        assert!(!cache_fits(3840, 2160, 2, DEFAULT_INTERNAL));
        assert!(cache_fits(3840, 2160, 2, COMPACT_INTERNAL));
        assert_eq!(internal_bytes(DEFAULT_INTERNAL), 8);
        assert_eq!(internal_bytes(COMPACT_INTERNAL), 4);
    }


    #[test]
    fn final_fusion_only_accepts_single_pass_builtin_effects() {
        let s = sample_scene();
        let mut blur = s.layers[2].effects[0].clone();
        assert!(!final_effect_eligible(&blur));
        blur.rotation = Some(45.0);
        assert!(final_effect_eligible(&blur));
        assert!(final_effect_eligible(&s.layers[4].effects[0]));
        blur.kind = EffectKind::Custom;
        assert!(!final_effect_eligible(&blur));
    }

    #[test]
    fn fused_shader_preserves_effect_and_selects_output_conversion() {
        let crt = include_str!("../shaders/crt.wgsl");
        let srgb = fused_effect_body(crt, wgpu::TextureFormat::Bgra8UnormSrgb);
        assert!(srgb.contains("fn final_effect("));
        assert!(srgb.contains("return vec4<f32>(c.rgb, 1.0);"));
        assert_eq!(srgb.matches("fn effect(").count(), 1);
        let linear = fused_effect_body(crt, wgpu::TextureFormat::Bgra8Unorm);
        assert!(linear.contains("return vec4<f32>(linear_to_srgb(c.rgb), 1.0);"));
    }

    #[test]
    fn layer_fusion_scales_premultiplied_rgba_without_changing_strength() {
        let glitch = include_str!("../shaders/glitch.wgsl");
        for opacity in [0.0_f32, 0.375, 1.0] {
            let body = composite_effect_body(glitch, opacity);
            assert!(body.contains("fn layer_effect("));
            assert!(body.contains("params.strength"));
            assert!(body.contains(&format!("return layer_effect(uv) * {opacity:?};")));
            assert_eq!(body.matches("fn effect(").count(), 1);
        }
    }


    #[test]
    fn sparse_glitch_bounds_include_rotation_filtering_and_max_displacement() {
        let shape = Instance {
            center_size: [0.5, 0.5, 0.1, 0.1],
            color: [1.0; 4],
            params: [0.7853982, 1.0, 4.0, 0.0],
        };
        let rect = glitch_scissor(&[shape], 1000, 1000, 1.0).unwrap();
        // Radius ~76.4 px, displacement/split 99 px, margin 4 px.
        assert_eq!(rect, Scissor { x: 320, y: 320, width: 360, height: 360 });
        let weak = glitch_scissor(&[shape], 1000, 1000, 0.0).unwrap();
        assert!(weak.width < rect.width);
        let mut edge = shape;
        edge.center_size[0] = 0.0;
        let clipped = glitch_scissor(&[edge], 1000, 1000, 1.0).unwrap();
        assert_eq!(clipped.x, 0);
        assert!(clipped.width >= 180);
        assert_eq!(glitch_scissor(&[], 1000, 1000, 1.0), None);
    }

    #[test]
    fn sparse_glitch_rejects_opaque_or_multi_effect_layers() {
        let mut s = sample_scene();
        let transparent = wgpu::Color::TRANSPARENT;
        assert!(sparse_glitch_eligible(&s.layers[3], transparent, true));
        assert!(!sparse_glitch_eligible(&s.layers[3], wgpu::Color::BLACK, true));
        assert!(!sparse_glitch_eligible(&s.layers[3], transparent, false));
        let grid = s.layers[1].effects[0].clone();
        s.layers[3].effects.insert(0, grid);
        assert!(!sparse_glitch_eligible(&s.layers[3], transparent, true));
    }

}
