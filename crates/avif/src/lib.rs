//! AVIF pictures, still and animated, decoded with libavif (for the
//! renderer and for canvases) (libaom's AV1
//! decoder; both built from source by `libavif-sys`, with CMake).
//!
//! libavif is C: the unsafe calls are kept to two small owners, [`Decoder`]
//! and [`Rgb`], which free what libavif allocated when they're dropped.

use libavif_sys as sys;

/// Every frame of an AVIF, BGRA with straight alpha, with its delay.
pub fn decode(bytes: &[u8]) -> Option<Vec<image::Frame>> {
    let decoder = Decoder::new(bytes)?;
    let mut out = Vec::new();
    while decoder.next() {
        let rgb = Rgb::of(&decoder)?;
        let buffer = image::RgbaImage::from_raw(rgb.width, rgb.height, rgb.pixels())?;
        let ms = (decoder.frame_duration() * 1000.0).round().max(10.0) as u32;
        out.push(image::Frame::from_parts(buffer, 0, 0, image::Delay::from_numer_denom_ms(ms, 1)));
    }
    (!out.is_empty()).then_some(out)
}

/// A libavif decoder reading `'a` bytes.
struct Decoder<'a> {
    raw: *mut sys::avifDecoder,
    _bytes: std::marker::PhantomData<&'a [u8]>,
}

impl<'a> Decoder<'a> {
    /// A decoder over `bytes`, its container parsed.
    fn new(bytes: &'a [u8]) -> Option<Self> {
        // SAFETY: a fresh decoder (checked for null), owned by `Self` from
        // here on, so dropped on every path.
        let decoder = Decoder { raw: unsafe { sys::avifDecoderCreate() }, _bytes: std::marker::PhantomData };
        if decoder.raw.is_null() {
            return None;
        }
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        // SAFETY: `raw` is a live decoder; libavif reads `bytes` only while
        // decoding, and `'a` keeps them alive as long as the decoder.
        unsafe {
            (*decoder.raw).maxThreads = threads as i32;
            // A page's picture, not a camera's: 64 megapixels, 1000 frames.
            (*decoder.raw).imageSizeLimit = 64 << 20;
            (*decoder.raw).imageCountLimit = 1000;
            if sys::avifDecoderSetIOMemory(decoder.raw, bytes.as_ptr(), bytes.len()) != sys::AVIF_RESULT_OK
                || sys::avifDecoderParse(decoder.raw) != sys::AVIF_RESULT_OK
            {
                return None;
            }
        }
        Some(decoder)
    }

    /// Decode the next frame; false when there are no more.
    fn next(&self) -> bool {
        // SAFETY: `raw` is a live, parsed decoder.
        unsafe { sys::avifDecoderNextImage(self.raw) == sys::AVIF_RESULT_OK }
    }

    /// How long the frame just decoded shows, seconds.
    fn frame_duration(&self) -> f64 {
        // SAFETY: `raw` is a live decoder.
        unsafe { (*self.raw).imageTiming.duration }
    }
}

impl Drop for Decoder<'_> {
    fn drop(&mut self) {
        // SAFETY: `raw` came from `avifDecoderCreate` and is destroyed once.
        unsafe { sys::avifDecoderDestroy(self.raw) }
    }
}

/// A decoded frame converted to 8-bit BGRA, in a buffer libavif allocated.
struct Rgb {
    raw: sys::avifRGBImage,
    width: u32,
    height: u32,
}

impl Rgb {
    /// The frame `decoder` decoded last, converted.
    fn of(decoder: &Decoder) -> Option<Self> {
        let mut raw = sys::avifRGBImage::default();
        // SAFETY: `decoder.raw.image` is the frame just decoded; the pixels
        // allocated here are owned by `Self` (freed when dropped).
        unsafe {
            let picture = (*decoder.raw).image;
            sys::avifRGBImageSetDefaults(&mut raw, picture);
            raw.format = sys::AVIF_RGB_FORMAT_BGRA;
            raw.depth = 8;
            if sys::avifRGBImageAllocatePixels(&mut raw) != sys::AVIF_RESULT_OK {
                return None;
            }
            let mut rgb = Rgb { raw, width: raw.width, height: raw.height };
            (sys::avifImageYUVToRGB(picture, &mut rgb.raw) == sys::AVIF_RESULT_OK).then_some(rgb)
        }
    }

    /// The pixels, rows packed (libavif may pad them).
    fn pixels(&self) -> Vec<u8> {
        let (w, h, row) = (self.width as usize, self.height as usize, self.raw.rowBytes as usize);
        // SAFETY: libavif allocated `rowBytes * height` bytes at `pixels`,
        // alive until `self` is dropped.
        let src = unsafe { std::slice::from_raw_parts(self.raw.pixels, row * h) };
        src.chunks_exact(row).flat_map(|r| &r[..w * 4]).copied().collect()
    }
}

impl Drop for Rgb {
    fn drop(&mut self) {
        // SAFETY: the pixels were allocated by `avifRGBImageAllocatePixels`
        // and are freed once.
        unsafe { sys::avifRGBImageFreePixels(&mut self.raw) }
    }
}
