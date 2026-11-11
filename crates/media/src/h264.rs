//! H.264 to RGBA pictures (openh264), from samples as MP4 stores them
//! (length-prefixed NAL units), in presentation order.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use anyhow::{Result, anyhow};

use crate::VideoFrame;

#[derive(PartialEq, PartialOrd, Clone, Copy)]
struct OrdF64(f64);
impl Eq for OrdF64 {}
#[allow(clippy::derive_ord_xor_partial_ord)]
impl Ord for OrdF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

pub(crate) struct H264 {
    decoder: openh264::decoder::Decoder,
    /// The parameter sets (SPS, PPS), as Annex B.
    sets: Vec<u8>,
    /// Bytes in each NAL unit's length prefix (`avcC`).
    pub length_size: usize,
    /// Times of samples sent but not yet out (B-frames come out after the
    /// frames they reference; the earliest pending time is the next
    /// picture's).
    pending: BinaryHeap<Reverse<OrdF64>>,
    /// Pictures still held when the samples ran out.
    flushed: Vec<VideoFrame>,
    drained: bool,
    /// The hardware decoder, when it takes the stream (macOS).
    #[cfg(target_os = "macos")]
    hard: Option<crate::vt::Session>,
    /// The stream's `avcC` (to start the hardware decoder again).
    avcc: Vec<u8>,
    /// The hardware decoder's pictures, in decode order: shown by time once
    /// enough are in that none decoded later can come before them.
    held: Vec<VideoFrame>,
    /// HEVC (H.265): the hardware decoder only.
    hevc: bool,
}

/// Pictures held to put the hardware decoder's in presentation order
/// (B-frames come out after the frames they reference).
const REORDER: usize = 3;

/// `avcC`'s parameter sets (as Annex B) and its length prefix size.
pub(crate) fn avc_config(avcc: &[u8]) -> Option<(Vec<u8>, usize)> {
    let length_size = usize::from(*avcc.get(4)? & 3) + 1;
    let mut at = 5;
    let mut annex_b = Vec::new();
    for count_mask in [0x1f, 0xff] {
        let count = usize::from(*avcc.get(at)? & count_mask);
        at += 1;
        for _ in 0..count {
            let len = usize::from(u16::from_be_bytes([*avcc.get(at)?, *avcc.get(at + 1)?]));
            at += 2;
            annex_b.extend_from_slice(&[0, 0, 0, 1]);
            annex_b.extend_from_slice(avcc.get(at..at + len)?);
            at += len;
        }
    }
    Some((annex_b, length_size))
}

/// Length-prefixed NAL units as Annex B (start codes).
fn to_annex_b(data: &[u8], length_size: usize, out: &mut Vec<u8>) {
    out.clear();
    let mut at = 0;
    while at + length_size <= data.len() {
        let len = data[at..at + length_size].iter().fold(0usize, |n, &b| (n << 8) | usize::from(b));
        at += length_size;
        let end = (at + len).min(data.len());
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[at..end]);
        at = end;
    }
}

/// Whether a sample (length-prefixed NAL units) starts a picture decodable
/// on its own (an IDR slice).
pub(crate) fn is_keyframe(data: &[u8], length_size: usize) -> bool {
    let mut at = 0;
    while at + length_size < data.len() {
        let len = data[at..at + length_size].iter().fold(0usize, |n, &b| (n << 8) | usize::from(b));
        at += length_size;
        if data[at] & 0x1f == 5 {
            return true;
        }
        at += len;
    }
    false
}

fn decoder(sets: &[u8]) -> Result<openh264::decoder::Decoder> {
    // No flush per decode: reordered pictures must be kept.
    let config = openh264::decoder::DecoderConfig::new().flush_after_decode(openh264::decoder::Flush::NoFlush);
    let mut decoder = openh264::decoder::Decoder::with_api_config(openh264::OpenH264API::from_source(), config).map_err(|e| anyhow!("openh264: {e}"))?;
    if !sets.is_empty() {
        decoder.decode(sets).ok();
    }
    Ok(decoder)
}

fn rgba_of(yuv: &openh264::decoder::DecodedYUV<'_>) -> (u32, u32, Vec<u8>) {
    use openh264::formats::YUVSource;
    let (w, h) = yuv.dimensions();
    let mut rgba = vec![0u8; w * h * 4];
    yuv.write_rgba8(&mut rgba);
    (w as u32, h as u32, rgba)
}

impl H264 {
    /// A decoder for the stream `avcc` (its `avcC` box) describes.
    pub fn new(avcc: Option<&[u8]>) -> Result<H264> {
        let (sets, length_size) = avcc.and_then(avc_config).unwrap_or((Vec::new(), 4));
        let avcc = avcc.map(<[u8]>::to_vec).unwrap_or_default();
        Ok(H264 {
            decoder: decoder(&sets)?,
            sets,
            length_size,
            pending: BinaryHeap::new(),
            flushed: Vec::new(),
            drained: false,
            #[cfg(target_os = "macos")]
            hard: hardware(&avcc),
            avcc,
            held: Vec::new(),
            hevc: false,
        })
    }

    /// An HEVC decoder for the stream `hvcc` (its `hvcC` box) describes:
    /// VideoToolbox's (there's no software one).
    #[cfg(target_os = "macos")]
    pub fn hevc(hvcc: &[u8]) -> Result<H264> {
        let hard = crate::vt::Session::new_hevc(hvcc)?;
        let length_size = usize::from(*hvcc.get(21).unwrap_or(&3) & 3) + 1;
        Ok(H264 {
            decoder: decoder(&[])?,
            sets: Vec::new(),
            length_size,
            pending: BinaryHeap::new(),
            flushed: Vec::new(),
            drained: false,
            hard: Some(hard),
            avcc: hvcc.to_vec(),
            held: Vec::new(),
            hevc: true,
        })
    }

    /// A picture already decoded and due out before the next sample's (the
    /// hardware decoder can give several for one sample).
    pub fn ready(&mut self) -> Option<VideoFrame> {
        self.next_held(REORDER)
    }

    /// The earliest held picture, if `enough` are held (or any, draining).
    fn next_held(&mut self, enough: usize) -> Option<VideoFrame> {
        if self.held.len() <= enough {
            return None;
        }
        let i = (0..self.held.len()).min_by(|&a, &b| self.held[a].time.total_cmp(&self.held[b].time))?;
        Some(self.held.swap_remove(i))
    }

    /// Decode one sample shown at `time`; a picture, when one is ready
    /// (with the earliest time still owed).
    pub fn decode(&mut self, data: &[u8], time: f64) -> Option<VideoFrame> {
        #[cfg(target_os = "macos")]
        if let Some(hard) = &mut self.hard {
            let started = std::time::Instant::now();
            let out = hard.decode(data, time);
            if std::env::var_os("MEDIA_DEBUG").is_some() {
                thread_local!(static SPENT: std::cell::Cell<(u32, f64)> = const { std::cell::Cell::new((0, 0.0)) });
                let (n, total) = SPENT.with(|s| s.get());
                let (n, total) = (n + 1, total + started.elapsed().as_secs_f64());
                if n == 120 {
                    eprintln!("media: videotoolbox {:.2}ms a sample", total / 120.0 * 1000.0);
                    SPENT.with(|s| s.set((0, 0.0)));
                } else {
                    SPENT.with(|s| s.set((n, total)));
                }
            }
            for (time, width, height, bgra) in out {
                if std::env::var_os("MEDIA_DEBUG").is_some() {
                    thread_local!(static SIZE: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) });
                    if SIZE.with(|s| s.replace((width, height))) != (width, height) {
                        eprintln!("media: videotoolbox now {width}x{height} at {time:.3}s");
                    }
                }
                self.held.push(VideoFrame { width, height, rgba: bgra, time, bgra: true });
            }
            return self.next_held(REORDER);
        }
        self.pending.push(Reverse(OrdF64(time)));
        let mut annex_b = Vec::with_capacity(data.len() + 16);
        to_annex_b(data, self.length_size, &mut annex_b);
        let mut decoded = None;
        // One NAL unit at a time, as openh264 takes them.
        for nal in openh264::nal_units(&annex_b) {
            match self.decoder.decode(nal) {
                Ok(Some(yuv)) => decoded = Some(rgba_of(&yuv)),
                Ok(None) => {}
                // A broken unit: skip it.
                Err(e) => {
                    if std::env::var_os("MEDIA_DEBUG").is_some() {
                        eprintln!("media: h264 sample at {time:.3}s: {e}");
                    }
                }
            }
        }
        let (width, height, rgba) = decoded?;
        if std::env::var_os("MEDIA_DEBUG").is_some() {
            thread_local!(static SIZE: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) });
            if SIZE.with(|s| s.replace((width, height))) != (width, height) {
                eprintln!("media: h264 now {width}x{height} at {time:.3}s");
            }
        }
        let time = self.pending.pop().map_or(time, |Reverse(t)| t.0);
        Some(VideoFrame { width, height, rgba, time, bgra: false })
    }

    /// The pictures still held, one by one, once the samples run out.
    pub fn drain(&mut self) -> Option<VideoFrame> {
        #[cfg(target_os = "macos")]
        if let Some(hard) = &mut self.hard {
            if !self.drained {
                self.drained = true;
                for (time, width, height, bgra) in hard.flush() {
                    self.held.push(VideoFrame { width, height, rgba: bgra, time, bgra: true });
                }
            }
            return self.next_held(0);
        }
        if !self.drained {
            self.drained = true;
            if let Ok(rest) = self.decoder.flush_remaining() {
                for yuv in rest {
                    let (width, height, rgba) = rgba_of(&yuv);
                    let time = self.pending.pop().map_or(0.0, |Reverse(t)| t.0);
                    self.flushed.push(VideoFrame { width, height, rgba, time, bgra: false });
                }
                self.flushed.reverse();
            }
        }
        self.flushed.pop()
    }

    /// Start over (a seek): nothing held from before.
    pub fn reset(&mut self) -> Result<()> {
        self.decoder = decoder(&self.sets)?;
        self.pending.clear();
        self.flushed.clear();
        self.held.clear();
        self.drained = false;
        #[cfg(target_os = "macos")]
        if self.hevc {
            self.hard = crate::vt::Session::new_hevc(&self.avcc).ok();
        } else if self.hard.is_some() {
            self.hard = hardware(&self.avcc);
        }
        Ok(())
    }

    /// The stream changed its parameters (another quality): decode with
    /// these from the next keyframe.
    pub fn reconfigure(&mut self, avcc: &[u8]) -> Result<()> {
        if let Some((sets, length_size)) = avc_config(avcc) {
            self.sets = sets;
            self.length_size = length_size;
        }
        self.avcc = avcc.to_vec();
        #[cfg(target_os = "macos")]
        if !self.hevc {
            self.hard = hardware(&self.avcc);
        }
        self.reset()
    }
}

/// The hardware decoder for `avcc`, if it'll take it.
#[cfg(target_os = "macos")]
fn hardware(avcc: &[u8]) -> Option<crate::vt::Session> {
    if avcc.is_empty() {
        return None;
    }
    crate::vt::Session::new(avcc).map_err(|e| if std::env::var_os("MEDIA_DEBUG").is_some() { eprintln!("media: no hardware decoding: {e}") }).ok()
}
