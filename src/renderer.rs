use crate::scene::{self, Effect, EffectKind, Instance, Scene};
use anyhow::{anyhow, bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use std::{borrow::Cow, fs, path::Path};
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
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("floatpaper"),
                required_features: wgpu::Features::empty(),
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
    _texture: wgpu::Texture,
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
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
            _texture: texture,
            view,
            bind,
        }
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
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    geometry_uniform: wgpu::Buffer,
    composite: wgpu::RenderPipeline,
    present: wgpu::RenderPipeline,
    layers: Vec<LayerPipeline>,
    global: Vec<EffectPipeline>,
    background: wgpu::Color,
    uniform_bytes: Vec<u8>,
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
                layout,
                sampler,
                geometry_uniform,
                composite,
                present,
                layers,
                global,
                background,
                uniform_bytes: Vec::with_capacity(stride * MAX_PASSES),
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
        gpu.queue.write_buffer(
            &self.geometry_uniform,
            0,
            bytemuck::cast_slice(&[width as f32, height as f32, 0., 0.]),
        );
        self.width = width;
        self.height = height;
        Ok(())
    }
    pub fn render(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        t: f64,
    ) {
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
        };
        clear(passes.encoder, &self.targets[2].view, self.background);
        for (i, layer) in self.layers.iter().enumerate() {
            {
                let mut pass = begin(
                    passes.encoder,
                    &self.targets[0].view,
                    wgpu::LoadOp::Clear(layer.clear),
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
            let current = passes.chain(
                &self.targets,
                0,
                1,
                &layer.effects,
                t,
                self.scene.loop_seconds,
            );
            passes.draw(
                &self.composite,
                &self.targets[current],
                &self.targets[2].view,
                wgpu::LoadOp::Load,
                self.scene.layers[i].opacity,
                0.0,
                [0.0; 2],
                0.0,
                48.0,
                [0.0, 0.0, 0.0, 1.0],
            );
        }
        let current = passes.chain(
            &self.targets,
            2,
            3,
            &self.global,
            t,
            self.scene.loop_seconds,
        );
        passes.draw(
            &self.present,
            &self.targets[current],
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
        gpu.queue
            .write_buffer(&self.uniform, 0, &self.uniform_bytes);
    }
}
struct Passes<'a> {
    encoder: &'a mut wgpu::CommandEncoder,
    bytes: &'a mut Vec<u8>,
    stride: usize,
    common: Uniform,
}

impl Passes<'_> {
    fn draw(
        &mut self,
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

        let mut pass = begin(self.encoder, output, load);

        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &input.bind, &[offset as u32]);
        pass.draw(0..3, 0..1);
    }

    fn chain(
        &mut self,
        targets: &[Target; 4],
        a: usize,
        b: usize,
        effects: &[EffectPipeline],
        t: f64,
        duration: Option<f64>,
    ) -> usize {
        let mut current = a;

        for effect in effects {
            let (strength, phase) = scene::effect_values(&effect.config, t, duration);

            let effect_color =
                scene::parse_color(&effect.config.color).expect("validated effect color");

            let rotation = effect.config.rotation.unwrap_or(0.0);

            let rotations = [rotation, rotation + 90.0];

            let pass_count =
                if effect.config.kind == EffectKind::Blur && effect.config.rotation.is_none() {
                    2
                } else {
                    1
                };

            for pipeline in &effect.passes {
                for &angle in &rotations[..pass_count] {
                    let next = if current == a { b } else { a };

                    self.draw(
                        pipeline,
                        &targets[current],
                        &targets[next].view,
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

fn begin<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    view: &'a wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("floatpaper pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}
fn clear(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, color: wgpu::Color) {
    drop(begin(encoder, view, wgpu::LoadOp::Clear(color)));
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
                    Cow::Borrowed(include_str!("../shaders/noise.wgsl")),
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
