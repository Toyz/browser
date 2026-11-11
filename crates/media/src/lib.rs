//! Media decoding for `<video>` and `<audio>`: a container (MP4, WebM and
//! Matroska, Ogg, MP3, FLAC, WAV) demuxed by symphonia, or fragmented MP4
//! a page appends through Media Source Extensions ([`mse`]); H.264 video
//! decoded by openh264 (built from source) to RGBA frames; audio (AAC,
//! Vorbis, MP3, FLAC, PCM) decoded by symphonia to interleaved `f32`
//! samples. Each output carries its presentation time, seconds. [`Player`]
//! plays one: pictures shown when due, sound through the default device.

mod audio;
mod fmp4;
mod h264;
pub mod mse;
mod player;
mod source;

pub use player::{Command, Event, Input, Player};
pub use source::{Feeder, Growing, growing};

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

use crate::h264::H264;

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

/// Something to play: decoded outputs in decode order, and seeking.
pub(crate) trait Source: Send {
    fn info(&self) -> Info;
    /// The next output (`None` at the end; may wait for data).
    fn decode_next(&mut self) -> Result<Option<Output>>;

    /// Decoding has fallen behind: skip pictures up to the first keyframe
    /// at or after `t` (sound goes on).
    fn skip_video_to(&mut self, _t: f64) {}
    /// The next outputs are from the keyframe at or before `time`.
    fn seek(&mut self, time: f64) -> Result<()>;
}

/// Decoded sound as a chunk.
pub(crate) fn audio_chunk(buffer: symphonia::core::audio::GenericAudioBufferRef<'_>, time: f64) -> AudioChunk {
    let rate = buffer.spec().rate();
    let channels = buffer.spec().channels().count() as u16;
    let mut samples = Vec::new();
    buffer.copy_to_vec_interleaved(&mut samples);
    AudioChunk { rate, channels, samples, time }
}

struct VideoTrack {
    id: u32,
    time_base: Option<TimeBase>,
    h264: H264,
}

struct AudioTrack {
    id: u32,
    time_base: Option<TimeBase>,
    decoder: Box<dyn AudioDecoder>,
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
                    let avcc = p.extra_data.iter().find(|e| e.id == VIDEO_EXTRA_DATA_ID_AVC_DECODER_CONFIG).map(|e| &*e.data);
                    info.has_video = true;
                    info.width = u32::from(p.width.unwrap_or(0));
                    info.height = u32::from(p.height.unwrap_or(0));
                    video = Some(VideoTrack { id: track.id, time_base: track.time_base, h264: H264::new(avcc)? });
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
            let Some(packet) = self.reader.next_packet()? else {
                return Ok(self.video.as_mut().and_then(|v| v.h264.drain()).map(Output::Video));
            };
            if let Some(v) = self.video.as_mut().filter(|v| v.id == packet.track_id) {
                let time = seconds(v.time_base, packet.pts);
                if let Some(f) = v.h264.decode(&packet.data, time) {
                    return Ok(Some(Output::Video(f)));
                }
                continue;
            }
            if let Some(a) = self.audio.as_mut().filter(|a| a.id == packet.track_id) {
                let time = seconds(a.time_base, packet.pts);
                let Ok(buffer) = a.decoder.decode(&packet) else { continue };
                return Ok(Some(Output::Audio(audio_chunk(buffer, time))));
            }
        }
    }

    /// Go to (about) `time` seconds: the next outputs are from the
    /// keyframe at or before it.
    pub fn seek(&mut self, time: f64) -> Result<()> {
        // Pictures decode from a keyframe: the last one at or before.
        let from = self.keyframe_before(time)?.unwrap_or(time);
        self.seek_raw(from)?;
        if let Some(v) = &mut self.video {
            // Nothing held from before the jump.
            v.h264.reset()?;
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
        let Some((id, time_base, length_size)) = self.video.as_ref().map(|v| (v.id, v.time_base, v.h264.length_size)) else { return Ok(None) };
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
                if h264::is_keyframe(&p.data, length_size) {
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

impl Source for Demuxer {
    fn info(&self) -> Info {
        self.info
    }

    fn decode_next(&mut self) -> Result<Option<Output>> {
        Demuxer::decode_next(self)
    }

    fn seek(&mut self, time: f64) -> Result<()> {
        Demuxer::seek(self, time)
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
