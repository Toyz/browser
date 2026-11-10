//! Loaded images: decoding metadata and the per-page image store.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use std::borrow::Cow;
use std::sync::{OnceLock, RwLock};

use anyhow::{Context as _, Result, bail};
use gpui_kit::{
    AnyElement, AssetSource, Image, ImageFormat, ImageSource, InteractiveElement, IntoElement, ObjectFit, ParentElement, RenderImage, SharedString,
    Img, Pixels, Styled, StyledImage, div, img, px, relative,
};
use url::Url;

#[derive(Clone)]
pub enum ImageState {
    Loading,
    /// Intrinsic size in image pixels; `padded`: see [`Prepared::padded`].
    /// `frames`: an animation's frames, decoded (see [`Prepared::frames`]).
    Ready { image: Arc<Image>, padded: Option<Arc<RenderImage>>, frames: Option<Arc<RenderImage>>, width: f32, height: f32 },
    Failed,
}

/// A downloaded image, ready to draw.
pub struct Prepared {
    pub image: Arc<Image>,
    /// Raster images decoded here, with a 1px border repeating their edge
    /// pixels. GPUI samples images from a texture atlas with linear
    /// filtering; scaled up (every image on a 2x display), the outermost
    /// pixels blend with the atlas's transparent padding into a faint grey
    /// frame. Drawn a source pixel larger and clipped, the blur falls in
    /// the repeated border instead.
    pub padded: Option<Arc<RenderImage>>,
    /// GIFs and animated WebPs, decoded here: every frame (BGRA) with its
    /// delay. GPUI's own decoding of an encoded WebP keeps only the first
    /// frame, and decoded frames can be filtered (CSS `filter`).
    pub frames: Option<Arc<RenderImage>>,
    pub width: f32,
    pub height: f32,
}

impl Prepared {
    pub fn into_state(self) -> ImageState {
        ImageState::Ready { image: self.image, padded: self.padded, frames: self.frames, width: self.width, height: self.height }
    }
}

/// Decode and pad a raster image (not GIFs: they may animate).
fn padded(bytes: &[u8], format: ImageFormat) -> Option<Arc<RenderImage>> {
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Webp | ImageFormat::Bmp | ImageFormat::Ico) {
        return None;
    }
    let src = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = src.dimensions();
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    let mut out = image::RgbaImage::new(w + 2, h + 2);
    for (x, y, px) in out.enumerate_pixels_mut() {
        let sx = x.saturating_sub(1).min(w - 1);
        let sy = y.saturating_sub(1).min(h - 1);
        let p = src.get_pixel(sx, sy).0;
        // GPUI wants BGRA.
        *px = image::Rgba([p[2], p[1], p[0], p[3]]);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(out)])))
}

/// Draw a loaded image filling its parent (padded when possible).
pub fn picture(image: &Arc<Image>, padded: &Option<Arc<RenderImage>>, width: f32, height: f32) -> AnyElement {
    picture_rounded(image, padded, width, height, [px(0.); 4])
}

/// The same with rounded corners (top-left, top-right, bottom-right,
/// bottom-left).
pub fn picture_rounded(image: &Arc<Image>, padded: &Option<Arc<RenderImage>>, width: f32, height: f32, corners: [Pixels; 4]) -> AnyElement {
    picture_fitted(image.clone(), padded, width, height, corners, style::ObjectFit::Fill, None)
}

/// The same, fitted per CSS `object-fit` (cropped by `cover`).
/// The middle of a padded raster (see [`Prepared::padded`]) that
/// `object-fit: cover` shows in a box of aspect `ratio` (width / height),
/// cached per picture and ratio.
fn cover_crop(padded: &Arc<RenderImage>, ratio: f32) -> Option<Arc<RenderImage>> {
    thread_local! {
        static CROPS: RefCell<HashMap<(usize, u32), Arc<RenderImage>>> = RefCell::default();
    }
    let key = (Arc::as_ptr(padded) as usize, (ratio * 1000.0).round() as u32);
    if let Some(hit) = CROPS.with_borrow(|c| c.get(&key).cloned()) {
        return Some(hit);
    }
    let size = padded.size(0);
    let (pw, ph) = (size.width.0 as u32, size.height.0 as u32);
    if pw < 3 || ph < 3 || !ratio.is_finite() || ratio <= 0.0 {
        return None;
    }
    // Without the 1px border the padding added.
    let (w, h) = (pw - 2, ph - 2);
    let (cw, ch) = if (w as f32 / h as f32) > ratio {
        (((h as f32) * ratio).round().max(1.0) as u32, h)
    } else {
        (w, ((w as f32) / ratio).round().max(1.0) as u32)
    };
    let (x0, y0) = (1 + (w - cw.min(w)) / 2, 1 + (h - ch.min(h)) / 2);
    let bytes = padded.as_bytes(0)?;
    let mut out = image::RgbaImage::new(cw, ch);
    for y in 0..ch {
        for x in 0..cw {
            let i = (((y0 + y) * pw + (x0 + x)) * 4) as usize;
            out.put_pixel(x, y, image::Rgba([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]));
        }
    }
    let cropped = Arc::new(RenderImage::new(vec![image::Frame::new(out)]));
    CROPS.with_borrow_mut(|c| {
        if c.len() > 256 {
            c.clear();
        }
        c.insert(key, cropped.clone());
    });
    Some(cropped)
}

/// The middle of each frame that `object-fit: cover` shows in a box of
/// aspect `ratio`, with the frames' delays; cached per picture and ratio.
fn cover_crop_frames(frames: &Arc<RenderImage>, ratio: f32) -> Option<Arc<RenderImage>> {
    thread_local! {
        static CROPS: RefCell<HashMap<(usize, u32), Arc<RenderImage>>> = RefCell::default();
    }
    let key = (Arc::as_ptr(frames) as usize, (ratio * 1000.0).round() as u32);
    if let Some(hit) = CROPS.with_borrow(|c| c.get(&key).cloned()) {
        return Some(hit);
    }
    if !ratio.is_finite() || ratio <= 0.0 {
        return None;
    }
    let mut out = Vec::new();
    for i in 0..frames.frame_count() {
        let size = frames.size(i);
        let (w, h) = (size.width.0 as u32, size.height.0 as u32);
        if w == 0 || h == 0 {
            return None;
        }
        let (cw, ch) = if (w as f32 / h as f32) > ratio {
            (((h as f32) * ratio).round().max(1.0) as u32, h)
        } else {
            (w, ((w as f32) / ratio).round().max(1.0) as u32)
        };
        let full = image::RgbaImage::from_raw(w, h, frames.as_bytes(i)?.to_vec())?;
        let cropped = image::imageops::crop_imm(&full, (w - cw.min(w)) / 2, (h - ch.min(h)) / 2, cw.min(w), ch.min(h)).to_image();
        out.push(image::Frame::from_parts(cropped, 0, 0, frames.delay(i)));
    }
    let cropped = Arc::new(RenderImage::new(out));
    CROPS.with_borrow_mut(|c| {
        if c.len() > 128 {
            c.clear();
        }
        c.insert(key, cropped.clone());
    });
    Some(cropped)
}

pub fn picture_fitted(
    image: impl Into<ImageSource>,
    padded: &Option<Arc<RenderImage>>,
    width: f32,
    height: f32,
    corners: [Pixels; 4],
    fit: style::ObjectFit,
    // The box's width / height, when known.
    box_ratio: Option<f32>,
) -> AnyElement {
    let round = |i: Img| i.rounded_tl(corners[0]).rounded_tr(corners[1]).rounded_br(corners[2]).rounded_bl(corners[3]);
    // A rounded `cover` picture: GPUI rounds the scaled-up picture, whose
    // corners are outside the box (so they never show). Draw the part
    // `cover` shows instead, rounded to the box.
    if fit == style::ObjectFit::Cover
        && corners.iter().any(|c| *c > px(0.))
        && let (Some(p), Some(ratio)) = (padded, box_ratio)
        && let Some(cropped) = cover_crop(p, ratio)
    {
        return round(img(cropped)).size_full().object_fit(ObjectFit::Fill).into_any_element();
    }
    let image: ImageSource = image.into();
    // The same for pictures decoded here (animations, AVIF): their frames,
    // cropped.
    if fit == style::ObjectFit::Cover
        && corners.iter().any(|c| *c > px(0.))
        && let (ImageSource::Render(frames), Some(ratio)) = (&image, box_ratio)
        && let Some(cropped) = cover_crop_frames(frames, ratio)
    {
        let source: ImageSource = cropped.into();
        let picture = round(img(source.clone())).size_full().object_fit(ObjectFit::Fill);
        return match &source {
            ImageSource::Render(r) if r.frame_count() > 1 => picture.id(("animation", r.id.0)).into_any_element(),
            _ => picture.into_any_element(),
        };
    }
    let fit = match fit {
        style::ObjectFit::Fill => None,
        style::ObjectFit::Contain => Some(ObjectFit::Contain),
        style::ObjectFit::Cover => Some(ObjectFit::Cover),
        style::ObjectFit::None => Some(ObjectFit::None),
        style::ObjectFit::ScaleDown => Some(ObjectFit::ScaleDown),
    };
    // An animation plays only in an element with state (its frame and
    // clock), which needs an id: the picture's own (the same animation
    // shown twice plays in step).
    let animated = |i: Img, source: &ImageSource| match source {
        ImageSource::Render(r) if r.frame_count() > 1 => i.id(("animation", r.id.0)).into_any_element(),
        _ => i.into_any_element(),
    };
    if let Some(fit) = fit {
        return animated(round(img(image.clone())).size_full().object_fit(fit), &image);
    }
    match padded {
        Some(p) if width > 0.0 && height > 0.0 => div()
            .size_full()
            .relative()
            .overflow_hidden()
            .child(
                round(img(p.clone()))
                    .absolute()
                    .left(relative(-1.0 / width))
                    .top(relative(-1.0 / height))
                    .w(relative((width + 2.0) / width))
                    .h(relative((height + 2.0) / height))
                    .object_fit(ObjectFit::Fill),
            )
            .into_any_element(),
        _ => animated(round(img(image.clone())).size_full().object_fit(ObjectFit::Fill), &image),
    }
}

/// One BGRA pixel (straight alpha) through `filters`' colors.
fn filter_bgra(px: &mut [u8; 4], filters: &[style::Filter]) {
    let c = [px[2] as f32 / 255.0, px[1] as f32 / 255.0, px[0] as f32 / 255.0, px[3] as f32 / 255.0];
    let [r, g, b, a] = filters.iter().fold(c, |c, f| f.color(c));
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    *px = [q(b), q(g), q(r), q(a)];
}

/// A loaded picture as CSS `filter` color functions leave it: the picture
/// to draw and its padded raster (see [`Prepared::padded`]). Unchanged
/// without filters, or for pictures not decoded here (GIFs, SVGs).
pub fn filtered(
    image: &Arc<Image>,
    padded: &Option<Arc<RenderImage>>,
    frames: &Option<Arc<RenderImage>>,
    filters: Option<&[style::Filter]>,
) -> (ImageSource, Option<Arc<RenderImage>>) {
    let colors: Vec<style::Filter> = filters.unwrap_or_default().iter().copied().filter(|f| f.is_color()).collect();
    thread_local! {
        static FILTERED: RefCell<HashMap<(usize, String), (Arc<RenderImage>, Arc<RenderImage>)>> = RefCell::default();
        static ANIMATIONS: RefCell<HashMap<(usize, String), Arc<RenderImage>>> = RefCell::default();
    }
    // An animation: every frame, filtered.
    if let Some(frames) = frames {
        if colors.is_empty() {
            return (frames.clone().into(), None);
        }
        let key = (Arc::as_ptr(frames) as usize, format!("{colors:?}"));
        if let Some(hit) = ANIMATIONS.with_borrow(|c| c.get(&key).cloned()) {
            return (hit.into(), None);
        }
        let out: Vec<image::Frame> = (0..frames.frame_count())
            .filter_map(|i| {
                let size = frames.size(i);
                let mut buf = image::RgbaImage::from_raw(size.width.0 as u32, size.height.0 as u32, frames.as_bytes(i)?.to_vec())?;
                for px in buf.pixels_mut() {
                    filter_bgra(&mut px.0, &colors);
                }
                Some(image::Frame::from_parts(buf, 0, 0, frames.delay(i)))
            })
            .collect();
        let out = Arc::new(RenderImage::new(out));
        ANIMATIONS.with_borrow_mut(|c| {
            if c.len() > 128 {
                c.clear();
            }
            c.insert(key, out.clone());
        });
        return (out.into(), None);
    }
    let Some(p) = padded.as_ref().filter(|_| !colors.is_empty()) else {
        return (image.clone().into(), padded.clone());
    };
    let key = (Arc::as_ptr(p) as usize, format!("{colors:?}"));
    if let Some((plain, padded)) = FILTERED.with_borrow(|c| c.get(&key).cloned()) {
        return (plain.into(), Some(padded));
    }
    let size = p.size(0);
    let (pw, ph) = (size.width.0 as u32, size.height.0 as u32);
    let Some(bytes) = p.as_bytes(0).filter(|_| pw >= 3 && ph >= 3) else {
        return (image.clone().into(), padded.clone());
    };
    // BGRA, straight alpha.
    let Some(mut out) = image::RgbaImage::from_raw(pw, ph, bytes.to_vec()) else {
        return (image.clone().into(), padded.clone());
    };
    for px in out.pixels_mut() {
        filter_bgra(&mut px.0, &colors);
    }
    let plain = image::imageops::crop_imm(&out, 1, 1, pw - 2, ph - 2).to_image();
    let plain = Arc::new(RenderImage::new(vec![image::Frame::new(plain)]));
    let padded = Arc::new(RenderImage::new(vec![image::Frame::new(out)]));
    FILTERED.with_borrow_mut(|c| {
        if c.len() > 256 {
            c.clear();
        }
        c.insert(key, (plain.clone(), padded.clone()));
    });
    (plain.into(), Some(padded))
}

/// Images for the current page, filled in as they arrive.
pub type ImageStore = Rc<RefCell<HashMap<Url, ImageState>>>;

const SVG_PREFIX: &str = "page-svg/";

fn svg_registry() -> &'static RwLock<HashMap<u64, Arc<[u8]>>> {
    static REGISTRY: OnceLock<RwLock<HashMap<u64, Arc<[u8]>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// Make a page's SVG available to GPUI's `svg()` element (which loads by
/// asset path, and draws in the current text color: exactly a CSS mask).
pub fn svg_asset_path(image: &Image) -> SharedString {
    let id = image.id();
    if !svg_registry().read().unwrap().contains_key(&id) {
        svg_registry().write().unwrap().insert(id, image.bytes.clone().into());
    }
    format!("{SVG_PREFIX}{id}.svg").into()
}

/// The app's asset source, plus SVGs registered by pages.
pub struct PageAssets<A>(pub A);

impl<A: AssetSource> AssetSource for PageAssets<A> {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(rest) = path.strip_prefix(SVG_PREFIX) {
            let id: u64 = rest.trim_end_matches(".svg").parse().unwrap_or(0);
            return Ok(svg_registry().read().unwrap().get(&id).map(|b| Cow::Owned(b.to_vec())));
        }
        self.0.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        self.0.list(path)
    }
}

/// Identify the format and intrinsic size of downloaded image bytes. Cheap
/// (header parsing only); GPUI does the actual decoding when it first draws.
pub fn prepare(bytes: Vec<u8>) -> Result<Prepared> {
    let (format, (width, height)) = if looks_like_svg(&bytes) {
        (ImageFormat::Svg, svg_size(&bytes).unwrap_or((300.0, 150.0)))
    } else {
        let format = match imagesize::image_type(&bytes).context("unrecognized image data")? {
            imagesize::ImageType::Png => ImageFormat::Png,
            imagesize::ImageType::Jpeg => ImageFormat::Jpeg,
            imagesize::ImageType::Gif => ImageFormat::Gif,
            imagesize::ImageType::Webp => ImageFormat::Webp,
            imagesize::ImageType::Bmp => ImageFormat::Bmp,
            imagesize::ImageType::Tiff => ImageFormat::Tiff,
            imagesize::ImageType::Ico => ImageFormat::Ico,
            // AVIF: decoded below, by the system (GPUI has no format for it).
            imagesize::ImageType::Heif(imagesize::Compression::Av1) => {
                let frames = crate::avif::decode(&bytes).context("can't decode AVIF")?;
                let (w, h) = (frames[0].buffer().width() as f32, frames[0].buffer().height() as f32);
                let frames = Some(Arc::new(RenderImage::new(frames)));
                return Ok(Prepared { image: Arc::new(Image::from_bytes(ImageFormat::Png, Vec::new())), padded: None, frames, width: w, height: h });
            }
            other => bail!("unsupported image format {other:?}"),
        };
        let size = imagesize::blob_size(&bytes).context("can't read image size")?;
        (format, (size.width as f32, size.height as f32))
    };
    if width <= 0.0 || height <= 0.0 {
        bail!("empty image");
    }
    let frames = animation(&bytes, format, width * height);
    let padded = if frames.is_some() { None } else { padded(&bytes, format) };
    Ok(Prepared { image: Arc::new(Image::from_bytes(format, bytes)), padded, frames, width, height })
}

/// A GIF's or animated WebP's frames, decoded to BGRA (`area`: one
/// frame's, in pixels; very long or large animations are left to GPUI).
fn animation(bytes: &[u8], format: ImageFormat, area: f32) -> Option<Arc<RenderImage>> {
    use image::AnimationDecoder;
    use std::io::Cursor;
    const MAX_PIXELS: f32 = 64.0 * 1024.0 * 1024.0;
    let frames: Box<dyn Iterator<Item = image::ImageResult<image::Frame>>> = match format {
        ImageFormat::Gif => Box::new(image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).ok()?.into_frames()),
        ImageFormat::Webp => {
            let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).ok()?;
            if !decoder.has_animation() {
                return None;
            }
            let _ = decoder.set_background_color(image::Rgba([0, 0, 0, 0]));
            Box::new(decoder.into_frames())
        }
        _ => return None,
    };
    let mut out = Vec::new();
    for frame in frames {
        let Ok(mut frame) = frame else { continue };
        if (out.len() + 1) as f32 * area > MAX_PIXELS {
            return None;
        }
        for px in frame.buffer_mut().chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        out.push(frame);
    }
    (!out.is_empty()).then(|| Arc::new(RenderImage::new(out)))
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    let text = String::from_utf8_lossy(head);
    let t = text.trim_start_matches('\u{feff}').trim_start();
    t.starts_with('<') && text.contains("<svg")
}

/// `width`/`height` attributes of the root `<svg>`, else its viewBox.
fn svg_size(bytes: &[u8]) -> Option<(f32, f32)> {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let start = text.find("<svg")?;
    let tag = &text[start..start + text[start..].find('>')?];
    let attr = |name: &str| -> Option<String> {
        let pattern = format!(" {name}=");
        let i = tag.find(&pattern)? + pattern.len();
        let quote = tag[i..].chars().next()?;
        let rest = &tag[i + 1..];
        Some(rest[..rest.find(quote)?].to_owned())
    };
    let number = |v: String| -> Option<f32> {
        let v = v.trim();
        if v.ends_with('%') {
            return None;
        }
        v.trim_end_matches("px").parse().ok()
    };
    if let (Some(w), Some(h)) = (attr("width").and_then(number), attr("height").and_then(number)) {
        return Some((w, h));
    }
    let view_box = attr("viewBox")?;
    let parts: Vec<f32> = view_box
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    (parts.len() == 4).then(|| (parts[2], parts[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_images_get_an_edge_border() {
        let mut src = image::RgbaImage::new(3, 2);
        src.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        src.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let p = prepare(png.into_inner()).unwrap();
        let padded = p.padded.expect("padded");
        let frame = padded.as_bytes(0).unwrap();
        // 5x4 BGRA; the corner repeats the red top-left pixel (as BGRA).
        assert_eq!(frame.len(), 5 * 4 * 4);
        assert_eq!(&frame[0..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn svg_sizes() {
        assert_eq!(svg_size(br#"<svg xmlns="x" width="18" height="18px">"#), Some((18.0, 18.0)));
        assert_eq!(svg_size(br#"<?xml?><svg viewBox="0 0 24 12">"#), Some((24.0, 12.0)));
        assert_eq!(svg_size(br#"<svg width="100%" viewBox="0,0,8,8">"#), Some((8.0, 8.0)));
    }

    #[test]
    fn prepares_png() {
        // 1×2 transparent PNG
        let png = net::decode_data_url(
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAACCAYAAACZgbYnAAAAD0lEQVR42mNkYGBgZGAAAAAQAAEh4qYfAAAAAElFTkSuQmCC",
        )
        .unwrap();
        let p = prepare(png).unwrap();
        assert_eq!((p.width, p.height), (1.0, 2.0));
        assert!(prepare(b"not an image".to_vec()).is_err());
    }
}
