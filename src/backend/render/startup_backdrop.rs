// SPDX-License-Identifier: GPL-3.0-only

//! WMDE: the desktop wallpaper under everything until wmde-bg maps it.
//!
//! The session's first frames would show `CLEAR_COLOR` until `wmde-bg` maps its wallpaper;
//! instead they show the wallpaper wmde-bg is about to show, read from its own config and
//! scaled the same way, so the boot splash (autologin) or the login screen goes straight to the
//! desktop. The compositor of the login screen itself shows the greeter's picture instead: it
//! has no wallpaper, and in that role nobody's wallpaper is read. Once any output has a
//! wallpaper everything here is dropped for good and never decoded again.

use std::{
    borrow::Borrow,
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use cosmic_bg_config::{Color, Entry, ScalingMode, Source, state::State as BgState};
use cosmic_config::{ConfigGet, CosmicConfigEntry};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportMem, Renderer, Texture,
            element::{Id, Kind, texture::TextureRenderElement},
            gles::{GlesRenderer, GlesTexture},
            glow::GlowRenderer,
            utils::with_renderer_surface_state,
        },
    },
    desktop::layer_map_for_output,
    output::Output,
    utils::{Buffer, Logical, Point, Rectangle, Size, Transform},
    wayland::shell::wlr_layer::Layer,
};
use tracing::{info, warn};

use super::element::AsGlowRenderer;
use crate::utils::prelude::*;

/// The greeter's background, `BACKGROUND_PATH` in wmde-greeter.
const GREETER_BACKGROUND: &str = "/usr/share/wmde-greeter/background.jpg";
/// The program the login screen's compositor runs, `wmde-greeter-start` in wmde-greeter.
const GREETER_COMMAND: &str = "wmde-greeter";
/// The layer namespace of wmde-bg's wallpaper surfaces.
const WALLPAPER_NAMESPACE: &str = "wallpaper";

/// Set once a wallpaper has been seen; from then on nothing here does any work.
static DONE: AtomicBool = AtomicBool::new(false);

/// What to draw, worked out and decoded off the render path by `preload`.
static BACKDROP: Mutex<Option<Backdrop>> = Mutex::new(None);
static PRELOAD: OnceLock<()> = OnceLock::new();

/// What one output shows, as wmde-bg draws it.
#[derive(Clone, Debug)]
enum Fill {
    Picture(PathBuf, ScalingMode),
    Color([u8; 4]),
}

struct Backdrop {
    /// By output name; `DEFAULT_BACKGROUND` ("all") for every output without one of its own.
    fills: HashMap<String, Fill>,
    /// Decoded RGBA pixels and their size, one per distinct picture.
    pixels: HashMap<PathBuf, (Vec<u8>, Size<i32, Buffer>)>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum TextureKey {
    Picture(PathBuf),
    Color([u8; 4]),
}

type Textures = HashMap<TextureKey, (Id, GlesTexture)>;

/// The textures, per GL context: uploaded on first use, dropped when `DONE` is set.
struct BackdropTextures(RefCell<Textures>);

/// Starts working out and decoding the backdrop on a thread of its own. Called once at
/// startup; until it is ready, frames show the clear colour as before.
pub fn preload() {
    PRELOAD.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("startup-backdrop".into())
            .spawn(|| {
                let fills = if is_greeter() {
                    HashMap::from([(
                        cosmic_bg_config::DEFAULT_BACKGROUND.to_owned(),
                        Fill::Picture(GREETER_BACKGROUND.into(), ScalingMode::Zoom),
                    )])
                } else {
                    wallpaper_fills()
                };
                let mut pixels = HashMap::new();
                for fill in fills.values() {
                    if let Fill::Picture(path, _) = fill
                        && !pixels.contains_key(path)
                        && let Some(decoded) = decode(path)
                    {
                        pixels.insert(path.clone(), decoded);
                    }
                }
                info!(?fills, "Startup backdrop");
                if !DONE.load(Ordering::Acquire) {
                    *BACKDROP.lock().unwrap() = Some(Backdrop { fills, pixels });
                }
            });
        if let Err(err) = spawned {
            warn!(?err, "Failed to start preparing the startup backdrop");
        }
    });
}

/// Whether this compositor runs the login screen: `wmde-comp [options] wmde-greeter`.
fn is_greeter() -> bool {
    std::env::args_os()
        .skip(1)
        .find(|arg| !arg.to_string_lossy().starts_with('-'))
        .is_some_and(|command| Path::new(&command).file_name() == Some(GREETER_COMMAND.as_ref()))
}

/// The wallpaper of every output that has its own, plus the default one, the way wmde-bg picks
/// them on startup. Only reads: `Context::same_on_all` would write its default back.
fn wallpaper_fills() -> HashMap<String, Fill> {
    let config = match cosmic_bg_config::context() {
        Ok(context) => context.0,
        Err(err) => {
            warn!(?err, "No wallpaper config for the startup backdrop");
            return HashMap::new();
        }
    };
    let state = BgState::state()
        .ok()
        .map(|state| match BgState::get_entry(&state) {
            Ok(state) => state,
            Err((_, state)) => state,
        })
        .unwrap_or_default();

    let mut entries = vec![
        config
            .get::<Entry>(cosmic_bg_config::DEFAULT_BACKGROUND)
            .unwrap_or_else(|_| Entry::fallback()),
    ];
    if !config
        .get::<bool>(cosmic_bg_config::SAME_ON_ALL)
        .unwrap_or(true)
    {
        let outputs = config
            .get::<Vec<String>>(cosmic_bg_config::BACKGROUNDS)
            .unwrap_or_default();
        entries.extend(
            outputs
                .iter()
                .filter_map(|output| config.get::<Entry>(&format!("output.{output}")).ok()),
        );
    }

    entries
        .into_iter()
        .filter_map(|entry| {
            let fill = match &entry.source {
                Source::Path(path) if path.is_dir() => {
                    // A slideshow resumes at the picture it showed last, kept in wmde-bg's state.
                    let dir = path.canonicalize().ok()?;
                    let mut shown = state.wallpapers.iter().filter_map(|(output, source)| {
                        let Source::Path(shown) = source else {
                            return None;
                        };
                        let own = entry.output == cosmic_bg_config::DEFAULT_BACKGROUND
                            || *output == entry.output;
                        (own && shown.starts_with(&dir) && shown.is_file()).then_some(shown)
                    });
                    Fill::Picture(shown.next()?.clone(), entry.scaling_mode.clone())
                }
                Source::Path(path) => Fill::Picture(path.clone(), entry.scaling_mode.clone()),
                Source::Color(Color::Single(color)) => Fill::Color(rgba(color)),
                // Drawn by wmde-bg's own code; the clear colour until it arrives.
                Source::Color(Color::Gradient(_)) => return None,
            };
            Some((entry.output, fill))
        })
        .collect()
}

fn decode(path: &Path) -> Option<(Vec<u8>, Size<i32, Buffer>)> {
    let picture = image::ImageReader::open(path)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(image::ImageError::IoError)
        .and_then(|reader| reader.decode());
    match picture {
        Ok(picture) => {
            // Opaque, as wmde-bg draws it into an Xrgb buffer: alpha is dropped after its
            // premultiplied resize, so a fully transparent pixel comes out black and any other
            // keeps its colour, whatever is underneath.
            let mut picture = picture.into_rgba8();
            for pixel in picture.pixels_mut() {
                if pixel[3] == 0 {
                    pixel.0 = [0, 0, 0, 255];
                } else {
                    pixel[3] = 255;
                }
            }
            let size = Size::from((picture.width() as i32, picture.height() as i32));
            Some((picture.into_raw(), size))
        }
        Err(err) => {
            warn!(?err, ?path, "No startup backdrop");
            None
        }
    }
}

/// wmde-bg's colours are 0..1 per channel, written into the buffer as they are.
fn rgba(color: &[f32; 3]) -> [u8; 4] {
    let channel = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    [channel(color[0]), channel(color[1]), channel(color[2]), 255]
}

fn has_wallpaper(output: &Output) -> bool {
    let map = layer_map_for_output(output);
    map.layers_on(Layer::Background).any(|layer| {
        layer.namespace() == WALLPAPER_NAMESPACE
            && with_renderer_surface_state(layer.wl_surface(), |state| state.buffer().is_some())
                .unwrap_or(false)
    })
}

/// The backdrop as elements covering `output`, top first: the picture, then the colour around
/// it. Empty once the output has its wallpaper, before the backdrop is ready, or if there is
/// none.
pub fn elements<R: AsGlowRenderer>(
    renderer: &mut R,
    output: &Output,
) -> Vec<TextureRenderElement<GlesTexture>> {
    if DONE.load(Ordering::Acquire) {
        return Vec::new();
    }

    let glow = renderer.glow_renderer_mut();
    let with_cache = |glow: &GlowRenderer, f: &mut dyn FnMut(&mut Textures)| {
        let gles: &GlesRenderer = glow.borrow();
        let user_data = gles.egl_context().user_data();
        user_data.insert_if_missing(|| BackdropTextures(RefCell::new(HashMap::new())));
        if let Some(cache) = user_data.get::<BackdropTextures>() {
            f(&mut cache.0.borrow_mut());
        }
    };

    if has_wallpaper(output) {
        DONE.store(true, Ordering::Release);
        BACKDROP.lock().unwrap().take();
        with_cache(glow, &mut |cache| cache.clear());
        return Vec::new();
    }

    let backdrop = BACKDROP.lock().unwrap();
    let Some(backdrop) = backdrop.as_ref() else {
        return Vec::new();
    };
    let Some(fill) = backdrop
        .fills
        .get(&output.name())
        .or_else(|| backdrop.fills.get(cosmic_bg_config::DEFAULT_BACKGROUND))
    else {
        return Vec::new();
    };

    let context_id = glow.context_id();
    let mut texture = |key: TextureKey| -> Option<(Id, GlesTexture)> {
        let mut found = None;
        with_cache(glow, &mut |cache| found = cache.get(&key).cloned());
        if found.is_some() {
            return found;
        }
        let uploaded = match &key {
            TextureKey::Picture(path) => {
                let (data, size) = backdrop.pixels.get(path)?;
                glow.import_memory(data, Fourcc::Abgr8888, *size, false)
            }
            TextureKey::Color(color) => {
                glow.import_memory(color, Fourcc::Abgr8888, (1, 1).into(), false)
            }
        };
        match uploaded {
            Ok(uploaded) => {
                let entry = (Id::new(), uploaded);
                with_cache(glow, &mut |cache| {
                    cache.insert(key.clone(), entry.clone());
                });
                Some(entry)
            }
            Err(err) => {
                warn!(?err, "Failed to upload the startup backdrop");
                None
            }
        }
    };

    let output_size = output.geometry().size.as_logical();
    let scale = output.current_scale().fractional_scale();
    let mut elements = Vec::with_capacity(2);
    let mut push = |(id, texture): (Id, GlesTexture),
                    location: Point<i32, Logical>,
                    src: Option<Rectangle<f64, Logical>>,
                    size: Size<i32, Logical>| {
        // Without a source rectangle smithay samples one the size of the destination, not of
        // the texture: the picture would come out at its own size with the edges smeared.
        let src = src.unwrap_or_else(|| {
            let full = texture.size();
            Rectangle::from_size((f64::from(full.w), f64::from(full.h)).into())
        });
        elements.push(TextureRenderElement::from_static_texture(
            id,
            context_id.clone(),
            location.to_f64().to_physical(scale),
            texture,
            1,
            Transform::Normal,
            None,
            Some(src),
            Some(size),
            None,
            Kind::Unspecified,
        ));
    };

    match fill {
        Fill::Color(color) => {
            if let Some(texture) = texture(TextureKey::Color(*color)) {
                push(texture, (0, 0).into(), None, output_size);
            }
        }
        Fill::Picture(path, mode) => {
            let Some(picture) = texture(TextureKey::Picture(path.clone())) else {
                return Vec::new();
            };
            let picture_size = picture.1.size();
            let (pw, ph) = (f64::from(picture_size.w), f64::from(picture_size.h));
            let (ow, oh) = (f64::from(output_size.w), f64::from(output_size.h));
            match mode {
                // Cover: fill the output and crop the overflow evenly, as `scaler::zoom` does.
                ScalingMode::Zoom => {
                    let zoom = (ow / pw).max(oh / ph);
                    let visible = Size::<f64, Logical>::from((ow / zoom, oh / zoom));
                    let src = Rectangle::new(
                        ((pw - visible.w) / 2.0, (ph - visible.h) / 2.0).into(),
                        visible,
                    );
                    push(picture, (0, 0).into(), Some(src), output_size);
                }
                ScalingMode::Stretch => push(picture, (0, 0).into(), None, output_size),
                // Centred inside the output, the rest in the entry's colour, as `scaler::fit`.
                ScalingMode::Fit(color) => {
                    let ratio = (ow / pw).min(oh / ph);
                    let size = Size::<i32, Logical>::from((
                        (pw * ratio).round() as i32,
                        (ph * ratio).round() as i32,
                    ));
                    let location =
                        Point::from(((output_size.w - size.w) / 2, (output_size.h - size.h) / 2));
                    push(picture, location, None, size);
                    if let Some(texture) = texture(TextureKey::Color(rgba(color))) {
                        push(texture, (0, 0).into(), None, output_size);
                    }
                }
            }
        }
    }
    elements
}
