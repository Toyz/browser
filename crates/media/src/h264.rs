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
}

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
        Ok(H264 { decoder: decoder(&sets)?, sets, length_size, pending: BinaryHeap::new(), flushed: Vec::new(), drained: false })
    }

    /// Decode one sample shown at `time`; a picture, when one is ready
    /// (with the earliest time still owed).
    pub fn decode(&mut self, data: &[u8], time: f64) -> Option<VideoFrame> {
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
        let time = self.pending.pop().map_or(time, |Reverse(t)| t.0);
        Some(VideoFrame { width, height, rgba, time })
    }

    /// The pictures still held, one by one, once the samples run out.
    pub fn drain(&mut self) -> Option<VideoFrame> {
        if !self.drained {
            self.drained = true;
            if let Ok(rest) = self.decoder.flush_remaining() {
                for yuv in rest {
                    let (width, height, rgba) = rgba_of(&yuv);
                    let time = self.pending.pop().map_or(0.0, |Reverse(t)| t.0);
                    self.flushed.push(VideoFrame { width, height, rgba, time });
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
        self.drained = false;
        Ok(())
    }

    /// The stream changed its parameters (another quality): decode with
    /// these from the next keyframe.
    pub fn reconfigure(&mut self, avcc: &[u8]) -> Result<()> {
        if let Some((sets, length_size)) = avc_config(avcc) {
            self.sets = sets;
            self.length_size = length_size;
        }
        self.reset()
    }
}
