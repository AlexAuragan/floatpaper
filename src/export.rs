use crate::{renderer::{self, Gpu, Renderer}, scene::Scene};
use anyhow::{bail, Context, Result};
use std::{fs, io::Write, path::{Path, PathBuf}, process::{Command, Stdio}, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Instant};

pub struct Options { pub width:u32, pub height:u32, pub fps:Option<u32>, pub duration:Option<f64>, pub crf:u8, pub allow_non_loop:bool, pub overwrite:bool }
pub fn run(scene_path:&Path, output:&Path, options:Options) -> Result<()> {
    let scene=Scene::load(scene_path)?;
    let fps=options.fps.unwrap_or(scene.fps);
    if !(1..=240).contains(&fps) { bail!("fps must be 1..=240"); }
    let duration=options.duration.or(scene.loop_seconds).context("Set --duration for a non-looping scene")?;
    let frames=frame_count(duration,fps)?;
    if !options.allow_non_loop {
        let loop_seconds=scene.loop_seconds.context("Scene is not periodic; use --allow-non-loop")?;
        let loops=duration/loop_seconds;
        if loops<1.0-1e-8 || (loops-loops.round()).abs()>1e-8 { bail!("Duration must be an integer multiple of the scene loop ({loop_seconds}s); use --allow-non-loop to override"); }
    }
    let (width,height)=(options.width,options.height);
    if width==0 || height==0 || width%2!=0 || height%2!=0 { bail!("H.264 yuv420p requires positive, even dimensions"); }
    if options.crf>51 { bail!("CRF must be 0..=51"); }
    if output.extension().and_then(|s|s.to_str())!=Some("mp4") { bail!("Output must have .mp4 extension"); }
    if output.exists() && !options.overwrite { bail!("Output already exists; use --overwrite"); }
    let base=scene_path.parent().unwrap_or(Path::new("."));
    let instance=Gpu::instance();
    let gpu=Gpu::new(&instance,None)?;
    let format=wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer=Renderer::new(&gpu,scene,base,width,height,format)?;
    let texture=gpu.device.create_texture(&wgpu::TextureDescriptor { label:Some("export frame"),size:renderer::extent(width,height),mip_level_count:1,sample_count:1,dimension:wgpu::TextureDimension::D2,format,usage:wgpu::TextureUsages::RENDER_ATTACHMENT|wgpu::TextureUsages::COPY_SRC,view_formats:&[] });
    let view=texture.create_view(&Default::default());
    let row_bytes=width as usize*4;
    let padded=renderer::align_up(row_bytes,wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let readback=gpu.device.create_buffer(&wgpu::BufferDescriptor { label:Some("export readback"),size:(padded*height as usize) as u64,usage:wgpu::BufferUsages::COPY_DST|wgpu::BufferUsages::MAP_READ,mapped_at_creation:false });
    // Encode to a temporary file on the same filesystem; never replace a good video until encoding succeeds.
    let temporary=temporary_path(output)?;
    let mut child=Command::new("ffmpeg").args(["-hide_banner","-loglevel","warning","-n","-f","rawvideo","-pixel_format","rgba","-video_size",&format!("{width}x{height}"),"-framerate",&fps.to_string(),"-i","pipe:0","-an","-vf","scale=out_color_matrix=bt709:out_range=tv,format=yuv420p","-c:v","libx264","-preset","medium","-crf",&options.crf.to_string(),"-color_primaries","bt709","-color_trc","iec61966-2-1","-colorspace","bt709","-color_range","tv","-movflags","+faststart"])
        .arg(&temporary).stdin(Stdio::piped()).spawn().context("Starting FFmpeg (requires libx264)")?;
    let mut input=child.stdin.take().context("FFmpeg stdin unavailable")?;
    let running=Arc::new(AtomicBool::new(true));
    let r=running.clone();
    if let Err(e)=ctrlc::set_handler(move||r.store(false,Ordering::SeqCst)) { let _=child.kill(); let _=child.wait(); let _=fs::remove_file(&temporary); return Err(e.into()); }
    let start=Instant::now();
    eprintln!("Exporting {frames} frames, {width}x{height}, {fps} fps, {duration:.3} seconds");
    let result=(||->Result<()> {
        for frame in 0..frames {
            if !running.load(Ordering::SeqCst) { bail!("Export cancelled"); }
            let mut encoder=gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label:Some("export frame") });
            renderer.render(&gpu,&mut encoder,&view,frame as f64/fps as f64);
            encoder.copy_texture_to_buffer(wgpu::ImageCopyTexture { texture:&texture,mip_level:0,origin:wgpu::Origin3d::ZERO,aspect:wgpu::TextureAspect::All },wgpu::ImageCopyBuffer { buffer:&readback,layout:wgpu::ImageDataLayout { offset:0,bytes_per_row:Some(padded as u32),rows_per_image:Some(height) } },renderer::extent(width,height));
            gpu.queue.submit(Some(encoder.finish()));
            let slice=readback.slice(..);
            let (tx,rx)=std::sync::mpsc::sync_channel(1);
            slice.map_async(wgpu::MapMode::Read,move|result| { let _=tx.send(result); });
            gpu.device.poll(wgpu::Maintain::Wait);
            rx.recv().context("GPU readback callback lost")?.context("GPU readback failed")?;
            let data=slice.get_mapped_range();
            let write_result=(||->Result<()> { for row in data.chunks_exact(padded) { input.write_all(&row[..row_bytes]).context("Writing video frame to FFmpeg")?; } Ok(()) })();
            drop(data); readback.unmap(); write_result?;
            if frame%((fps as u64)*5)==0 || frame+1==frames { eprintln!("{}/{} frames ({:.1}%), {:.1} render fps",frame+1,frames,100.*(frame+1) as f64/frames as f64,(frame+1) as f64/start.elapsed().as_secs_f64().max(0.001)); }
        }
        Ok(())
    })();
    drop(input);
    if result.is_err() { let _=child.kill(); }
    let status=child.wait();
    if let Err(e)=result { let _=fs::remove_file(&temporary); return Err(e); }
    let status=status.context("Waiting for FFmpeg")?;
    if !status.success() { let _=fs::remove_file(&temporary); bail!("FFmpeg exited with {status}"); }
    if options.overwrite { fs::rename(&temporary,output).with_context(||format!("Video complete at {}; could not move to {}",temporary.display(),output.display()))?; }
    else { fs::hard_link(&temporary,output).with_context(||format!("Video complete at {}; could not publish without overwriting {}",temporary.display(),output.display()))?; fs::remove_file(&temporary)?; }
    eprintln!("Saved {}",output.display());
    Ok(())
}
fn temporary_path(output:&Path)->Result<PathBuf> {
    let name=output.file_name().context("Missing output filename")?.to_string_lossy();
    let path=output.with_file_name(format!(".{name}.{}.partial.mp4",std::process::id()));
    if path.exists() { bail!("Temporary output already exists: {}",path.display()); }
    Ok(path)
}
fn frame_count(duration:f64,fps:u32)->Result<u64> {
    let count=duration*fps as f64;
    if !count.is_finite() || count<1.0 || count>10_000_000.0 || (count-count.round()).abs()>1e-6 { bail!("duration * fps must be a whole number of frames between 1 and 10000000"); }
    Ok(count.round() as u64)
}
#[cfg(test)] mod tests { use super::*; #[test] fn exact_frames() { assert_eq!(frame_count(1200.,30).unwrap(),36000); assert!(frame_count(0.123,30).is_err()); } }
