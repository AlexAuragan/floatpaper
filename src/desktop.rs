//! Minimal native Wayland backend. The renderer never sees Wayland objects.
use crate::{renderer::{Gpu,Renderer},scene::Scene};
use anyhow::{bail, Context, Result};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use notify::{EventKind, RecursiveMode, Watcher};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use std::{collections::{BTreeMap,HashMap}, path::{Path,PathBuf}, ptr::NonNull, sync::{Arc,atomic::{AtomicBool,Ordering},mpsc}, time::{Duration,Instant}};
use wayland_client::{Connection,Dispatch,Proxy,QueueHandle,delegate_noop,protocol::{wl_callback,wl_compositor,wl_output,wl_region,wl_registry,wl_surface}};
use wayland_protocols::xdg::shell::client::{xdg_surface,xdg_toplevel,xdg_wm_base};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1,zwlr_layer_surface_v1};

pub enum Mode { Preview {width:u32,height:u32}, Live {output:Option<String>} }
#[derive(Default)]
struct Output { proxy:Option<wl_output::WlOutput>, name:Option<String>, scale:i32 }
struct Desktop {
    compositor:Option<wl_compositor::WlCompositor>,
    layer_shell:Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    wm_base:Option<xdg_wm_base::XdgWmBase>,
    outputs:BTreeMap<u32,Output>,
    surfaces:BTreeMap<u32,Screen>,
    removed:Vec<u32>,
    quit:bool,
}
impl Default for Desktop { fn default()->Self { Self { compositor:None,layer_shell:None,wm_base:None,outputs:BTreeMap::new(),surfaces:BTreeMap::new(),removed:Vec::new(),quit:false } } }
const PREVIEW:u32=0;
struct Screen {
    // Drop wgpu surface before releasing native wl_surface (raw handles are borrowed).
    render:Option<RenderSurface>,
    role:Role,
    surface:wl_surface::WlSurface,
    logical:(u32,u32),
    scale:i32,
    pending:bool,
    configured:bool,
    ready:bool,
    closed:bool,
    last_frame:Option<Instant>,
}
enum Role { Layer(zwlr_layer_surface_v1::ZwlrLayerSurfaceV1), Window(xdg_surface::XdgSurface,xdg_toplevel::XdgToplevel) }
impl Drop for Screen {
    fn drop(&mut self) {
        self.render.take();
        match &self.role { Role::Layer(l)=>l.destroy(),Role::Window(s,t)=>{t.destroy();s.destroy();} }
        self.surface.destroy();
    }
}
struct RenderSurface { surface:wgpu::Surface<'static>, config:wgpu::SurfaceConfiguration }

#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]
struct RenderKey {
    width:u32,
    height:u32,
    format:wgpu::TextureFormat,
    // Isolate outputs when sharing is disabled (including preview).
    isolated:Option<u32>,
}
impl RenderKey {
    fn new(config:&wgpu::SurfaceConfiguration,id:u32,share:bool)->Self {
        Self {width:config.width,height:config.height,format:config.format,
            isolated:if share {None} else {Some(id)}}
    }
    fn label(self)->String {
        match self.isolated {
            Some(id)=>format!("surface={id}"),
            None=>format!("group={}x{}:{:?}",self.width,self.height,self.format),
        }
    }
}

struct SharedFrame {
    texture:wgpu::Texture,
    view:wgpu::TextureView,
    bind:wgpu::BindGroup,
    blit:wgpu::RenderPipeline,
    rendered_at:Option<Instant>,
}
impl SharedFrame {
    fn new(gpu:&Gpu,key:RenderKey)->Result<Self> {
        let d=&gpu.device;
        d.push_error_scope(wgpu::ErrorFilter::Validation);
        let texture=d.create_texture(&wgpu::TextureDescriptor {
            label:Some("shared output frame"),size:crate::renderer::extent(key.width,key.height),
            mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format:key.format,
            usage:wgpu::TextureUsages::RENDER_ATTACHMENT|wgpu::TextureUsages::COPY_SRC|wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats:&[],
        });
        let view=texture.create_view(&Default::default());
        let layout=d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label:Some("shared frame blit layout"),
            entries:&[wgpu::BindGroupLayoutEntry {
                binding:0,visibility:wgpu::ShaderStages::FRAGMENT,
                ty:wgpu::BindingType::Texture {sample_type:wgpu::TextureSampleType::Float{filterable:false},
                    view_dimension:wgpu::TextureViewDimension::D2,multisampled:false},count:None,
            }],
        });
        let bind=d.create_bind_group(&wgpu::BindGroupDescriptor {
            label:Some("shared frame blit input"),layout:&layout,
            entries:&[wgpu::BindGroupEntry{binding:0,resource:wgpu::BindingResource::TextureView(&view)}],
        });
        let shader=d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label:Some("shared frame blit"),
            source:wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@vertex fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>,3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0));
    return vec4(p[i],0.0,1.0);
}
@fragment fn fs_main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(source,vec2<i32>(p.xy),0);
}
"#)),
        });
        let pipeline_layout=d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label:Some("shared frame blit pipeline layout"),bind_group_layouts:&[&layout],push_constant_ranges:&[],
        });
        let blit=d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label:Some("shared frame blit"),layout:Some(&pipeline_layout),
            vertex:wgpu::VertexState{module:&shader,entry_point:"vs_main",buffers:&[]},
            primitive:wgpu::PrimitiveState::default(),depth_stencil:None,
            multisample:wgpu::MultisampleState::default(),
            fragment:Some(wgpu::FragmentState{module:&shader,entry_point:"fs_main",
                targets:&[Some(wgpu::ColorTargetState{format:key.format,blend:None,write_mask:wgpu::ColorWrites::ALL})]}),
            multiview:None,
        });
        if let Some(error)=pollster::block_on(d.pop_error_scope()) {
            bail!("Shared frame GPU validation: {error}");
        }
        Ok(Self{texture,view,bind,blit,rendered_at:None})
    }
    fn present(&self,encoder:&mut wgpu::CommandEncoder,frame:&wgpu::Texture,config:&wgpu::SurfaceConfiguration) {
        if config.usage.contains(wgpu::TextureUsages::COPY_DST) {
            encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture{texture:&self.texture,mip_level:0,origin:wgpu::Origin3d::ZERO,aspect:wgpu::TextureAspect::All},
                wgpu::ImageCopyTexture{texture:frame,mip_level:0,origin:wgpu::Origin3d::ZERO,aspect:wgpu::TextureAspect::All},
                crate::renderer::extent(config.width,config.height),
            );
        } else {
            // Same-format unfiltered texel fetch; sRGB decode/encode is paired.
            let view=frame.create_view(&Default::default());
            let mut pass=encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label:Some("shared frame blit"),color_attachments:&[Some(wgpu::RenderPassColorAttachment {
                    view:&view,resolve_target:None,ops:wgpu::Operations{load:wgpu::LoadOp::Clear(wgpu::Color::BLACK),store:wgpu::StoreOp::Store},
                })],depth_stencil_attachment:None,timestamp_writes:None,occlusion_query_set:None,
            });
            pass.set_pipeline(&self.blit);
            pass.set_bind_group(0,&self.bind,&[]);
            pass.draw(0..3,0..1);
        }
    }
}

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum BenchmarkMode { Off, Render, Replay, Hold }

impl BenchmarkMode {
    fn parse(value:&str)->Result<Self> {
        match value {
            "render"=>Ok(Self::Render),
            "replay"=>Ok(Self::Replay),
            "hold"=>Ok(Self::Hold),
            _=>bail!("FLOATPAPER_BENCHMARK_MODE must be render, replay or hold"),
        }
    }
    fn from_env()->Result<Self> {
        match std::env::var("FLOATPAPER_BENCHMARK_MODE") {
            Ok(value)=>Self::parse(&value),
            Err(std::env::VarError::NotPresent)=>Ok(Self::Off),
            Err(e)=>Err(e).context("Reading FLOATPAPER_BENCHMARK_MODE"),
        }
    }
    fn active(self)->bool { self!=Self::Off }
    fn frozen(self)->bool { matches!(self,Self::Replay|Self::Hold) }
    fn should_present(self,last:Option<Instant>)->bool { self!=Self::Hold || last.is_none() }
    fn needs_render(self,last:Option<Instant>,now:Instant,interval:Duration)->bool {
        last.map_or(true,|last| !self.frozen() && now.saturating_duration_since(last)>=interval)
    }
}

struct BenchmarkStats {
    start:Instant,
    renders:u64,
    presents:BTreeMap<u32,u64>,
}
impl BenchmarkStats {
    fn new()->Self {Self{start:Instant::now(),renders:0,presents:BTreeMap::new()}}
    fn record(&mut self,id:u32,rendered:bool) {
        self.renders+=u64::from(rendered);
        *self.presents.entry(id).or_default()+=1;
    }
    fn report(&mut self,mode:BenchmarkMode,state:&Desktop) {
        let elapsed=self.start.elapsed().as_secs_f64();
        if elapsed<5.0 {return;}
        let detail=state.surfaces.iter().filter(|(_,s)|s.configured && !s.closed)
            .map(|(id,_)| format!("surface={id}:{:.2}",self.presents.get(id).copied().unwrap_or(0) as f64/elapsed))
            .collect::<Vec<_>>().join(" ");
        eprintln!("BENCHMARK mode={mode:?} scene_renders_per_sec={:.2} surface_submit_fps | {detail}",self.renders as f64/elapsed);
        self.start=Instant::now();
        self.renders=0;
        self.presents.clear();
    }
}

struct RenderGroup { renderer:Renderer, shared:Option<SharedFrame> }
type RenderPool=HashMap<RenderKey,RenderGroup>;

fn new_group(gpu:&Gpu,scene:&Scene,base:&Path,key:RenderKey,shared:bool)->Result<RenderGroup> {
    let mut renderer=Renderer::new(gpu,scene.clone(),base,key.width,key.height,key.format)?;
    renderer.enable_profile(gpu,key.label());
    let shared=if shared {Some(SharedFrame::new(gpu,key)?)} else {None};
    Ok(RenderGroup{renderer,shared})
}

fn reconcile_renderers(gpu:&Gpu,state:&Desktop,pool:&mut RenderPool,scene:&Scene,base:&Path,share:bool,benchmark:BenchmarkMode)->Result<()> {
    let mut counts=HashMap::<RenderKey,usize>::new();
    for (&id,screen) in &state.surfaces {
        if screen.closed {continue;}
        if let Some(r)=&screen.render {
            *counts.entry(RenderKey::new(&r.config,id,share)).or_default()+=1;
        }
    }
    pool.retain(|key,_|counts.contains_key(key));
    for (key,count) in counts {
        let shared=count>1 || benchmark.active();
        if let Some(group)=pool.get_mut(&key) {
            if shared && group.shared.is_none() {
                group.shared=Some(SharedFrame::new(gpu,key)?);
                eprintln!("Shared rendering: {} ({count} outputs)",key.label());
            } else if !shared {
                group.shared=None;
            }
        } else {
            let group=new_group(gpu,scene,base,key,shared)?;
            if shared {eprintln!("Shared rendering: {} ({count} outputs)",key.label());}
            pool.insert(key,group);
        }
    }
    Ok(())
}

pub fn run(path:&Path,mode:Mode)->Result<()> {
    let path=path.canonicalize().context("Scene path does not exist")?;
    let base=path.parent().context("Missing scene directory")?;
    let mut scene=Scene::load(&path)?;
    let connection=Connection::connect_to_env().context("Connect to Wayland (run inside Hyprland)")?;
    let display=connection.display();
    let mut queue=connection.new_event_queue::<Desktop>();
    let qh=queue.handle();
    let _registry=display.get_registry(&qh,());
    let mut state=Desktop::default();
    queue.roundtrip(&mut state).context("Discover Wayland globals")?;
    queue.roundtrip(&mut state).context("Read output properties")?;
    if state.compositor.is_none() { bail!("No wl_compositor"); }
    match &mode {
        Mode::Preview{width,height}=>{
            if *width==0 || *height==0 { bail!("Preview dimensions must be positive"); }
            let compositor=state.compositor.as_ref().unwrap();
            let wm=state.wm_base.as_ref().context("Compositor does not support xdg-shell")?;
            let surface=compositor.create_surface(&qh,PREVIEW);
            let xdg=wm.get_xdg_surface(&surface,&qh,PREVIEW);
            let top=xdg.get_toplevel(&qh,PREVIEW);
            top.set_title("Floatpaper preview".into());
            top.set_app_id("floatpaper.preview".into());
            surface.commit();
            state.surfaces.insert(PREVIEW,Screen { render:None,role:Role::Window(xdg,top),surface,logical:(*width,*height),scale:1,pending:true,configured:false,ready:true,closed:false,last_frame:None });
        }
        Mode::Live{output}=>{
            if state.layer_shell.is_none() { bail!("Compositor does not support wlr-layer-shell"); }
            reconcile_outputs(&mut state,&qh,output.as_deref());
            if state.surfaces.is_empty() { bail!("No matching Wayland output. Available: {:?}",state.outputs.values().filter_map(|o|o.name.as_deref()).collect::<Vec<_>>()); }
        }
    }
    queue.roundtrip(&mut state).context("Configure Wayland surfaces")?;
    // Create the first surface before requesting an adapter, so the adapter supports presentation.
    let instance=Gpu::instance();
    let first=state.surfaces.values().find(|s|!s.closed).context("All surfaces closed before GPU initialization")?;
    let first_surface=make_surface(&instance,&connection,&first.surface)?;
    let gpu=Gpu::new(&instance,Some(&first_surface))?;
    drop(first_surface);
    let mut event_loop=EventLoop::<Desktop>::try_new().context("Create Wayland event loop")?;
    WaylandSource::new(connection.clone(),queue).insert(event_loop.handle()).map_err(|e|anyhow::anyhow!("Register Wayland source: {e}"))?;
    let (rx,_watcher)=watch(&path,&scene)?;
    let running=Arc::new(AtomicBool::new(true));
    let r=running.clone();
    ctrlc::set_handler(move||r.store(false,Ordering::SeqCst)).context("Install Ctrl-C handler")?;
    let share=matches!(&mode,Mode::Live{..}) && std::env::var_os("FLOATPAPER_DISABLE_OUTPUT_SHARING").is_none();
    let benchmark=BenchmarkMode::from_env()?;
    if benchmark.active() && !matches!(&mode,Mode::Live{..}) {
        bail!("Presentation benchmarking is supported only in live mode");
    }
    if benchmark.active() {
        eprintln!("Presentation benchmark: {benchmark:?}; all render groups use offscreen output");
    }
    let mut stats=BenchmarkStats::new();
    let mut pool=RenderPool::new();
    let start=Instant::now();
    let mut reload_at=None;
    let mut last_watch_error=None;
    while running.load(Ordering::SeqCst) && !state.quit {
        if let Mode::Live{output}=&mode {
            for id in std::mem::take(&mut state.removed) { state.surfaces.remove(&id); }
            reconcile_outputs(&mut state,&qh,output.as_deref());
        }
        // A closed layer is not automatically recreated until its output is removed/re-added.
        if matches!(&mode,Mode::Preview{..}) && state.surfaces.get(&PREVIEW).is_some_and(|s|s.closed) { break; }
        for event in rx.try_iter() {
            match event {
                Ok(e) if matches!(e.kind,EventKind::Modify(_)|EventKind::Create(_)|EventKind::Remove(_)|EventKind::Any) => { reload_at=Some(Instant::now()+Duration::from_millis(180)); },
                Err(e)=>{ let msg=e.to_string(); if last_watch_error.as_ref()!=Some(&msg) { eprintln!("Watcher: {msg}"); last_watch_error=Some(msg); } },
                _=>{},
            }
        }
        if reload_at.is_some_and(|t|Instant::now()>=t) {
            reload_at=None;
            match reload(&gpu,&mut pool,&path) {
                Ok(new_scene)=>{
                    scene=new_scene;
                    if benchmark.frozen() {
                        for screen in state.surfaces.values_mut() {screen.last_frame=None;}
                    }
                    eprintln!("Reloaded {}",path.display());
                },
                Err(e)=>eprintln!("Reload rejected; retaining last valid scene: {e:#}"),
            }
        }
        let interval=Duration::from_secs_f64(1.0/scene.fps as f64);
        let now=Instant::now();
        let mut timeout=Duration::from_millis(250); // bounds Ctrl-C/hot reload latency without spinning
        if let Some(t)=reload_at { timeout=timeout.min(t.saturating_duration_since(now)); }
        for screen in state.surfaces.values_mut() {
            if screen.closed { screen.render.take(); continue; }
            if !screen.configured { continue; }
            if screen.pending || screen.render.is_none() {
                configure(&instance,&connection,&gpu,screen,share || benchmark.active())?;
                screen.pending=false;
                if benchmark==BenchmarkMode::Hold {screen.last_frame=None;}
            }
        }
        reconcile_renderers(&gpu,&state,&mut pool,&scene,base,share,benchmark)?;
        for (&id,screen) in &mut state.surfaces {
            if screen.closed || !screen.configured || !screen.ready || !benchmark.should_present(screen.last_frame) { continue; }
            let r=screen.render.as_ref().context("Surface not configured")?;
            let key=RenderKey::new(&r.config,id,share);
            let group=pool.get_mut(&key).context("Missing render group")?;
            if let Some(last)=screen.last_frame {
                let remaining=interval.saturating_sub(now.saturating_duration_since(last));
                if !remaining.is_zero() { timeout=timeout.min(remaining); continue; }
            }
            let frame_time=if benchmark.frozen() {0.0} else {now.duration_since(start).as_secs_f64()};
            match draw(&gpu,screen,group,&qh,frame_time,now,interval,benchmark)? {
                Some(rendered)=>{
                    screen.last_frame=Some(now);
                    if benchmark.active() {stats.record(id,rendered);}
                },
                None=>{timeout=timeout.min(interval);}
            }
        }
        if benchmark.active() {stats.report(benchmark,&state);}
        event_loop.dispatch(timeout,&mut state).context("Dispatch Wayland events")?;
    }
    state.surfaces.clear();
    connection.flush().context("Flush Wayland cleanup")?;
    Ok(())
}

fn reconcile_outputs(state:&mut Desktop,qh:&QueueHandle<Desktop>,selected:Option<&str>) {
    let compositor=state.compositor.as_ref().unwrap();
    let shell=state.layer_shell.as_ref().unwrap();
    for (&id,output) in &state.outputs {
        if state.surfaces.contains_key(&id) || selected.is_some_and(|n|output.name.as_deref()!=Some(n)) { continue; }
        let Some(proxy)=&output.proxy else {continue;};
        let surface=compositor.create_surface(qh,id);
        // Empty input region lets mouse events pass through to the desktop.
        let region=compositor.create_region(qh,());
        surface.set_input_region(Some(&region)); region.destroy();
        let layer=shell.get_layer_surface(&surface,Some(proxy),zwlr_layer_shell_v1::Layer::Background,"floatpaper".into(),qh,id);
        use zwlr_layer_surface_v1::Anchor;
        layer.set_anchor(Anchor::Top|Anchor::Bottom|Anchor::Left|Anchor::Right);
        layer.set_size(0,0);
        layer.set_exclusive_zone(-1); // cover full output without reserving any work area
        layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        let scale=output.scale.max(1);
        if surface.version()>=3 { surface.set_buffer_scale(scale); }
        surface.commit();
        eprintln!("Attached output {} (surface {})",output.name.as_deref().unwrap_or("unnamed"),id);
        state.surfaces.insert(id,Screen {render:None,role:Role::Layer(layer),surface,logical:(1,1),scale,pending:true,configured:false,ready:true,closed:false,last_frame:None});
    }
}
fn make_surface(instance:&wgpu::Instance,connection:&Connection,surface:&wl_surface::WlSurface)->Result<wgpu::Surface<'static>> {
    let display_ptr=NonNull::new(connection.backend().display_ptr().cast()).context("Null Wayland display")?;
    let surface_ptr=NonNull::new(surface.id().as_ptr().cast()).context("Null Wayland surface")?;
    // SAFETY: the caller keeps the connection and wl_surface alive until this wgpu surface is dropped.
    unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle { raw_display_handle:RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display_ptr)),raw_window_handle:RawWindowHandle::Wayland(WaylandWindowHandle::new(surface_ptr)) }) }.context("Create wgpu Wayland surface")
}
fn configure(instance:&wgpu::Instance,connection:&Connection,gpu:&Gpu,screen:&mut Screen,share:bool)->Result<()> {
    let width=screen.logical.0.checked_mul(screen.scale as u32).context("Output width overflow")?;
    let height=screen.logical.1.checked_mul(screen.scale as u32).context("Output height overflow")?;
    crate::renderer::check_size(gpu,width,height)?;
    if let Some(r)=&mut screen.render {
        r.config.width=width; r.config.height=height;
        r.surface.configure(&gpu.device,&r.config);
    } else {
        let surface=make_surface(instance,connection,&screen.surface)?;
        let caps=surface.get_capabilities(&gpu.adapter);
        let format=caps.formats.iter().copied().find(|f|f.is_srgb()).or_else(||caps.formats.first().copied()).context("GPU cannot present to this output")?;
        let alpha_mode=if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) { wgpu::CompositeAlphaMode::Opaque } else { *caps.alpha_modes.first().context("No supported alpha modes")? };
        let mut usage=wgpu::TextureUsages::RENDER_ATTACHMENT;
        // A/B test the render-pass transfer against the swapchain copy path.
        // Omit COPY_DST when forced so the swapchain is allocated for rendering only.
        let force_blit=std::env::var_os("FLOATPAPER_FORCE_PRESENT_BLIT").is_some();
        if share && !force_blit && caps.usages.contains(wgpu::TextureUsages::COPY_DST) {
            usage|=wgpu::TextureUsages::COPY_DST;
        }
        let config=wgpu::SurfaceConfiguration { usage,format,width,height,present_mode:wgpu::PresentMode::Fifo,desired_maximum_frame_latency:2,alpha_mode,view_formats:vec![] };
        surface.configure(&gpu.device,&config);
        screen.render=Some(RenderSurface{surface,config});
        eprintln!("Surface {width}x{height} ({format:?}; shared frame transfer: {})",
            if !share {"disabled"} else if usage.contains(wgpu::TextureUsages::COPY_DST) {"copy"} else {"blit"});
    }
    Ok(())
}
fn draw(gpu:&Gpu,screen:&mut Screen,group:&mut RenderGroup,qh:&QueueHandle<Desktop>,t:f64,now:Instant,interval:Duration,benchmark:BenchmarkMode)->Result<Option<bool>> {
    let r=screen.render.as_mut().context("Surface not configured")?;
    let frame=match r.surface.get_current_texture() {
        Ok(f)=>f,
        Err(wgpu::SurfaceError::Lost|wgpu::SurfaceError::Outdated)=>{ screen.pending=true; return Ok(None); },
        Err(wgpu::SurfaceError::Timeout)=>{eprintln!("Wayland surface acquisition timed out");return Ok(None);},
        Err(wgpu::SurfaceError::OutOfMemory)=>bail!("GPU surface out of memory"),
    };
    let mut encoder=gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor{label:Some("live frame")});
    let rendered=if let Some(shared)=&mut group.shared {
        // A ready output can reuse the most recent group frame for at most
        // one requested frame interval. Never wait for another output.
        let refresh=benchmark.needs_render(shared.rendered_at,now,interval);
        if refresh {
            group.renderer.render(gpu,&mut encoder,&shared.view,t);
            shared.rendered_at=Some(now);
        }
        shared.present(&mut encoder,&frame.texture,&r.config);
        refresh
    } else {
        let view=frame.texture.create_view(&Default::default());
        group.renderer.render(gpu,&mut encoder,&view,t);
        true
    };
    gpu.queue.submit(Some(encoder.finish()));
    // Request before presentation, so the callback is associated with the next commit.
    // The callback's user data matches the wl_surface's registry id.
    let id=*screen.surface.data::<u32>().context("Missing surface id")?;
    screen.surface.frame(qh,id);
    screen.ready=false;
    frame.present();
    if rendered {group.renderer.profile_submitted();}
    Ok(Some(rendered))
}
fn reload(gpu:&Gpu,pool:&mut RenderPool,path:&Path)->Result<Scene> {
    let scene=Scene::load(path)?;
    let base=path.parent().unwrap_or(Path::new("."));
    let mut replacements=RenderPool::new();
    for (&key,group) in pool.iter() {
        replacements.insert(key,new_group(gpu,&scene,base,key,group.shared.is_some())?);
    }
    *pool=replacements;
    Ok(scene)
}
fn watch(path:&Path,scene:&Scene)->Result<(mpsc::Receiver<notify::Result<notify::Event>>,notify::RecommendedWatcher)> {
    let (tx,rx)=mpsc::channel();
    let mut watcher=notify::recommended_watcher(move|event| { let _=tx.send(event); }).context("Create scene watcher")?;
    let base=path.parent().context("Missing scene directory")?;
    watcher.watch(base,RecursiveMode::Recursive).context("Watch scene directory")?;
    // Watch external custom shader parent directories too. Newly added external paths need restart.
    let mut dirs=std::collections::BTreeSet::<PathBuf>::new();
    for effect in scene.effects.iter().chain(scene.layers.iter().flat_map(|l|l.effects.iter())) {
        if let Some(p)=&effect.shader { let full=base.join(p).canonicalize()?; if let Some(dir)=full.parent() { if !dir.starts_with(base) { dirs.insert(dir.to_path_buf()); } } }
    }
    for dir in dirs { watcher.watch(&dir,RecursiveMode::NonRecursive)?; }
    Ok((rx,watcher))
}

impl Dispatch<wl_registry::WlRegistry,()> for Desktop {
    fn event(state:&mut Self,registry:&wl_registry::WlRegistry,event:wl_registry::Event,_:&(),_:&Connection,qh:&QueueHandle<Self>) {
        match event {
            wl_registry::Event::Global{name,interface,version}=>match interface.as_str() {
                "wl_compositor"=>state.compositor=Some(registry.bind(name,version.min(4),qh,())),
                "zwlr_layer_shell_v1"=>state.layer_shell=Some(registry.bind(name,version.min(4),qh,())),
                "xdg_wm_base"=>state.wm_base=Some(registry.bind(name,version.min(3),qh,())),
                "wl_output"=>{ let proxy=registry.bind(name,version.min(4),qh,name); state.outputs.insert(name,Output{proxy:Some(proxy),name:None,scale:1}); },
                _=>{},
            },
            wl_registry::Event::GlobalRemove{name}=>{ if state.outputs.remove(&name).is_some() { state.removed.push(name); } },
            _=>{},
        }
    }
}
impl Dispatch<wl_output::WlOutput,u32> for Desktop {
    fn event(state:&mut Self,_:&wl_output::WlOutput,event:wl_output::Event,id:&u32,_:&Connection,_:&QueueHandle<Self>) {
        let Some(output)=state.outputs.get_mut(id) else {return;};
        match event {
            wl_output::Event::Name{name}=>output.name=Some(name),
            wl_output::Event::Scale{factor}=>{
                output.scale=factor.max(1);
                if let Some(screen)=state.surfaces.get_mut(id) { if screen.scale!=output.scale {screen.scale=output.scale; if screen.surface.version()>=3 {screen.surface.set_buffer_scale(screen.scale);} screen.pending=true;} }
            },
            _=>{},
        }
    }
}
impl Dispatch<wl_surface::WlSurface,u32> for Desktop {
    fn event(state:&mut Self,_:&wl_surface::WlSurface,event:wl_surface::Event,id:&u32,_:&Connection,_:&QueueHandle<Self>) {
        // Preview uses the entered output's integer scale. Fractional scale is not implemented.
        if *id==PREVIEW {
            if let wl_surface::Event::Enter{output}=event {
                if let Some(output_id)=output.data::<u32>() {
                    if let (Some(o),Some(s))=(state.outputs.get(output_id),state.surfaces.get_mut(id)) {
                        s.scale=o.scale.max(1); if s.surface.version()>=3 {s.surface.set_buffer_scale(s.scale);} s.pending=true;
                    }
                }
            }
        }
    }
}
impl Dispatch<wl_callback::WlCallback,u32> for Desktop {
    fn event(state:&mut Self,_:&wl_callback::WlCallback,event:wl_callback::Event,id:&u32,_:&Connection,_:&QueueHandle<Self>) {
        if let wl_callback::Event::Done{..}=event { if let Some(s)=state.surfaces.get_mut(id) {s.ready=true;} }
    }
}
impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,u32> for Desktop {
    fn event(state:&mut Self,layer:&zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,event:zwlr_layer_surface_v1::Event,id:&u32,_:&Connection,_:&QueueHandle<Self>) {
        if let Some(s)=state.surfaces.get_mut(id) {
            match event {
                zwlr_layer_surface_v1::Event::Configure{serial,width,height}=>{layer.ack_configure(serial);if width>0 && height>0 {s.logical=(width,height);s.configured=true;s.pending=true;}else{eprintln!("Output configure returned zero dimensions");}},
                zwlr_layer_surface_v1::Event::Closed=>s.closed=true,
                _=>{},
            }
        }
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase,()> for Desktop {
    fn event(_:&mut Self,wm:&xdg_wm_base::XdgWmBase,event:xdg_wm_base::Event,_:&(),_:&Connection,_:&QueueHandle<Self>) { if let xdg_wm_base::Event::Ping{serial}=event {wm.pong(serial);} }
}
impl Dispatch<xdg_surface::XdgSurface,u32> for Desktop {
    fn event(state:&mut Self,xdg:&xdg_surface::XdgSurface,event:xdg_surface::Event,id:&u32,_:&Connection,_:&QueueHandle<Self>) { if let xdg_surface::Event::Configure{serial}=event {xdg.ack_configure(serial); if let Some(s)=state.surfaces.get_mut(id) {s.configured=true;s.pending=true;} } }
}
impl Dispatch<xdg_toplevel::XdgToplevel,u32> for Desktop {
    fn event(state:&mut Self,_:&xdg_toplevel::XdgToplevel,event:xdg_toplevel::Event,id:&u32,_:&Connection,_:&QueueHandle<Self>) {
        if let Some(s)=state.surfaces.get_mut(id) {
            match event { xdg_toplevel::Event::Configure{width,height,..}=>{if width>0 && height>0 {s.logical=(width as u32,height as u32);s.pending=true;}},xdg_toplevel::Event::Close=>s.closed=true,_=>{} }
        }
    }
}
delegate_noop!(Desktop: ignore wl_compositor::WlCompositor);
delegate_noop!(Desktop: ignore wl_region::WlRegion);
delegate_noop!(Desktop: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);

#[cfg(test)]
mod sharing_tests {
    use super::*;
    fn config()->wgpu::SurfaceConfiguration {
        wgpu::SurfaceConfiguration{usage:wgpu::TextureUsages::RENDER_ATTACHMENT,
            format:wgpu::TextureFormat::Bgra8UnormSrgb,width:1920,height:1080,
            present_mode:wgpu::PresentMode::Fifo,desired_maximum_frame_latency:2,
            alpha_mode:wgpu::CompositeAlphaMode::Opaque,view_formats:vec![]}
    }
    #[test]
    fn matching_dimensions_and_format_share_but_other_outputs_do_not() {
        let a=config();
        let mut b=a.clone();
        assert_eq!(RenderKey::new(&a,1,true),RenderKey::new(&b,2,true));
        assert_ne!(RenderKey::new(&a,1,false),RenderKey::new(&b,2,false));
        b.format=wgpu::TextureFormat::Rgba8UnormSrgb;
        assert_ne!(RenderKey::new(&a,1,true),RenderKey::new(&b,2,true));
        b=a.clone();b.width=1440;b.height=2560;
        assert_ne!(RenderKey::new(&a,1,true),RenderKey::new(&b,2,true));
    }
    #[test]
    fn benchmark_modes_control_rendering_and_presentation_independently() {
        let now=Instant::now();
        let interval=Duration::from_millis(34);
        let later=now+interval;
        assert!(BenchmarkMode::parse("invalid").is_err());
        assert!(BenchmarkMode::Render.needs_render(Some(now),later,interval));
        assert!(!BenchmarkMode::Render.needs_render(Some(now),now,interval));
        for mode in [BenchmarkMode::Replay,BenchmarkMode::Hold] {
            assert!(mode.needs_render(None,now,interval));
            assert!(!mode.needs_render(Some(now),later,interval));
        }
        assert!(BenchmarkMode::Replay.should_present(Some(now)));
        assert!(BenchmarkMode::Hold.should_present(None));
        assert!(!BenchmarkMode::Hold.should_present(Some(now)));
    }

}
