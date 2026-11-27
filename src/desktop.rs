//! Minimal native Wayland backend. The renderer never sees Wayland objects.
use crate::{renderer::{Gpu,Renderer},scene::Scene};
use anyhow::{bail, Context, Result};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use notify::{EventKind, RecursiveMode, Watcher};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use std::{collections::BTreeMap, path::{Path,PathBuf}, ptr::NonNull, sync::{Arc,atomic::{AtomicBool,Ordering},mpsc}, time::{Duration,Instant}};
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
struct RenderSurface { surface:wgpu::Surface<'static>, config:wgpu::SurfaceConfiguration, renderer:Renderer }

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
            match reload(&gpu,&mut state,&path) {
                Ok(new_scene)=>{ scene=new_scene; eprintln!("Reloaded {}",path.display()); },
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
                configure(&instance,&connection,&gpu,screen,&scene,base)?;
                screen.pending=false;
            }
            if !screen.ready { continue; }
            if let Some(last)=screen.last_frame {
                let remaining=interval.saturating_sub(now.saturating_duration_since(last));
                if !remaining.is_zero() { timeout=timeout.min(remaining); continue; }
            }
            let frame_time=now.duration_since(start).as_secs_f64();
            if draw(&gpu,screen,&qh,frame_time)? { screen.last_frame=Some(now); }
            else { timeout=timeout.min(interval); }
        }
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
fn configure(instance:&wgpu::Instance,connection:&Connection,gpu:&Gpu,screen:&mut Screen,scene:&Scene,base:&Path)->Result<()> {
    let width=screen.logical.0.checked_mul(screen.scale as u32).context("Output width overflow")?;
    let height=screen.logical.1.checked_mul(screen.scale as u32).context("Output height overflow")?;
    crate::renderer::check_size(gpu,width,height)?;
    if let Some(r)=&mut screen.render {
        r.renderer.resize(gpu,width,height)?;
        r.config.width=width; r.config.height=height;
        r.surface.configure(&gpu.device,&r.config);
    } else {
        let surface=make_surface(instance,connection,&screen.surface)?;
        let caps=surface.get_capabilities(&gpu.adapter);
        let format=caps.formats.iter().copied().find(|f|f.is_srgb()).or_else(||caps.formats.first().copied()).context("GPU cannot present to this output")?;
        let alpha_mode=if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) { wgpu::CompositeAlphaMode::Opaque } else { *caps.alpha_modes.first().context("No supported alpha modes")? };
        let config=wgpu::SurfaceConfiguration { usage:wgpu::TextureUsages::RENDER_ATTACHMENT,format,width,height,present_mode:wgpu::PresentMode::Fifo,desired_maximum_frame_latency:2,alpha_mode,view_formats:vec![] };
        let mut renderer=Renderer::new(gpu,scene.clone(),base,width,height,format)?;
        let id=*screen.surface.data::<u32>().context("Missing surface id")?;
        renderer.enable_profile(gpu,format!("surface={id}"));
        surface.configure(&gpu.device,&config);
        screen.render=Some(RenderSurface{surface,config,renderer});
        eprintln!("Surface {width}x{height} ({format:?})");
    }
    Ok(())
}
fn draw(gpu:&Gpu,screen:&mut Screen,qh:&QueueHandle<Desktop>,t:f64)->Result<bool> {
    let r=screen.render.as_mut().context("Surface not configured")?;
    let frame=match r.surface.get_current_texture() {
        Ok(f)=>f,
        Err(wgpu::SurfaceError::Lost|wgpu::SurfaceError::Outdated)=>{ screen.pending=true; return Ok(false); },
        Err(wgpu::SurfaceError::Timeout)=>{eprintln!("Wayland surface acquisition timed out");return Ok(false);},
        Err(wgpu::SurfaceError::OutOfMemory)=>bail!("GPU surface out of memory"),
    };
    let view=frame.texture.create_view(&Default::default());
    let mut encoder=gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor{label:Some("live frame")});
    r.renderer.render(gpu,&mut encoder,&view,t);
    gpu.queue.submit(Some(encoder.finish()));
    // Request before presentation, so the callback is associated with the next commit.
    // The callback's user data matches the wl_surface's registry id.
    let id=*screen.surface.data::<u32>().context("Missing surface id")?;
    screen.surface.frame(qh,id);
    screen.ready=false;
    frame.present();
    r.renderer.profile_submitted();
    Ok(true)
}
fn reload(gpu:&Gpu,state:&mut Desktop,path:&Path)->Result<Scene> {
    let scene=Scene::load(path)?;
    let base=path.parent().unwrap_or(Path::new("."));
    let mut replacements=Vec::new();
    for (&id,screen) in &state.surfaces {
        if let Some(r)=&screen.render {
            let mut new=Renderer::new(gpu,scene.clone(),base,r.config.width,r.config.height,r.config.format)?;
            new.enable_profile(gpu,format!("surface={id}"));
            replacements.push((id,new));
        }
    }
    for (id,new) in replacements { state.surfaces.get_mut(&id).unwrap().render.as_mut().unwrap().renderer=new; }
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
