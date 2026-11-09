//! Loaded images: decoding metadata and the per-page image store.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use std::borrow::Cow;
use std::sync::{OnceLock, RwLock};

use anyhow::{Context as _, Result, bail};
use gpui_kit::{
    AnyElement, AssetSource, Image, ImageFormat, IntoElement, ObjectFit, ParentElement, RenderImage, SharedString,
    Img, Pixels, Styled, StyledImage, div, img, px, relative,
};
use url::Url;

#[derive(Clone)]
pub enum ImageState {
    Loading,
    /// Intrinsic size in image pixels; `padded`: see [`Prepared::padded`].
    Ready { image: Arc<Image>, padded: Option<Arc<RenderImage>>, width: f32, height: f32 },
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
    pub width: f32,
    pub height: f32,
}

impl Prepared {
    pub fn into_state(self) -> ImageState {
        ImageState::Ready { image: self.image, padded: self.padded, width: self.width, height: self.height }
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
    picture_fitted(image, padded, width, height, corners, style::ObjectFit::Fill)
}

/// The same, fitted per CSS `object-fit` (cropped by `cover`).
pub fn picture_fitted(
    image: &Arc<Image>,
    padded: &Option<Arc<RenderImage>>,
    width: f32,
    height: f32,
    corners: [Pixels; 4],
    fit: style::ObjectFit,
) -> AnyElement {
    let round = |i: Img| i.rounded_tl(corners[0]).rounded_tr(corners[1]).rounded_br(corners[2]).rounded_bl(corners[3]);
    let fit = match fit {
        style::ObjectFit::Fill => None,
        style::ObjectFit::Contain => Some(ObjectFit::Contain),
        style::ObjectFit::Cover => Some(ObjectFit::Cover),
        style::ObjectFit::None => Some(ObjectFit::None),
        style::ObjectFit::ScaleDown => Some(ObjectFit::ScaleDown),
    };
    if let Some(fit) = fit {
        return round(img(image.clone())).size_full().object_fit(fit).into_any_element();
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
        _ => round(img(image.clone())).size_full().object_fit(ObjectFit::Fill).into_any_element(),
    }
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
            other => bail!("unsupported image format {other:?}"),
        };
        let size = imagesize::blob_size(&bytes).context("can't read image size")?;
        (format, (size.width as f32, size.height as f32))
    };
    if width <= 0.0 || height <= 0.0 {
        bail!("empty image");
    }
    let padded = padded(&bytes, format);
    Ok(Prepared { image: Arc::new(Image::from_bytes(format, bytes)), padded, width, height })
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
