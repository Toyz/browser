//! Media Source Extensions' buffers: what a page appends to its
//! `SourceBuffer`s (fragmented MP4), kept as samples by time per track, so
//! `buffered`, `remove()` and seeking work as the page expects, and played
//! from by a [`Player`](crate::Player) (see [`Mse::source`]).

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Result, anyhow};
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_MP3};
use symphonia::core::packet::Packet;

use crate::fmp4::{self, Codec, Init, Sample};
use crate::h264::H264;
use crate::{Info, Output, Source};

/// Gaps shorter than this don't split a buffered range.
const JOIN: f64 = 0.15;

/// One `SourceBuffer`.
#[derive(Default)]
struct Buffer {
    /// Bytes appended but not yet a whole box (or a `moof` waiting for
    /// its `mdat`).
    pending: Vec<u8>,
    init: Option<Arc<Init>>,
    /// Kinds of the tracks it feeds.
    video: bool,
    audio: bool,
    removed: bool,
}

/// A track's samples, in decode order, each with the setup it decodes with.
#[derive(Default)]
struct Track {
    samples: Vec<(Sample, Arc<Codec>)>,
}

impl Track {
    /// Put in a segment's samples, replacing what they overlap.
    fn insert(&mut self, new: Vec<(Sample, Arc<Codec>)>) {
        let (Some(first), Some(last)) = (new.first(), new.last()) else { return };
        let (from, to) = (first.0.dts - 1e-4, last.0.dts + 1e-4);
        self.samples.retain(|(s, _)| s.dts < from || s.dts > to);
        let at = self.samples.partition_point(|(s, _)| s.dts < first.0.dts);
        self.samples.splice(at..at, new);
    }

    /// Ranges of presentation time covered.
    fn ranges(&self) -> Vec<(f64, f64)> {
        let mut spans: Vec<(f64, f64)> = self.samples.iter().map(|(s, _)| (s.pts, s.pts + s.duration.max(1e-3))).collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut out: Vec<(f64, f64)> = Vec::new();
        for (a, b) in spans {
            match out.last_mut() {
                Some(last) if a <= last.1 + JOIN => last.1 = last.1.max(b),
                _ => out.push((a, b)),
            }
        }
        out
    }

    /// Drop samples shown in [start, end), and those decoded from them (up
    /// to the next keyframe).
    fn remove(&mut self, start: f64, end: f64) {
        let mut dropping = false;
        self.samples.retain(|(s, _)| {
            if s.pts >= start && s.pts < end {
                dropping = true;
                return false;
            }
            if dropping && !s.key {
                return false;
            }
            dropping = false;
            true
        });
    }
}

#[derive(Default)]
struct State {
    buffers: Vec<Buffer>,
    video: Track,
    audio: Track,
    /// `endOfStream()`: nothing more is coming.
    ended: bool,
    /// Detached: players stop.
    closed: bool,
    duration: Option<f64>,
    size: (u32, u32),
}

/// A `MediaSource`'s buffers (shared by its page and its player).
#[derive(Clone, Default)]
pub struct Mse(Arc<(Mutex<State>, Condvar)>);

fn intersect(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (s, e) = (a[i].0.max(b[j].0), a[i].1.min(b[j].1));
        if s < e {
            out.push((s, e));
        }
        if a[i].1 < b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

impl Mse {
    pub fn new() -> Mse {
        Mse::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn changed(&self) {
        self.0.1.notify_all();
    }

    /// A new `SourceBuffer`: its number.
    pub fn add_buffer(&self) -> usize {
        let mut s = self.lock();
        s.buffers.push(Buffer::default());
        s.buffers.len() - 1
    }

    pub fn remove_buffer(&self, buffer: usize) {
        if let Some(b) = self.lock().buffers.get_mut(buffer) {
            b.removed = true;
        }
        self.changed();
    }

    /// `appendBuffer`: whole boxes are taken in; a partial one waits for
    /// the rest. Times are shifted by `offset` seconds.
    pub fn append(&self, buffer: usize, bytes: &[u8], offset: f64) -> Result<()> {
        let mut guard = self.lock();
        let s = &mut *guard;
        let b = s.buffers.get_mut(buffer).ok_or_else(|| anyhow!("no such SourceBuffer"))?;
        b.pending.extend_from_slice(bytes);
        let data: Vec<u8> = std::mem::take(&mut b.pending);
        let mut at = 0;
        let mut video = Vec::new();
        let mut audio = Vec::new();
        while at + 8 <= data.len() {
            let Some((size, header)) = fmp4::whole_box(&data[at..]) else { break };
            let kind: [u8; 4] = data[at + 4..at + 8].try_into().expect("4 bytes");
            match &kind {
                b"moov" => {
                    let init = fmp4::parse_moov(&data[at + header..at + size]);
                    if init.tracks.is_empty() {
                        return Err(anyhow!("an initialization segment with no tracks"));
                    }
                    for t in &init.tracks {
                        match &t.codec {
                            Codec::Avc { width, height, .. } => {
                                b.video = true;
                                s.size = (u32::from(*width), u32::from(*height));
                            }
                            Codec::Aac { .. } | Codec::Mp3 { .. } => b.audio = true,
                            Codec::Other(c) => return Err(anyhow!("can't decode {c}")),
                        }
                    }
                    b.init = Some(Arc::new(init));
                }
                b"moof" => {
                    // With the `mdat` after it (offsets count from the moof).
                    let rest = &data[at + size..];
                    let Some((mdat_size, _)) = fmp4::whole_box(rest).filter(|_| rest.get(4..8) == Some(b"mdat")) else {
                        if rest.len() >= 8 && rest.get(4..8) != Some(b"mdat") {
                            return Err(anyhow!("a moof without its mdat"));
                        }
                        break;
                    };
                    let Some(init) = b.init.clone() else { return Err(anyhow!("a media segment before any initialization segment")) };
                    let segment: Arc<[u8]> = data[at..at + size + mdat_size].into();
                    let samples = fmp4::parse_fragment(&init, &segment, 0, &segment[header..size], offset);
                    let codecs: Vec<(u32, Arc<Codec>)> = init.tracks.iter().map(|t| (t.id, Arc::new(t.codec.clone()))).collect();
                    for sample in samples {
                        let Some((_, codec)) = codecs.iter().find(|(id, _)| *id == sample.track) else { continue };
                        match **codec {
                            Codec::Avc { .. } => video.push((sample, codec.clone())),
                            _ => audio.push((sample, codec.clone())),
                        }
                    }
                    at += mdat_size;
                }
                _ => {}
            }
            at += size;
        }
        b.pending = data[at..].to_vec();
        s.video.insert(video);
        s.audio.insert(audio);
        drop(guard);
        self.changed();
        Ok(())
    }

    /// `abort()`: drop a partly appended segment.
    pub fn reset_parser(&self, buffer: usize) {
        if let Some(b) = self.lock().buffers.get_mut(buffer) {
            b.pending.clear();
        }
    }

    /// `remove(start, end)` on a buffer's tracks.
    pub fn remove(&self, buffer: usize, start: f64, end: f64) {
        let mut s = self.lock();
        let Some(b) = s.buffers.get(buffer) else { return };
        let (v, a) = (b.video, b.audio);
        if v {
            s.video.remove(start, end);
        }
        if a {
            s.audio.remove(start, end);
        }
        drop(s);
        self.changed();
    }

    /// A buffer's `buffered` (its tracks' ranges, intersected).
    pub fn buffered(&self, buffer: usize) -> Vec<(f64, f64)> {
        let s = self.lock();
        let Some(b) = s.buffers.get(buffer) else { return Vec::new() };
        match (b.video, b.audio) {
            (true, true) => intersect(&s.video.ranges(), &s.audio.ranges()),
            (true, false) => s.video.ranges(),
            (false, true) => s.audio.ranges(),
            _ => Vec::new(),
        }
    }

    /// The media element's `buffered`: all buffers', intersected.
    pub fn buffered_all(&self) -> Vec<(f64, f64)> {
        let live: Vec<usize> = self.lock().buffers.iter().enumerate().filter(|(_, b)| !b.removed).map(|(i, _)| i).collect();
        let mut out: Option<Vec<(f64, f64)>> = None;
        for i in live {
            let r = self.buffered(i);
            out = Some(match out {
                Some(o) => intersect(&o, &r),
                None => r,
            });
        }
        out.unwrap_or_default()
    }

    /// Every buffer has had an initialization segment (the element has
    /// its metadata).
    pub fn initialized(&self) -> bool {
        let s = self.lock();
        let live: Vec<&Buffer> = s.buffers.iter().filter(|b| !b.removed).collect();
        !live.is_empty() && live.iter().all(|b| b.init.is_some())
    }

    pub fn set_duration(&self, d: f64) {
        self.lock().duration = Some(d);
        self.changed();
    }

    /// `endOfStream()` (or appending again after it).
    pub fn set_ended(&self, ended: bool) {
        self.lock().ended = ended;
        self.changed();
    }

    /// Attached (again) to a media element: open, with nothing in it.
    pub fn reopen(&self) {
        *self.lock() = State::default();
        self.changed();
    }

    /// The media element let go of it.
    pub fn close(&self) {
        self.lock().closed = true;
        self.changed();
    }

    /// What its player plays from.
    pub(crate) fn source(&self, stop: Arc<std::sync::atomic::AtomicBool>) -> Result<MseSource> {
        // The metadata: once every buffer has its initialization segment.
        let mut s = self.lock();
        loop {
            if s.closed || stop.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(anyhow!("detached"));
            }
            let live: Vec<&Buffer> = s.buffers.iter().filter(|b| !b.removed).collect();
            if !live.is_empty() && live.iter().all(|b| b.init.is_some()) {
                break;
            }
            s = self.0.1.wait_timeout(s, Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner()).0;
        }
        let info = Info {
            duration: s.duration.filter(|d| d.is_finite()),
            width: s.size.0,
            height: s.size.1,
            has_video: s.buffers.iter().any(|b| !b.removed && b.video),
            has_audio: s.buffers.iter().any(|b| !b.removed && b.audio),
        };
        drop(s);
        Ok(MseSource { mse: self.clone(), stop, info, video: None, audio: None, video_at: f64::NEG_INFINITY, audio_at: f64::NEG_INFINITY, seek_to: None, draining: false, skip_until: None })
    }
}

struct AudioDecode {
    codec: Arc<Codec>,
    decoder: Box<dyn AudioDecoder>,
}

/// Playing from the buffers: each track's next sample after the last one
/// decoded, the earlier of the two first.
pub(crate) struct MseSource {
    /// Pictures before the first keyframe at or after this aren't decoded
    /// (see `Source::skip_video_to`).
    skip_until: Option<f64>,
    mse: Mse,
    stop: Arc<std::sync::atomic::AtomicBool>,
    info: Info,
    video: Option<(Arc<Codec>, H264)>,
    audio: Option<AudioDecode>,
    /// Decode times of the last samples taken.
    video_at: f64,
    audio_at: f64,
    /// A seek still waiting for data at its time.
    seek_to: Option<f64>,
    draining: bool,
}

fn audio_decoder(codec: &Codec) -> Result<Box<dyn AudioDecoder>> {
    let mut p = AudioCodecParameters::new();
    match codec {
        Codec::Aac { asc, rate, channels } => {
            p.for_codec(CODEC_ID_AAC).with_sample_rate(*rate);
            if !asc.is_empty() {
                p.with_extra_data(asc.clone().into_boxed_slice());
            }
            if let Some(pos) = symphonia::core::audio::Position::from_count(u32::from(*channels)) {
                p.with_channels(symphonia::core::audio::Channels::Positioned(pos));
            }
        }
        Codec::Mp3 { rate, .. } => {
            p.for_codec(CODEC_ID_MP3).with_sample_rate(*rate);
        }
        _ => return Err(anyhow!("not audio")),
    }
    Ok(symphonia::default::get_codecs().make_audio_decoder(&p, &AudioDecoderOptions::default())?)
}

impl MseSource {
    /// Where a seek to `t` starts decoding, once data there is buffered:
    /// (video keyframe's decode time, audio's), as "after" positions.
    fn resolve_seek(s: &State, t: f64, has_video: bool, has_audio: bool) -> Option<(f64, f64)> {
        let video = if has_video {
            let v = &s.video.samples;
            // The range holding `t`, and its last keyframe at or before.
            let ranges = s.video.ranges();
            let range = ranges.iter().find(|r| r.0 <= t + 0.05 && t < r.1)?;
            let key = v.iter().rev().find(|(x, _)| x.key && x.pts <= t.max(range.0) + 1e-3 && x.pts >= range.0 - 1e-3)?;
            key.0.dts - 1e-6
        } else {
            f64::NEG_INFINITY
        };
        let audio = if has_audio {
            let a = &s.audio.samples;
            let i = a.partition_point(|(x, _)| x.pts + x.duration <= t);
            if i >= a.len() && !s.ended {
                return None;
            }
            a.get(i).map_or(t, |(x, _)| x.dts) - 1e-6
        } else {
            f64::NEG_INFINITY
        };
        Some((video, audio))
    }
}

impl Source for MseSource {
    fn info(&self) -> Info {
        self.info
    }

    fn skip_video_to(&mut self, t: f64) {
        // (Only forward: a keyframe already passed would replay the past.)
        if self.info.has_video && t > self.video_at {
            self.skip_until = Some(t);
        }
    }

    fn decode_next(&mut self) -> Result<Option<Output>> {
        if !self.draining
            && let Some(f) = self.video.as_mut().and_then(|v| v.1.ready())
        {
            return Ok(Some(Output::Video(f)));
        }
        loop {
            if self.draining {
                return Ok(self.video.as_mut().and_then(|v| v.1.drain()).map(Output::Video));
            }
            // The next sample of each track (waiting for appends).
            let (sample, ended) = {
                let mut s = self.mse.lock();
                loop {
                    if s.closed || self.stop.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err(anyhow!("detached"));
                    }
                    if let Some(t) = self.seek_to {
                        match Self::resolve_seek(&s, t, self.info.has_video, self.info.has_audio) {
                            Some((v, a)) => {
                                self.video_at = v;
                                self.audio_at = a;
                                self.seek_to = None;
                            }
                            None => {
                                s = self.mse.0.1.wait_timeout(s, Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner()).0;
                                continue;
                            }
                        }
                    }
                    let next = |track: &Track, after: f64| {
                        let i = track.samples.partition_point(|(x, _)| x.dts <= after);
                        track.samples.get(i).cloned()
                    };
                    let v = if self.info.has_video { next(&s.video, self.video_at) } else { None };
                    let a = if self.info.has_audio { next(&s.audio, self.audio_at) } else { None };
                    let pick = match (v, a) {
                        (Some(v), Some(a)) => Some(if v.0.dts <= a.0.dts { (v, true) } else { (a, false) }),
                        (Some(v), None) => Some((v, true)),
                        (None, Some(a)) => Some((a, false)),
                        (None, None) => None,
                    };
                    if pick.is_some() || s.ended {
                        break (pick, s.ended);
                    }
                    s = self.mse.0.1.wait_timeout(s, Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner()).0;
                }
            };
            let Some(((sample, codec), is_video)) = sample else {
                if ended {
                    self.draining = true;
                    continue;
                }
                continue;
            };
            if is_video {
                self.video_at = sample.dts;
                // The first decoder, or the stream changed (a new quality).
                let fresh = match &mut self.video {
                    Some((c, h)) if !Arc::ptr_eq(c, &codec) && **c != *codec => {
                        if let Codec::Avc { avcc, .. } = &*codec {
                            h.reconfigure(avcc)?;
                        }
                        *c = codec.clone();
                        false
                    }
                    Some(_) => false,
                    None => true,
                };
                if fresh {
                    let Codec::Avc { avcc, .. } = &*codec else { continue };
                    self.video = Some((codec.clone(), H264::new(Some(avcc))?));
                }
                if let Some(until) = self.skip_until {
                    if !(sample.key && sample.pts + 0.001 >= until) {
                        continue;
                    }
                    self.skip_until = None;
                }
                let (_, h) = self.video.as_mut().expect("a decoder");
                if let Some(f) = h.decode(sample.bytes(), sample.pts) {
                    return Ok(Some(Output::Video(f)));
                }
            } else {
                self.audio_at = sample.dts;
                if self.audio.as_ref().is_none_or(|a| *a.codec != *codec) {
                    self.audio = Some(AudioDecode { codec: codec.clone(), decoder: audio_decoder(&codec)? });
                }
                let a = self.audio.as_mut().expect("a decoder");
                let packet = Packet::new(0, 0.into(), 0u64.into(), sample.bytes());
                match a.decoder.decode(&packet) {
                    Ok(buffer) => return Ok(Some(Output::Audio(crate::audio_chunk(buffer, sample.pts)))),
                    Err(e) => {
                        if std::env::var_os("MEDIA_DEBUG").is_some() {
                            eprintln!("media: audio sample at {:.3}s: {e}", sample.pts);
                        }
                    }
                }
            }
            let _ = ended;
        }
    }

    fn seek(&mut self, time: f64) -> Result<()> {
        self.skip_until = None;
        self.seek_to = Some(time);
        self.draining = false;
        if let Some((_, h)) = &mut self.video {
            h.reset()?;
        }
        if let Some(a) = &mut self.audio {
            a.decoder.reset();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_appended_fragments() {
        use crate::{Command, Event, Input, Player};
        let mse = Mse::new();
        let (v, a) = (mse.add_buffer(), mse.add_buffer());
        // Appended in odd-sized pieces: boxes split across appends.
        for (buffer, bytes) in [(v, &include_bytes!("../tests/data/frag-video.mp4")[..]), (a, &include_bytes!("../tests/data/frag-audio.mp4")[..])] {
            for piece in bytes.chunks(1000) {
                mse.append(buffer, piece, 0.0).expect("appends");
            }
        }
        assert!(mse.initialized());
        for b in [v, a] {
            let r = mse.buffered(b);
            assert!(r.len() == 1 && r[0].0 < 0.1 && (r[0].1 - 2.0).abs() < 0.1, "{r:?}");
        }
        mse.remove(v, 1.5, 3.0);
        assert!(mse.buffered_all()[0].1 < 1.6, "{:?}", mse.buffered_all());
        // Put it back, and play it all.
        mse.append(v, include_bytes!("../tests/data/frag-video.mp4"), 0.0).expect("appends again");
        mse.set_ended(true);
        let shown: Arc<Mutex<Vec<f64>>> = Arc::default();
        let s = shown.clone();
        let (tx, events) = std::sync::mpsc::channel();
        let player = Player::start_with(Input::Mse(mse.clone()), move |f| s.lock().unwrap().push(f.time), move |e| {
            tx.send(e).ok();
        }, false);
        player.send(Command::Play);
        let started = std::time::Instant::now();
        loop {
            let e = events.recv_timeout(Duration::from_secs(5)).expect("an event");
            assert!(started.elapsed() < Duration::from_secs(6), "stuck at {e:?}; shown {:?}", shown.lock().unwrap().last());
            match e {
                Event::Ended => break,
                Event::Error(e) => panic!("{e}"),
                _ => {}
            }
        }
        let shown = shown.lock().unwrap();
        assert!(shown.len() > 40 && shown.windows(2).all(|w| w[0] <= w[1]), "{} pictures", shown.len());
    }

    #[test]
    fn ranges_join_and_intersect() {
        assert_eq!(intersect(&[(0.0, 5.0), (8.0, 10.0)], &[(1.0, 9.0)]), vec![(1.0, 5.0), (8.0, 9.0)]);
    }
}
