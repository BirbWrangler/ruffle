use anyhow::{anyhow, Result};
use clap::Parser;
use image::{RgbaImage, SubImage, GenericImage, GenericImageView};
use ruffle_core::focus_tracker::{TDisplayObject, TDisplayObjectContainer};
use ruffle_core::swf::avm2::read::Reader;
use ruffle_core::swf::avm2::types::{Multiname, AbcFile, Namespace};
use ruffle_core::swf::{self, Tag, SymbolClassLink, UTF_8, Rectangle, Twips};
use ruffle_core::{PlayerBuilder, Color, Player, ViewportDimensions};
use ruffle_core::limits::ExecutionLimit;
use ruffle_core::tag_utils::SwfMovie;
use ruffle_render_wgpu::target::TextureTarget;
use std::fs::{create_dir_all, remove_dir_all};
use std::io;
use std::panic::catch_unwind;
use std::path::{Path, PathBuf};
use ruffle_render_wgpu::clap::{GraphicsBackend, PowerPreference};
use ruffle_render_wgpu::backend::{request_adapter_and_device, WgpuRenderBackend};
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::wgpu;
use std::sync::{Arc, Mutex};

const RENDER_WIDTH: u32 = 2048;
const RENDER_HEIGHT: u32 = 2048;

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

    /// Skip unsupported movie types (currently AVM 2)
    #[clap(long, action)]
    skip_unsupported: bool,

    /// Clear the output folder before exporting all the textures
    #[clap(long, short, action)]
    clear_textures: bool,
}

#[cfg(not(feature = "render_trace"))]
fn trace_path(_opt: &Opt) -> Option<&Path> {
    None
}

fn get_namespace_name<'a>(namespace: &Namespace, abc: &'a AbcFile) -> &'a String {
    match namespace {
        Namespace::Namespace(i) => abc.constant_pool.strings.get((i.0-1) as usize).expect("Could not get string!"),
        Namespace::Package(i) => abc.constant_pool.strings.get((i.0-1) as usize).expect("Could not get string!"),
        Namespace::PackageInternal(i) => abc.constant_pool.strings.get((i.0-1) as usize).expect("Could not get string!"),
        Namespace::Protected(i) => abc.constant_pool.strings.get((i.0-1) as usize).expect("Could not get string!"),
        Namespace::Explicit(i) => abc.constant_pool.strings.get((i.0-1) as usize).expect("Could not get string!"),
        Namespace::StaticProtected(i) => abc.constant_pool.strings.get((i.0-1) as usize).expect("Could not get string!"),
        Namespace::Private(i) => abc.constant_pool.strings.get((i.0-1) as usize).expect("Could not get string!"),
    }
}

fn get_name<'a>(name: &Multiname, abc: &'a AbcFile) -> (&'a String, &'a String) {
    match name {
        Multiname::QNameA { namespace: _, name: _ } => panic!("This name type is not supported yet!"),
        Multiname::RTQName { name: _ } => panic!("This name type is not supported yet!"),
        Multiname::RTQNameA { name: _ } => panic!("This name type is not supported yet!"),
        Multiname::RTQNameL => panic!("This name type is not supported yet!"),
        Multiname::RTQNameLA => panic!("This name type is not supported yet!"),
        Multiname::MultinameA { namespace_set: _, name: _ } => panic!("This name type is not supported yet!"),
        Multiname::MultinameL { namespace_set: _ } => panic!("This name type is not supported yet!"),
        Multiname::MultinameLA { namespace_set: _ } => panic!("This name type is not supported yet!"),
        Multiname::TypeName { base_type: _, parameters: _ } => panic!("This name type is not supported yet!"),
        Multiname::QName { namespace, name } => {
            let ns = abc.constant_pool.namespaces.get((namespace.0-1) as usize).expect("Could not get namespace!");
            let ns = get_namespace_name(ns, abc);
            let name = abc.constant_pool.strings.get((name.0-1) as usize).expect("Could not get string!");
            (ns, name)
        },
        Multiname::Multiname { namespace_set: _, name: _ } => panic!("This name type is not supported yet!"),
    }
}

#[derive(Debug)]
struct ClassName {
    namespace: String,
    name: String
}

#[derive(Debug)]
struct ExportedTexture {
    classname: ClassName,
    supername: ClassName,
    id: Option<u16>,
}

fn parse_swf_for_textures(path: &Path) -> Vec<ExportedTexture> {
    let data = std::fs::read(path).expect("Could not read swf file!");
    let buf = swf::decompress_swf(&data[..]).expect("Could not decompress swf!");
    let swf = swf::parse_swf(&buf).expect("Could not parse swf!");

    let mut result = Vec::new();

    let mut links: Option<&Vec<SymbolClassLink<'_>>> = None;

    for t in &swf.tags {
        if let Tag::SymbolClass(vec) = t {
            links = Some(vec);
        }
        if let Tag::DoAbc(_abc) = t {
            println!("WARNING :: Abc v1 is not supported yet");
        }
        if let Tag::DoAbc2(abc) = t {
            let mut reader = Reader::new(abc.data);
            let abc = reader.read().expect("Could not read avm data!");
            for i in &abc.instances {
                let name = abc.constant_pool.multinames.get((i.name.0-1) as usize).expect("No index at thingy!");
                let name = get_name(name, &abc);
                let supername = abc.constant_pool.multinames.get((i.super_name.0-1) as usize).expect("No index at thingy!");
                let supername = get_name(supername, &abc);
                // println!("instance found: name : {:?} supername : {:?}", name, supername);

                if name.0 == "com.exported.textures" && supername.0 == "com.lachhh.flash" && supername.1 == "FlashAnimationTexture" {
                    result.push(ExportedTexture {
                        classname: ClassName { namespace: name.0.clone(), name: name.1.clone() },
                        supername: ClassName { namespace: supername.0.clone(), name: supername.1.clone() },
                        id: None,
                    });
                }
            }
        }
    }

    for link in links.expect("No class links found in swf!") {
        result.iter_mut().for_each(|tex| {
            if format!("{}.{}", tex.classname.namespace, tex.classname.name) == link.class_name.to_string_lossy(UTF_8) {
                tex.id = Some(link.id);
            }
        });
    }

    result
}

fn set_up_player(
    descriptors: Arc<Descriptors>,
    swf_path: &Path,
    skip_unsupported: bool,
) -> Result<Arc<Mutex<Player>>> {
    let movie = SwfMovie::from_path(swf_path, None).map_err(|e| anyhow!(e.to_string()))?;

    if movie.is_action_script_3() && skip_unsupported {
        return Err(anyhow!("Skipping unsupported movie"));
    }

    // let width = movie.width().to_pixels();

    // let height = movie.height().to_pixels();

    let target = TextureTarget::new(&descriptors.device, (RENDER_WIDTH, RENDER_HEIGHT))
        .map_err(|e| anyhow!(e.to_string()))?;
    let player = PlayerBuilder::new()
        .with_renderer(
            WgpuRenderBackend::new(descriptors, target).map_err(|e| anyhow!(e.to_string()))?,
        )
        .with_movie(movie)
        .with_viewport_dimensions(RENDER_WIDTH, RENDER_HEIGHT, 1.0)
        .with_scale_mode(ruffle_core::StageScaleMode::NoScale, false)
        .build();

    player.lock().unwrap().set_window_mode("transparent");

    player.lock().unwrap().preload(&mut ExecutionLimit::none());

    player.lock().unwrap().run_frame();

    player.lock().unwrap().update(|context| {
        context.stage.set_background_color(context.gc_context, Some(Color::GREEN));

        let mut stage = context.stage;

        // remove all children
        stage.remove_range(context, 0..stage.num_children());

        stage.construct_frame(context);
        stage.frame_constructed(context);

        context.stage.set_invalidated(context.gc_context, true);
    });
    Ok(player)
}

fn prepare_stage(player: &Arc<Mutex<Player>>, texture: &ExportedTexture) -> (u32, u32) {

    let mut width: u32 = 0;
    let mut height: u32 = 0;

    player.lock().unwrap().update(|context| {
        context.stage.set_background_color(context.gc_context, Some(Color::GREEN));

        let mut stage = context.stage;

        // remove all children
        stage.remove_range(context, 0..stage.num_children());

        let movie = context.library.known_movies().get(0).expect("no first movie!").clone();

        let lib = context.library.library_for_movie(movie).expect("No lib for movie!");

        let mc = lib.instantiate_by_id(texture.id.expect("ID is none!"), context.gc_context).expect("Could not get symbol!");

        stage.insert_at_index(context, mc, 0);

        // mc.post_instantiation(context, None, Instantiator::Movie, false);
        mc.enter_frame(context);

        stage.construct_frame(context);
        stage.frame_constructed(context);

        let bounds = mc.bounds();
        // println!("x_min: {} x_max: {} y_min: {} y_max: {}", bounds.x_min.to_pixels(), bounds.x_max.to_pixels(), bounds.y_min.to_pixels(), bounds.y_max.to_pixels());

        width = mc.width() as u32;
        height = mc.height() as u32;

        mc.set_x(context.gc_context, bounds.x_min * -1);
        mc.set_y(context.gc_context, bounds.y_min * -1);

        stage.construct_frame(context);
        stage.frame_constructed(context);

        // let bounds = mc.world_bounds();
        // println!("x_min: {} x_max: {} y_min: {} y_max: {}", bounds.x_min.to_pixels(), bounds.x_max.to_pixels(), bounds.y_min.to_pixels(), bounds.y_max.to_pixels());

        context.stage.set_invalidated(context.gc_context, true);
    });

    (width, height)

}


/// Captures a screenshot. The resulting image uses straight alpha
fn take_screenshot(
    player: &Arc<Mutex<Player>>,
    texture: &ExportedTexture
) -> Result<RgbaImage> {
    match catch_unwind(|| {
        player.lock().unwrap().render();
        let mut player = player.lock().unwrap();
        let renderer = player
            .renderer_mut()
            .downcast_mut::<WgpuRenderBackend<TextureTarget>>()
            .unwrap();
        renderer.capture_frame()
    }) {
        Ok(Some(image)) => Ok(image),
        Ok(None) => return Err(anyhow!("Unable to capture frame of {:?}", texture.classname.name)),
        Err(e) => {
            return Err(anyhow!(
                "Unable to capture frame of {:?}: {:?}",
                texture.classname.name,
                e
            ))
        }
    }
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

    let textures = parse_swf_for_textures(&opt.swf);

    let player = set_up_player(
        descriptors,
        &opt.swf,
        opt.skip_unsupported
    )?;

    let (m_width, m_height) = {
        let mut player = player.lock().unwrap();
        (player.movie_width(), player.movie_height())
    };

    for texture in textures {
        let (width, height) = prepare_stage(&player, &texture);
        let image = take_screenshot(&player, &texture)?;

        let (half_width, half_height) = {
            ((((RENDER_WIDTH-m_width) as f32) / 2.0).round() as u32, (((RENDER_HEIGHT-m_height) as f32) / 2.0).round() as u32)
        };

        let image = image.view(half_width, half_height, width, height).to_image();

        let mut bytes: Vec<u8> = Vec::new();
        image
            .write_to(
                &mut io::Cursor::new(&mut bytes),
                image::ImageOutputFormat::Png,
            )
            .expect("Encoding failed");

        let img_file = format!("{}.png", texture.classname.name);

        let path = swf_output.join(img_file);

        // println!("writing: {:?}", path);
        std::fs::write(path, bytes)?;
    }

    Ok(())
}