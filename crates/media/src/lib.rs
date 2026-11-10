//! Media decoding for `<video>` and `<audio>`: a container (MP4, WebM and
//! Matroska, Ogg, MP3, FLAC, WAV) demuxed by symphonia; H.264 video decoded
//! by openh264 (built from source) to RGBA frames; audio (AAC, Vorbis, MP3,
//! FLAC, PCM) decoded by symphonia to interleaved `f32` samples. Each
//! output carries its presentation time, seconds. [`Player`] plays one:
//! pictures shown when due, sound through the default device.

mod audio;
mod player;
mod source;

pub use player::{Command, Event, Player};
pub use source::{Feeder, Growing, growing};

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io::Cursor;

use anyhow::{Context as _, Result, anyhow};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::video::well_known::CODEC_ID_H264;
use symphonia::core::codecs::video::well_known::extra_data::VIDEO_EXTRA_DATA_ID_AVC_DECODER_CONFIG;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase};

/// A decoded picture.
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    pub time: f64,
}

/// Decoded sound.
pub struct AudioChunk {
    pub rate: u32,
    pub channels: u16,
    /// Interleaved, `channels` per frame.
    pub samples: Vec<f32>,
    pub time: f64,
}

pub enum Output {
    Video(VideoFrame),
    Audio(AudioChunk),
}

/// What a file holds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Info {
    /// Seconds, when the container says.
    pub duration: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub has_video: bool,
    pub has_audio: bool,
}

struct VideoTrack {
    id: u32,
    time_base: Option<TimeBase>,
    decoder: openh264::decoder::Decoder,
    /// The parameter sets (SPS, PPS), as Annex B.
    sets: Vec<u8>,
    /// Bytes in each NAL unit's length prefix (`avcC`).
    length_size: usize,
    /// Times of packets sent but not yet out (decoders reorder B-frames;
    /// the earliest pending time is the next picture's).
    pending: BinaryHeap<Reverse<OrdF64>>,
    /// Pictures still held at the end of the stream.
    flushed: Vec<VideoFrame>,
    ended: bool,
}

struct AudioTrack {
    id: u32,
    time_base: Option<TimeBase>,
    decoder: Box<dyn AudioDecoder>,
}

#[derive(PartialEq, PartialOrd, Clone, Copy)]
struct OrdF64(f64);
impl Eq for OrdF64 {}
#[allow(clippy::derive_ord_xor_partial_ord)]
impl Ord for OrdF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// A media file being decoded, output by output.
pub struct Demuxer {
    reader: Box<dyn FormatReader>,
    video: Option<VideoTrack>,
    audio: Option<AudioTrack>,
    info: Info,
}

fn seconds(time_base: Option<TimeBase>, ts: symphonia::core::units::Timestamp) -> f64 {
    time_base.and_then(|tb| tb.calc_time(ts)).map_or(0.0, |t| t.as_secs_f64())
}

/// `avcC`'s parameter sets (as Annex B) and its length prefix size.
fn avc_config(avcc: &[u8]) -> Option<(Vec<u8>, usize)> {
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

/// An H.264 decoder primed with the stream's parameter sets.
fn h264_decoder(sets: &[u8]) -> Result<openh264::decoder::Decoder> {
    // No flush per decode: B-frames come out after the frames they
    // reference, so the decoder must keep them.
    let config = openh264::decoder::DecoderConfig::new().flush_after_decode(openh264::decoder::Flush::NoFlush);
    let mut decoder = openh264::decoder::Decoder::with_api_config(openh264::OpenH264API::from_source(), config).map_err(|e| anyhow!("openh264: {e}"))?;
    if !sets.is_empty() {
        decoder.decode(sets).ok();
    }
    Ok(decoder)
}

/// Whether a packet (length-prefixed NAL units) starts a picture decodable
/// on its own (an IDR slice).
fn is_keyframe(data: &[u8], length_size: usize) -> bool {
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

/// Length-prefixed NAL units as Annex B (start codes).
fn to_annex_b(data: &[u8], length_size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut at = 0;
    while at + length_size <= data.len() {
        let len = data[at..at + length_size].iter().fold(0usize, |n, &b| (n << 8) | usize::from(b));
        at += length_size;
        let end = (at + len).min(data.len());
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[at..end]);
        at = end;
    }
    out
}

fn rgba_of(yuv: &openh264::decoder::DecodedYUV<'_>) -> (u32, u32, Vec<u8>) {
    use openh264::formats::YUVSource;
    let (w, h) = yuv.dimensions();
    let mut rgba = vec![0u8; w * h * 4];
    yuv.write_rgba8(&mut rgba);
    (w as u32, h as u32, rgba)
}

impl Demuxer {
    /// Open a whole file (`hint`: its extension or MIME subtype, if known).
    pub fn open(bytes: Vec<u8>, hint: Option<&str>) -> Result<Self> {
        Self::open_source(Box::new(Cursor::new(bytes)), hint)
    }

    /// Open a file that may still be arriving (see [`Growing`]).
    pub fn open_source(source: Box<dyn symphonia::core::io::MediaSource>, hint: Option<&str>) -> Result<Self> {
        let source = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
        let mut h = Hint::new();
        if let Some(ext) = hint {
            h.with_extension(ext);
        }
        let reader = symphonia::default::get_probe()
            .probe(&h, source, FormatOptions::default(), MetadataOptions::default())
            .context("unrecognized media")?;
        let mut info = Info::default();
        let mut video = None;
        let mut audio = None;
        for track in reader.tracks() {
            match &track.codec_params {
                Some(CodecParameters::Video(p)) if video.is_none() && p.codec == CODEC_ID_H264 => {
                    let config = p.extra_data.iter().find(|e| e.id == VIDEO_EXTRA_DATA_ID_AVC_DECODER_CONFIG).and_then(|e| avc_config(&e.data));
                    let (sets, length_size) = config.unwrap_or((Vec::new(), 4));
                    let decoder = h264_decoder(&sets)?;
                    info.has_video = true;
                    info.width = u32::from(p.width.unwrap_or(0));
                    info.height = u32::from(p.height.unwrap_or(0));
                    video = Some(VideoTrack {
                        id: track.id,
                        time_base: track.time_base,
                        decoder,
                        sets,
                        length_size,
                        pending: BinaryHeap::new(),
                        flushed: Vec::new(),
                        ended: false,
                    });
                }
                Some(CodecParameters::Audio(p)) if audio.is_none() => {
                    if let Ok(decoder) = symphonia::default::get_codecs().make_audio_decoder(p, &AudioDecoderOptions::default()) {
                        info.has_audio = true;
                        audio = Some(AudioTrack { id: track.id, time_base: track.time_base, decoder });
                    }
                }
                _ => {}
            }
            if let (Some(n), Some(tb)) = (track.num_frames, track.time_base)
                && let Ok(ts) = i64::try_from(n)
                && let Some(t) = tb.calc_time(ts.into())
            {
                let d = t.as_secs_f64();
                info.duration = Some(info.duration.map_or(d, |old: f64| old.max(d)));
            }
        }
        if video.is_none() && audio.is_none() {
            return Err(anyhow!("no playable track"));
        }
        Ok(Demuxer { reader, video, audio, info })
    }

    pub fn info(&self) -> Info {
        self.info
    }

    /// The next decoded output, in decode order (`None` at the end).
    pub fn decode_next(&mut self) -> Result<Option<Output>> {
        loop {
            let Some(packet) = self.reader.next_packet()? else { return Ok(self.drain().map(Output::Video)) };
            if let Some(v) = self.video.as_mut().filter(|v| v.id == packet.track_id) {
                let time = seconds(v.time_base, packet.pts);
                v.pending.push(Reverse(OrdF64(time)));
                let annex_b = to_annex_b(&packet.data, v.length_size);
                // One NAL unit at a time, as openh264 takes them.
                let mut decoded = None;
                for nal in openh264::nal_units(&annex_b) {
                    match v.decoder.decode(nal) {
                        Ok(Some(yuv)) => decoded = Some(rgba_of(&yuv)),
                        Ok(None) => {}
                        // A broken unit: skip it.
                        Err(e) => {
                            if std::env::var_os("MEDIA_DEBUG").is_some() {
                                eprintln!("media: h264 packet at {time:.3}s: {e}");
                            }
                        }
                    }
                }
                if let Some((width, height, rgba)) = decoded {
                    let time = v.pending.pop().map_or(time, |Reverse(t)| t.0);
                    return Ok(Some(Output::Video(VideoFrame { width, height, rgba, time })));
                }
                continue;
            }
            if let Some(a) = self.audio.as_mut().filter(|a| a.id == packet.track_id) {
                let time = seconds(a.time_base, packet.pts);
                let Ok(buffer) = a.decoder.decode(&packet) else { continue };
                let rate = buffer.spec().rate();
                let channels = buffer.spec().channels().count() as u16;
                let mut samples = Vec::new();
                buffer.copy_to_vec_interleaved(&mut samples);
                return Ok(Some(Output::Audio(AudioChunk { rate, channels, samples, time })));
            }
        }
    }

    /// The pictures the decoder still holds, once the packets run out.
    fn drain(&mut self) -> Option<VideoFrame> {
        let v = self.video.as_mut()?;
        if !v.ended {
            v.ended = true;
            if let Ok(rest) = v.decoder.flush_remaining() {
                for yuv in rest {
                    let (width, height, rgba) = rgba_of(&yuv);
                    let time = v.pending.pop().map_or(0.0, |Reverse(t)| t.0);
                    v.flushed.push(VideoFrame { width, height, rgba, time });
                }
                v.flushed.reverse();
            }
        }
        v.flushed.pop()
    }

    /// Go to (about) `time` seconds: the next outputs are from the
    /// keyframe at or before it.
    pub fn seek(&mut self, time: f64) -> Result<()> {
        // Pictures decode from a keyframe: the last one at or before.
        let from = match self.keyframe_before(time)? {
            Some(k) => k,
            None => time,
        };
        self.seek_raw(from)?;
        if let Some(v) = &mut self.video {
            v.pending.clear();
            v.flushed.clear();
            v.ended = false;
            // A fresh decoder: nothing held from before the jump.
            v.decoder = h264_decoder(&v.sets)?;
        }
        if let Some(a) = &mut self.audio {
            a.decoder.reset();
        }
        Ok(())
    }

    fn seek_raw(&mut self, time: f64) -> Result<()> {
        let track_id = self.video.as_ref().map(|v| v.id).or(self.audio.as_ref().map(|a| a.id));
        let time = Time::try_from_secs_f64(time.max(0.0)).ok_or_else(|| anyhow!("bad time"))?;
        self.reader.seek(SeekMode::Accurate, SeekTo::Time { time, track_id })?;
        Ok(())
    }

    /// The time of the last keyframe at or before `t`: looking back from
    /// it further and further (containers here don't say where they are).
    fn keyframe_before(&mut self, t: f64) -> Result<Option<f64>> {
        let Some((id, time_base, length_size)) = self.video.as_ref().map(|v| (v.id, v.time_base, v.length_size)) else { return Ok(None) };
        for back in [0.0, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0] {
            let from = (t - back).max(0.0);
            self.seek_raw(from)?;
            let mut found = None;
            while let Some(p) = self.reader.next_packet()? {
                if p.track_id != id {
                    continue;
                }
                let pts = seconds(time_base, p.pts);
                if pts > t + 0.0005 {
                    break;
                }
                if is_keyframe(&p.data, length_size) {
                    found = Some(pts);
                }
            }
            if found.is_some() || from == 0.0 {
                return Ok(found);
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_h264_and_aac_from_mp4() {
        let bytes = include_bytes!("../tests/data/test.mp4").to_vec();
        let mut d = Demuxer::open(bytes, Some("mp4")).expect("opens");
        let info = d.info();
        assert!(info.has_video && info.has_audio, "{info:?}");
        assert_eq!((info.width, info.height), (320, 240));
        assert!(info.duration.is_some_and(|t| (t - 2.0).abs() < 0.1), "{info:?}");
        let (mut frames, mut audio_secs, mut last) = (0, 0.0, -1.0);
        let mut first: Option<VideoFrame> = None;
        while let Some(out) = d.decode_next().expect("decodes") {
            match out {
                Output::Video(f) => {
                    assert_eq!((f.width, f.height, f.rgba.len()), (320, 240, 320 * 240 * 4));
                    assert!(f.time >= last, "frames come out in time order");
                    last = f.time;
                    frames += 1;
                    first.get_or_insert(f);
                }
                Output::Audio(a) => audio_secs += a.samples.len() as f64 / f64::from(a.channels) / f64::from(a.rate),
            }
        }
        assert_eq!(frames, 60, "every frame, reordered ones too");
        assert!((audio_secs - 2.0).abs() < 0.2, "{audio_secs}s of audio");
        // ffmpeg's test pattern: not all one color.
        let f = first.expect("a frame");
        assert!(f.rgba.chunks(4).any(|p| p[0] > 200) && f.rgba.chunks(4).any(|p| p[0] < 50));
    }
}
