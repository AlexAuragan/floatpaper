use crate::scene::{self, Effect, EffectKind, Instance, Scene};
use anyhow::{anyhow, bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use std::{borrow::Cow, collections::BTreeMap, fs, ops::Range, path::Path, sync::mpsc, time::Instant};
use wgpu::util::DeviceExt;

const INTERNAL: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
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
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: surface,
            force_fallback_adapter: false,
        }))
        .context("No compatible GPU adapter. Check Vulkan/Mesa drivers")?;
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
    ) -> Self {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("intermediate layer"),
            size: extent(width, height),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: INTERNAL,
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
        matches!(shape.motion, scene::Motion::Static)
            && shape.spin == 0.0
            && shape.pulse == 0.0
    })
}

fn effect_is_static(effect: &Effect) -> bool {
    effect.pulse == 0.0
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

fn cache_fits(width: u32, height: u32, textures: u64) -> bool {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(8 * textures))
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
    ) -> Option<Self> {
        let textures = if has_layer_cache { 2 } else { 1 };
        if std::env::var_os("FLOATPAPER_DISABLE_CACHE").is_some()
            || std::env::var_os("FLOATPAPER_DISABLE_PREFIX_CACHE").is_some()
            || !cache_fits(width, height, textures)
        {
            return None;
        }
        let (layer, effects) = static_prefix(scene)?;
        eprintln!("Static effect prefix cache: layer {} ({}) first {} effects ({}x{})",
            layer, scene.layers[layer].name, effects, width, height);
        Some(Self {
            layer,
            effects,
            target: Target::new(gpu, layout, sampler, uniform, width, height),
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
    ) -> Option<Self> {
        // Opt-out for visual/performance comparisons; cap extra allocation at
        // 64 MiB per output (RGBA16Float = 8 bytes per pixel).
        if std::env::var_os("FLOATPAPER_DISABLE_CACHE").is_some()
            || !cache_fits(width, height, 1)
        {
            return None;
        }
        let range = static_run(scene)?;
        eprintln!("Static layer cache: {}..{} ({}x{})", range.start, range.end, width, height);
        Some(Self {
            range,
            target: Target::new(gpu, layout, sampler, uniform, width, height),
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

struct EffectPipeline {
    config: Effect,
    passes: Vec<wgpu::RenderPipeline>,
}
struct LayerPipeline {
    effects: Vec<EffectPipeline>,
    clear: wgpu::Color,
}
pub struct Renderer {
    scene: Scene,
    width: u32,
    height: u32,
    output_format: wgpu::TextureFormat,
    geometry: wgpu::RenderPipeline,
    geometry_cutout: wgpu::RenderPipeline,
    geo_bind: wgpu::BindGroup,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    uniform: wgpu::Buffer,
    stride: usize,
    targets: [Target; 4], // 0,1 layer ping-pong; 2,3 scene ping-pong
    static_cache: Option<StaticLayerCache>,
    prefix_cache: Option<StaticPrefixCache>,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    geometry_uniform: wgpu::Buffer,
    composite: wgpu::RenderPipeline,
    present: wgpu::RenderPipeline,
    layers: Vec<LayerPipeline>,
    global: Vec<EffectPipeline>,
    background: wgpu::Color,
    uniform_bytes: Vec<u8>,
    profiler: Option<FrameProfiler>,
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
            let targets = std::array::from_fn(|_| {
                Target::new(gpu, &layout, &sampler, &uniform, width, height)
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
                INTERNAL,
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
                INTERNAL,
                Some(CUTOUT_BLEND),
                &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x4,1=>Float32x4,2=>Float32x4],
                }],
            );
            let composite = effect_pipeline(
                d,
                &layout,
                "composite",
                include_str!("../shaders/composite.wgsl"),
                INTERNAL,
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
                        effects: compile_effects(d, &layout, &l.effects, base)?,
                        clear: clear_color(&l.background)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let global = compile_effects(d, &layout, &scene.effects, base)?;
            let background = clear_color(&scene.background)?;
            let instance_capacity = (scene.shape_count() * 3).max(1);
            let instances = d.create_buffer(&wgpu::BufferDescriptor {
                label: Some("shape instances"),
                size: (instance_capacity * std::mem::size_of::<Instance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let static_cache = StaticLayerCache::new(gpu, &scene, &layout, &sampler, &uniform, width, height);
            let prefix_cache = StaticPrefixCache::new(
                gpu, &scene, &layout, &sampler, &uniform, width, height, static_cache.is_some(),
            );
            Ok(Self {
                scene,
                width,
                height,
                output_format,
                geometry,
                geometry_cutout,
                geo_bind,
                instances,
                instance_capacity,
                uniform,
                stride,
                targets,
                static_cache,
                prefix_cache,
                layout,
                sampler,
                geometry_uniform,
                composite,
                present,
                layers,
                global,
                background,
                uniform_bytes: Vec::with_capacity(stride * MAX_PASSES),
                profiler: None,
            })
        })();
        let validation = pollster::block_on(d.pop_error_scope());
        if let Some(e) = validation {
            return Err(anyhow!("GPU pipeline validation: {e}"));
        }
        result
    }
    pub fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) -> Result<()> {
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        check_size(gpu, width, height)?;
        self.targets = std::array::from_fn(|_| {
            Target::new(
                gpu,
                &self.layout,
                &self.sampler,
                &self.uniform,
                width,
                height,
            )
        });
        // Release old caches before allocating their replacements.
        self.static_cache = None;
        self.prefix_cache = None;
        self.static_cache = StaticLayerCache::new(
            gpu, &self.scene, &self.layout, &self.sampler, &self.uniform, width, height,
        );
        self.prefix_cache = StaticPrefixCache::new(
            gpu, &self.scene, &self.layout, &self.sampler, &self.uniform, width, height,
            self.static_cache.is_some(),
        );
        gpu.queue.write_buffer(
            &self.geometry_uniform,
            0,
            bytemuck::cast_slice(&[width as f32, height as f32, 0., 0.]),
        );
        self.width = width;
        self.height = height;
        // Do not mix timestamps from different render resolutions in a report.
        if let Some(old) = self.profiler.take() {
            self.profiler = Some(FrameProfiler::new(gpu, old.label.clone()));
        }
        Ok(())
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
    ) {
        if let Some(profiler) = &mut self.profiler {
            profiler.prepare(gpu);
        }
        let t = self.scene.time(t);
        let layer_instances = self.scene.instances(t);
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
        };
        let mut passes = Passes {
            encoder,
            bytes: &mut self.uniform_bytes,
            stride: self.stride,
            common,
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
                }
                let current = if let Some(cache) = prefix {
                    if !cache.valid {
                        let input = passes.chain(
                            &self.targets[0],
                            &self.targets[0],
                            &self.targets[1],
                            &layer.effects[..cache.effects],
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
                        &layer.effects[cache.effects..],
                        t,
                        self.scene.loop_seconds,
                    )
                } else {
                    passes.chain(
                        &self.targets[0],
                        &self.targets[0],
                        &self.targets[1],
                        &layer.effects,
                        t,
                        self.scene.loop_seconds,
                    )
                };
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
            let (strength, phase) = scene::effect_values(&effect.config, t, duration);

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
fn compile_effects(
    d: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    effects: &[Effect],
    base: &Path,
) -> Result<Vec<EffectPipeline>> {
    effects
        .iter()
        .map(|e| {
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
                    Cow::Borrowed(include_str!("../shaders/glitch.wgsl")),
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
            Ok(EffectPipeline {
                config: e.clone(),
                passes: bodies
                    .iter()
                    .map(|(label, body)| effect_pipeline(d, layout, label, body, INTERNAL, None))
                    .collect(),
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
        assert!(cache_fits(1440, 2560, 2));
        assert!(cache_fits(3840, 2160, 1));
        assert!(!cache_fits(3840, 2160, 2));
    }

}
