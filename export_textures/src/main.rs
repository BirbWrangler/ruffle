use anyhow::{anyhow, Result};
use clap::Parser;
use image::RgbaImage;
use ruffle_core::PlayerBuilder;
use ruffle_core::limits::ExecutionLimit;
use ruffle_core::tag_utils::SwfMovie;
use ruffle_render_wgpu::target::TextureTarget;
use std::fs::{create_dir_all, remove_dir_all};
use std::io::{self, Write};
use std::panic::catch_unwind;
use std::path::{Path, PathBuf};
use ruffle_render_wgpu::clap::{GraphicsBackend, PowerPreference};
use ruffle_render_wgpu::backend::{request_adapter_and_device, WgpuRenderBackend};
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::wgpu;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[clap(name = "Swf Texture Exporter", author, version)]
struct Opt {
    /// The file or directory of files to export frames from
    #[clap(name = "swf")]
    swf: PathBuf,

    /// The directory to store the capture in.
    #[clap(name = "output")]
    output_path: PathBuf,

    /// Type of graphics backend to use. Not all options may be supported by your current system.
    /// Default will attempt to pick the most supported graphics backend.
    #[clap(long, short, default_value = "default")]
    graphics: GraphicsBackend,

    /// Power preference for the graphics device used. High power usage tends to prefer dedicated GPUs,
    /// whereas a low power usage tends prefer integrated GPUs.
    #[clap(long, short, default_value = "high")]
    power: PowerPreference,

    /// Location to store a wgpu trace output
    #[clap(long)]
    #[cfg(feature = "render_trace")]
    trace_path: Option<PathBuf>,

    /// Skip unsupported movie types (currently AVM 2)
    #[clap(long, action)]
    skip_unsupported: bool,

    /// Clear the output folder before exporting all the textures
    #[clap(long, short, action)]
    clear_textures: bool,
}

#[cfg(feature = "render_trace")]
fn trace_path(opt: &Opt) -> Option<&Path> {
    if let Some(path) = &opt.trace_path {
        let _ = std::fs::create_dir_all(path);
        Some(path)
    } else {
        None
    }
}

#[cfg(not(feature = "render_trace"))]
fn trace_path(_opt: &Opt) -> Option<&Path> {
    None
}

/// Captures a screenshot. The resulting image uses straight alpha
fn take_screenshot(
    descriptors: Arc<Descriptors>,
    swf_path: &Path,
    skip_unsupported: bool,
) -> Result<Vec<RgbaImage>> {
    let movie = SwfMovie::from_path(swf_path, None).map_err(|e| anyhow!(e.to_string()))?;

    if movie.is_action_script_3() && skip_unsupported {
        return Err(anyhow!("Skipping unsupported movie"));
    }

    let width = movie.width().to_pixels();

    let height = movie.height().to_pixels();

    let target = TextureTarget::new(&descriptors.device, (width as u32, height as u32))
        .map_err(|e| anyhow!(e.to_string()))?;
    let player = PlayerBuilder::new()
        .with_renderer(
            WgpuRenderBackend::new(descriptors, target).map_err(|e| anyhow!(e.to_string()))?,
        )
        .with_movie(movie)
        .with_viewport_dimensions(width as u32, height as u32, 1.0)
        .build();

    // Maybe add external interface for rendering out shapes? That might make everything as compatable as possible
    // player.lock().unwrap().add_external_interface(provider)

    let mut result = Vec::new();
    let totalframes = 1;

    for i in 0..totalframes {

        player.lock().unwrap().preload(&mut ExecutionLimit::none());

        player.lock().unwrap().run_frame();
        if i >= 0 {
            match catch_unwind(|| {
                player.lock().unwrap().render();
                let mut player = player.lock().unwrap();
                let renderer = player
                    .renderer_mut()
                    .downcast_mut::<WgpuRenderBackend<TextureTarget>>()
                    .unwrap();
                renderer.capture_frame()
            }) {
                Ok(Some(image)) => result.push(image),
                Ok(None) => return Err(anyhow!("Unable to capture frame {} of {:?}", i, swf_path)),
                Err(e) => {
                    return Err(anyhow!(
                        "Unable to capture frame {} of {:?}: {:?}",
                        i,
                        swf_path,
                        e
                    ))
                }
            }
        }
    }
    Ok(result)
}

fn main() -> Result<()> {
    let opt: Opt = Opt::parse();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: opt.graphics.into(),
        dx12_shader_compiler: wgpu::Dx12Compiler::default(),
    });
    let (adapter, device, queue) = futures::executor::block_on(request_adapter_and_device(
        opt.graphics.into(),
        &instance,
        None,
        opt.power.into(),
        trace_path(&opt),
    ))
    .map_err(|e| anyhow!(e.to_string()))?;

    let descriptors = Arc::new(Descriptors::new(instance, adapter, device, queue));

    if !opt.swf.is_file() {
        return Err(anyhow!(
            "Swf argument is not a file or does not exist!"
        ));
    }

    if !opt.output_path.is_dir() {
        return Err(anyhow!(
            "Output path is not a directory or does not exist!"
        ));
    }

    let docname = opt.swf.file_stem().ok_or_else(|| anyhow!("Could not get file stem of swf!"))?;

    let swf_output = &opt.output_path.join(docname);

    if opt.clear_textures {
        let _ = remove_dir_all(swf_output);
    }

    let _ = create_dir_all(&opt.output_path.join(docname));

    let frames = take_screenshot(
        descriptors,
        &opt.swf,
        opt.skip_unsupported
    )?;

    let image = frames.get(0).unwrap();

    let mut bytes: Vec<u8> = Vec::new();
    image
        .write_to(
            &mut io::Cursor::new(&mut bytes),
            image::ImageOutputFormat::Png,
        )
        .expect("Encoding failed");

    let path = swf_output.join("test.png");

    std::fs::write(path, bytes)?;

    Ok(())
}