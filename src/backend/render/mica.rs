// SPDX-License-Identifier: GPL-3.0-only

//! WMDE: Mica under the title bars the compositor draws itself (a trial, off by default).
//!
//! Mica as Windows 11 has it: not transparency, but the wallpaper - blurred and tinted once -
//! cut out at the window's position on screen. Windows underneath never show through, so
//! nothing is blurred per frame: the source is a small picture rebuilt only when the
//! wallpaper or the theme changes, and every title bar draws its own crop of it.
//!
//! The picture is taken from wmde-bg's `wallpaper` layer surface, which is wl_shm, so it is
//! exactly what is on screen whatever the background is (an image in any scaling, a colour, a
//! gradient, a slideshow frame). An inactive window gets the flat theme background instead,
//! as in Windows 11.
//!
//! Only server-side title bars (`CosmicWindow`) take part. Clients drawing their own title
//! bar, the stack tabs and the compositor's menus are untouched.

use std::{
    borrow::{Borrow, BorrowMut},
    cell::RefCell,
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use cosmic::theme::ThemeType;
use glam::{Affine2, Mat3, Vec2};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Frame, ImportMem, Texture,
            element::{Element, Id, RenderElement},
            gles::{
                GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
                UniformValue,
            },
            glow::{GlowFrame, GlowRenderer},
            utils::{CommitCounter, with_renderer_surface_state},
        },
    },
    desktop::layer_map_for_output,
    output::Output,
    reexports::wayland_server::{Resource, backend::ObjectId, protocol::wl_shm},
    utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform},
    wayland::{
        shell::wlr_layer::Layer,
        shm::{BufferData, with_buffer_contents},
    },
};
use tracing::{info, warn};

use super::{element::AsGlowRenderer, wayland::clipped_surface::ClippingShader};
use crate::utils::prelude::*;

/// The layer namespace of wmde-bg's wallpaper surfaces.
const WALLPAPER_NAMESPACE: &str = "wallpaper";
/// Longer side of the reduced wallpaper, in pixels. The result is blurred far beyond this
/// resolution, so more pixels only cost time.
const SOURCE_SIDE: usize = 256;
/// Blur radius as a share of the reduced picture's width: about 8 px of 240, which is some
/// 64 px on a 1920 px wide output.
const BLUR_SHARE: f32 = 1.0 / 30.0;
/// Tint opacity over the luminosity-blended wallpaper, Windows 11's Mica values.
const TINT_OPACITY_DARK: f32 = 0.8;
const TINT_OPACITY_LIGHT: f32 = 0.5;

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Bumped on every new source picture, so elements showing it report a new commit.
static GENERATION: AtomicUsize = AtomicUsize::new(1);
/// Why the last attempt found no picture, logged once per change of reason.
static LAST_MISS: AtomicUsize = AtomicUsize::new(0);

fn miss(reason: usize, message: &str) {
    if LAST_MISS.swap(reason, Ordering::Relaxed) != reason {
        warn!("Mica: {message}, title bars get the flat colour");
    }
}

/// Mica on or off, from `mica_titlebars` in the compositor config.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// What a source picture was built from; a mismatch means it is rebuilt.
#[derive(Debug, Clone, PartialEq)]
struct SourceKey {
    surface: ObjectId,
    commit: CommitCounter,
    tint: [u8; 4],
    dark: bool,
}

struct Source {
    key: SourceKey,
    generation: usize,
    size: Size<i32, Buffer>,
    pixels: Arc<Vec<u8>>,
}

/// Source pictures by output name, shared by every GL context.
static SOURCES: Mutex<Option<HashMap<String, Source>>> = Mutex::new(None);

/// Uploaded source pictures by output name, and the flat 1x1 fallback, per GL context.
#[derive(Default)]
struct MicaTextures {
    outputs: RefCell<HashMap<String, (usize, GlesTexture)>>,
    flat: RefCell<Option<([u8; 4], usize, GlesTexture)>>,
}

/// The picture for the output being rendered on this thread.
struct CurrentOutput {
    texture: GlesTexture,
    generation: usize,
    size: Size<f64, Logical>,
}

thread_local! {
    static CURRENT: RefCell<Option<CurrentOutput>> = const { RefCell::new(None) };
}

/// Clears the current output when the frame's elements are collected.
pub struct OutputGuard(());

impl Drop for OutputGuard {
    fn drop(&mut self) {
        CURRENT.with(|current| current.borrow_mut().take());
    }
}

/// Makes `output`'s picture the one title bars on this thread crop from, until the guard is
/// dropped. Windows are rendered without their output at hand, hence the thread-local; set it
/// before collecting a frame's window elements, while no layer map is locked.
pub fn prepare_output<R: AsGlowRenderer>(
    renderer: &mut R,
    output: &Output,
    theme: &cosmic::Theme,
) -> Option<OutputGuard> {
    if !enabled() {
        return None;
    }

    let (tint, dark) = tint(theme);
    let name = output.name();
    let (generation, size, pixels) = source(output, &name, tint, dark)?;

    let glow = renderer.glow_renderer_mut();
    let texture = {
        let gles: &GlesRenderer = (*glow).borrow();
        let user_data = gles.egl_context().user_data();
        user_data.insert_if_missing(MicaTextures::default);
        user_data
            .get::<MicaTextures>()?
            .outputs
            .borrow()
            .get(&name)
            .filter(|(cached, _)| *cached == generation)
            .map(|(_, texture)| texture.clone())
    };
    let texture = match texture {
        Some(texture) => texture,
        None => {
            let texture = match glow.import_memory(&pixels, Fourcc::Abgr8888, size, false) {
                Ok(texture) => texture,
                Err(err) => {
                    warn!(?err, "Failed to upload the Mica picture");
                    return None;
                }
            };
            let gles: &GlesRenderer = (*glow).borrow();
            if let Some(textures) = gles.egl_context().user_data().get::<MicaTextures>() {
                textures
                    .outputs
                    .borrow_mut()
                    .insert(name.clone(), (generation, texture.clone()));
            }
            texture
        }
    };

    CURRENT.with(|current| {
        *current.borrow_mut() = Some(CurrentOutput {
            texture,
            generation,
            size: output.geometry().size.as_logical().to_f64(),
        })
    });
    Some(OutputGuard(()))
}

/// The tint colour and whether the theme is dark: the window background of the theme.
fn tint(theme: &cosmic::Theme) -> ([u8; 4], bool) {
    let base = theme.cosmic().background(false).base;
    let to_u8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    (
        [to_u8(base.red), to_u8(base.green), to_u8(base.blue), 255],
        theme.cosmic().is_dark,
    )
}

/// The output's source picture, rebuilt if the wallpaper or the tint changed. `None` while
/// there is no wallpaper, or one in a format this does not read.
fn source(
    output: &Output,
    name: &str,
    tint: [u8; 4],
    dark: bool,
) -> Option<(usize, Size<i32, Buffer>, Arc<Vec<u8>>)> {
    let surface = {
        let map = layer_map_for_output(output);
        map.layers_on(Layer::Background)
            .find(|layer| layer.namespace() == WALLPAPER_NAMESPACE)
            .map(|layer| layer.wl_surface().clone())
    };
    let Some(surface) = surface else {
        miss(1, "no wallpaper layer on the output");
        return None;
    };

    let Some(commit) = with_renderer_surface_state(&surface, |state| state.current_commit()) else {
        miss(2, "the wallpaper layer has no renderer state");
        return None;
    };
    let key = SourceKey {
        surface: surface.id(),
        commit,
        tint,
        dark,
    };

    let mut sources = SOURCES.lock().unwrap();
    let sources = sources.get_or_insert_with(HashMap::new);
    if let Some(source) = sources.get(name).filter(|source| source.key == key) {
        return Some((source.generation, source.size, source.pixels.clone()));
    }

    let started = std::time::Instant::now();
    let reduced = with_renderer_surface_state(&surface, |state| {
        let Some(buffer) = state.buffer() else {
            miss(3, "the wallpaper layer has no buffer");
            return None;
        };
        match with_buffer_contents(buffer, |ptr, len, data| unsafe {
            reduce(std::slice::from_raw_parts(ptr, len), data)
        }) {
            Ok(reduced) => reduced,
            Err(err) => {
                miss(
                    4,
                    &format!("the wallpaper buffer is not readable shm ({err:?})"),
                );
                None
            }
        }
    })
    .flatten()?;

    let (size, pixels) = build(reduced, tint, dark);
    LAST_MISS.store(0, Ordering::Relaxed);
    info!(
        output = name,
        width = size.w,
        height = size.h,
        elapsed = ?started.elapsed(),
        "Mica: picture built"
    );
    let source = Source {
        key,
        generation: GENERATION.fetch_add(1, Ordering::Relaxed) + 1,
        size,
        pixels: Arc::new(pixels),
    };
    let result = (source.generation, source.size, source.pixels.clone());
    sources.insert(name.to_string(), source);
    Some(result)
}

/// The shm buffer reduced to at most `SOURCE_SIDE` pixels on its longer side, by averaging
/// blocks. Every block is sampled on a sparse grid: the result is blurred far beyond what the
/// skipped pixels could change.
fn reduce(pool: &[u8], data: BufferData) -> Option<image::RgbImage> {
    let decode: fn(u32) -> [f32; 3] = match data.format {
        wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => |p| {
            [
                ((p >> 16) & 0xff) as f32 / 255.0,
                ((p >> 8) & 0xff) as f32 / 255.0,
                (p & 0xff) as f32 / 255.0,
            ]
        },
        wl_shm::Format::Xrgb2101010 | wl_shm::Format::Argb2101010 => |p| {
            [
                ((p >> 20) & 0x3ff) as f32 / 1023.0,
                ((p >> 10) & 0x3ff) as f32 / 1023.0,
                (p & 0x3ff) as f32 / 1023.0,
            ]
        },
        format => {
            miss(
                5,
                &format!("wallpaper buffer format {format:?} is not supported"),
            );
            return None;
        }
    };

    let (width, height) = (data.width.max(0) as usize, data.height.max(0) as usize);
    let (offset, stride) = (data.offset.max(0) as usize, data.stride.max(0) as usize);
    if width == 0 || height == 0 || offset + stride * (height - 1) + width * 4 > pool.len() {
        miss(6, "the wallpaper buffer lies outside its pool");
        return None;
    }

    let factor = width.max(height).div_ceil(SOURCE_SIDE).max(1);
    let (out_w, out_h) = ((width / factor).max(1), (height / factor).max(1));
    let step = (factor / 4).max(1);
    let mut out = image::RgbImage::new(out_w as u32, out_h as u32);
    for oy in 0..out_h {
        for ox in 0..out_w {
            let mut sum = [0.0f32; 3];
            let mut count = 0.0f32;
            for y in (oy * factor..((oy + 1) * factor).min(height)).step_by(step) {
                let row = offset + y * stride;
                for x in (ox * factor..((ox + 1) * factor).min(width)).step_by(step) {
                    let at = row + x * 4;
                    let p = u32::from_le_bytes(pool[at..at + 4].try_into().unwrap());
                    let [r, g, b] = decode(p);
                    sum[0] += r;
                    sum[1] += g;
                    sum[2] += b;
                    count += 1.0;
                }
            }
            let to_u8 = |c: f32| (c / count * 255.0).round() as u8;
            out.put_pixel(
                ox as u32,
                oy as u32,
                image::Rgb([to_u8(sum[0]), to_u8(sum[1]), to_u8(sum[2])]),
            );
        }
    }
    Some(out)
}

/// Blurs the reduced wallpaper and tints it the way Windows 11 builds Mica: the luminosity of
/// the tint over the hue and saturation of the wallpaper, then the tint itself on top.
/// Returns RGBA bytes for `Fourcc::Abgr8888`.
fn build(reduced: image::RgbImage, tint: [u8; 4], dark: bool) -> (Size<i32, Buffer>, Vec<u8>) {
    let sigma = (reduced.width() as f32 * BLUR_SHARE).max(1.0);
    let blurred = image::imageops::blur(&reduced, sigma);
    let tint_rgb = [tint[0], tint[1], tint[2]].map(|c| c as f32 / 255.0);
    let tint_opacity = if dark {
        TINT_OPACITY_DARK
    } else {
        TINT_OPACITY_LIGHT
    };

    let mut pixels = Vec::with_capacity(blurred.len() / 3 * 4);
    for pixel in blurred.pixels() {
        let wallpaper = pixel.0.map(|c| c as f32 / 255.0);
        let blended = set_lum(wallpaper, lum(tint_rgb));
        for i in 0..3 {
            let c = blended[i] + (tint_rgb[i] - blended[i]) * tint_opacity;
            pixels.push((c.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        pixels.push(255);
    }
    (
        Size::from((blurred.width() as i32, blurred.height() as i32)),
        pixels,
    )
}

// Luminosity blend, as the W3C compositing spec defines it.
fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    let c = c.map(|v| v + d);
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        c.map(|v| l + (v - l) * l / (l - n))
    } else if x > 1.0 {
        c.map(|v| l + (v - l) * (1.0 - l) / (x - l))
    } else {
        c
    }
}

/// A window theme whose title bar leaves its background to Mica: the plain background with
/// no alpha. Text and icon colours keep its colours. `transparent` is left alone - set, it
/// would make the title bar's `IcedElement` add a live blur under it every frame. No-op while
/// Mica is off.
pub fn window_theme(theme: &mut cosmic::Theme) {
    if !enabled() {
        return;
    }
    let mut cosmic = theme.cosmic().clone();
    cosmic.background_mut(false).base.alpha = 0.0;
    theme.theme_type = ThemeType::Custom(Arc::new(cosmic));
}

/// Mica under a title bar at `geometry` (logical, relative to the output being rendered).
/// The crop of the output's picture while the window is active and a picture is there, the
/// flat theme background otherwise.
#[allow(clippy::too_many_arguments)]
pub fn element<R: AsGlowRenderer>(
    renderer: &mut R,
    id: &Id,
    geometry: Rectangle<f64, Logical>,
    scale: Scale<f64>,
    radii: [u8; 4],
    active: bool,
    alpha: f32,
    theme: &cosmic::Theme,
) -> Option<MicaElement> {
    let current = active
        .then(|| {
            CURRENT.with(|current| {
                current
                    .borrow()
                    .as_ref()
                    .map(|c| (c.texture.clone(), c.generation, c.size))
            })
        })
        .flatten();

    let (texture, generation, src) = match current {
        Some((texture, generation, output)) => {
            let t = texture.size().to_f64();
            let src = Rectangle::<f64, Buffer>::new(
                (
                    geometry.loc.x / output.w * t.w,
                    geometry.loc.y / output.h * t.h,
                )
                    .into(),
                (
                    geometry.size.w / output.w * t.w,
                    geometry.size.h / output.h * t.h,
                )
                    .into(),
            );
            (texture, generation, src)
        }
        None => {
            let (texture, generation) = flat(renderer, tint(theme).0)?;
            (texture, generation, Rectangle::from_size((1.0, 1.0).into()))
        }
    };

    // Texture coordinates of the crop to `0..1` across the title bar, for the clipping
    // shader's rounded corners.
    let t = texture.size().to_f64();
    let input_to_geo = Mat3::from(
        Affine2::from_scale(Vec2::new(
            (t.w / src.size.w) as f32,
            (t.h / src.size.h) as f32,
        )) * Affine2::from_translation(-Vec2::new(
            (src.loc.x / t.w) as f32,
            (src.loc.y / t.h) as f32,
        )),
    );
    let uniforms = vec![
        Uniform::new("geo_size", (geometry.size.w as f32, geometry.size.h as f32)),
        Uniform::new("corner_radius", radii.map(|r| r as f32)),
        Uniform::new(
            "input_to_geo",
            UniformValue::Matrix3x3 {
                matrices: vec![*AsRef::<[f32; 9]>::as_ref(&input_to_geo)],
                transpose: false,
            },
        ),
        Uniform::new("noise", UniformValue::_1f(0.0)),
    ];

    Some(MicaElement {
        id: id.clone(),
        commit: CommitCounter::from(generation * 2 + active as usize),
        texture,
        src,
        geometry: geometry.to_physical_precise_round(scale),
        alpha,
        program: ClippingShader::get(renderer),
        uniforms,
    })
}

/// The 1x1 texture of the flat colour, with its generation.
fn flat<R: AsGlowRenderer>(renderer: &mut R, color: [u8; 4]) -> Option<(GlesTexture, usize)> {
    let glow = renderer.glow_renderer_mut();
    let cached = {
        let gles: &GlesRenderer = (*glow).borrow();
        let user_data = gles.egl_context().user_data();
        user_data.insert_if_missing(MicaTextures::default);
        user_data
            .get::<MicaTextures>()?
            .flat
            .borrow()
            .as_ref()
            .filter(|(cached, _, _)| *cached == color)
            .map(|(_, generation, texture)| (texture.clone(), *generation))
    };
    if cached.is_some() {
        return cached;
    }

    let texture = match glow.import_memory(&color, Fourcc::Abgr8888, (1, 1).into(), false) {
        Ok(texture) => texture,
        Err(err) => {
            warn!(?err, "Failed to upload the flat title bar colour");
            return None;
        }
    };
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let gles: &GlesRenderer = (*glow).borrow();
    if let Some(textures) = gles.egl_context().user_data().get::<MicaTextures>() {
        *textures.flat.borrow_mut() = Some((color, generation, texture.clone()));
    }
    Some((texture, generation))
}

/// A crop of a Mica picture (or the flat colour) under one title bar, with its top corners
/// rounded like the title bar's.
pub struct MicaElement {
    id: Id,
    commit: CommitCounter,
    texture: GlesTexture,
    src: Rectangle<f64, Buffer>,
    geometry: Rectangle<i32, Physical>,
    alpha: f32,
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
}

impl Element for MicaElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.src
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }
}

impl RenderElement<GlowRenderer> for MicaElement {
    fn draw(
        &self,
        frame: &mut GlowFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), GlesError> {
        BorrowMut::<GlesFrame>::borrow_mut(frame)
            .override_default_tex_program(self.program.clone(), self.uniforms.clone());
        let result = frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            self.alpha,
        );
        BorrowMut::<GlesFrame>::borrow_mut(frame).clear_tex_program_override();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(width: i32, height: i32, pixel: u32) -> (Vec<u8>, BufferData) {
        let stride = width * 4 + 8;
        let mut pool = vec![0u8; (stride * height) as usize];
        for y in 0..height {
            for x in 0..width {
                let at = (y * stride + x * 4) as usize;
                pool[at..at + 4].copy_from_slice(&pixel.to_le_bytes());
            }
        }
        let data = BufferData {
            offset: 0,
            width,
            height,
            stride,
            format: wl_shm::Format::Xrgb8888,
        };
        (pool, data)
    }

    #[test]
    fn reduce_keeps_a_flat_colour_and_the_aspect() {
        let (pool, data) = buffer(1920, 1080, 0x00_40_80_c0);
        let reduced = reduce(&pool, data).unwrap();
        assert_eq!((reduced.width(), reduced.height()), (240, 135));
        assert!(reduced.pixels().all(|p| p.0 == [0x40, 0x80, 0xc0]));
    }

    #[test]
    fn reduce_refuses_a_buffer_past_the_pool() {
        let (pool, mut data) = buffer(64, 64, 0);
        data.height = 65;
        assert!(reduce(&pool, data).is_none());
    }

    #[test]
    fn reduce_reads_ten_bit_pixels() {
        let (pool, mut data) = buffer(32, 32, (1023 << 20) | (512 << 10));
        data.format = wl_shm::Format::Xrgb2101010;
        let reduced = reduce(&pool, data).unwrap();
        assert_eq!(reduced.get_pixel(0, 0).0, [255, 128, 0]);
    }

    #[test]
    fn set_lum_takes_the_luminosity_and_keeps_the_hue() {
        let blue = set_lum([0.0, 0.0, 1.0], 0.125);
        assert!((lum(blue) - 0.125).abs() < 1e-4);
        assert!(blue[2] > blue[0] && blue[2] > blue[1]);
    }

    #[test]
    fn dark_mica_stays_close_to_the_tint() {
        let reduced = image::RgbImage::from_pixel(16, 9, image::Rgb([30, 90, 200]));
        let (size, pixels) = build(reduced, [0x20, 0x20, 0x20, 255], true);
        assert_eq!((size.w, size.h), (16, 9));
        assert_eq!(pixels.len(), 16 * 9 * 4);
        let [r, g, b, a] = pixels[..4] else {
            unreachable!()
        };
        assert_eq!(a, 255);
        // 80 % tint: near #202020, tinted toward the wallpaper's blue. Gives #1A212E; the
        // Windows 11 title bar over a blue wallpaper measures #1A202A (ref/w11).
        for c in [r, g, b] {
            assert!((0x10..=0x30).contains(&c), "{r} {g} {b}");
        }
        assert!(b > r);
    }
}
