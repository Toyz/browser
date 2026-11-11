//! Sound out through the system's default device (cpal), and the clock it
//! keeps: the time is what's been heard, so pictures wait for the sound.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::AudioChunk;

/// What the device's callback shares with the player.
#[derive(Default)]
struct Shared {
    /// Interleaved samples at the device's rate and channel count.
    queue: Mutex<VecDeque<f32>>,
    /// Frames played since the last `clear`.
    played: AtomicU64,
    /// `f32` bits.
    volume: AtomicU32,
    paused: AtomicBool,
}

pub(crate) struct Output {
    shared: Arc<Shared>,
    /// Kept for as long as sound plays (not `Send` everywhere: it lives
    /// on the player's thread).
    _stream: cpal::Stream,
    pub rate: u32,
    pub channels: u16,
    resampler: Resampler,
}

impl Output {
    pub fn open() -> Result<Output> {
        let device = cpal::default_host().default_output_device().ok_or_else(|| anyhow!("no audio device"))?;
        let supported = device.default_output_config()?;
        let config = supported.config();
        let shared = Arc::new(Shared { volume: AtomicU32::new(1f32.to_bits()), paused: AtomicBool::new(true), ..Default::default() });
        let channels = config.channels;
        let s = shared.clone();
        let err = |e| eprintln!("media: audio device: {e}");
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(config, move |out: &mut [f32], _: &_| fill(&s, channels, out, |v| v), err, None)?,
            cpal::SampleFormat::I16 => device.build_output_stream(
                config,
                move |out: &mut [i16], _: &_| fill(&s, channels, out, |v| (v.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16),
                err,
                None,
            )?,
            other => return Err(anyhow!("audio format {other:?}")),
        };
        stream.play()?;
        Ok(Output { shared, _stream: stream, rate: config.sample_rate, channels, resampler: Resampler::default() })
    }

    /// Queue a decoded chunk (converted to the device's rate and channels).
    pub fn push(&mut self, chunk: &AudioChunk) {
        let converted = self.resampler.run(chunk, self.rate, self.channels);
        self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).extend(converted);
    }

    /// Seconds queued but not yet heard.
    pub fn buffered(&self) -> f64 {
        let n = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).len();
        n as f64 / f64::from(self.channels) / f64::from(self.rate)
    }

    /// Seconds heard since the last `clear`.
    pub fn played(&self) -> f64 {
        self.shared.played.load(Ordering::Relaxed) as f64 / f64::from(self.rate)
    }

    /// Drop what's queued and restart the count (a seek).
    pub fn clear(&mut self) {
        self.shared.queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.shared.played.store(0, Ordering::Relaxed);
        self.resampler = Resampler::default();
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
    }

    pub fn set_volume(&self, volume: f32) {
        self.shared.volume.store(volume.to_bits(), Ordering::Relaxed);
    }
}

/// The device's callback: queued sound, or silence (paused, or the queue
/// ran dry: the clock then stands still until more comes).
fn fill<T: Copy>(s: &Shared, channels: u16, out: &mut [T], convert: impl Fn(f32) -> T) {
    let silence = convert(0.0);
    if s.paused.load(Ordering::Relaxed) {
        out.fill(silence);
        return;
    }
    // MEDIA_MUTE: play on (the clock runs) without a sound.
    static MUTE: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("MEDIA_MUTE").is_some());
    let volume = if *MUTE { 0.0 } else { f32::from_bits(s.volume.load(Ordering::Relaxed)) };
    let mut q = s.queue.lock().unwrap_or_else(|e| e.into_inner());
    // Whole frames only.
    let frames = (q.len() / usize::from(channels)).min(out.len() / usize::from(channels));
    let n = frames * usize::from(channels);
    for (o, v) in out.iter_mut().zip(q.drain(..n)) {
        *o = convert(v * volume);
    }
    drop(q);
    out[n..].fill(silence);
    s.played.fetch_add(frames as u64, Ordering::Relaxed);
}

/// Linear-interpolating rate conversion, continuous across chunks.
#[derive(Default)]
struct Resampler {
    /// The last input frame of the previous chunk.
    last: Vec<f32>,
    /// Where the next output frame falls, in input frames from `last`.
    pos: f64,
}

impl Resampler {
    fn run(&mut self, chunk: &AudioChunk, rate: u32, channels: u16) -> Vec<f32> {
        let cin = usize::from(chunk.channels.max(1));
        let cout = usize::from(channels.max(1));
        let frames = chunk.samples.len() / cin;
        if frames == 0 {
            return Vec::new();
        }
        // Each input frame mapped to the output's channels: mono to all,
        // the rest by position (extra inputs dropped, extra outputs silent
        // beyond stereo).
        let map = |frame: &[f32], ch: usize| -> f32 {
            if cin == 1 {
                frame[0]
            } else if ch < cin {
                frame[ch]
            } else {
                0.0
            }
        };
        let input = |i: isize| -> &[f32] {
            let i = i.clamp(-1, frames as isize - 1);
            if i < 0 && !self.last.is_empty() { &self.last } else { &chunk.samples[(i.max(0) as usize) * cin..(i.max(0) as usize + 1) * cin] }
        };
        let step = f64::from(chunk.rate) / f64::from(rate);
        let mut out = Vec::with_capacity(((frames as f64 / step) as usize + 2) * cout);
        // Positions are relative to the previous chunk's last frame (-1).
        let mut pos = if self.last.is_empty() { 0.0 } else { self.pos - 1.0 };
        while pos <= (frames - 1) as f64 {
            let i = pos.floor() as isize;
            let t = (pos - i as f64) as f32;
            let (a, b) = (input(i), input(i + 1));
            for ch in 0..cout {
                let (x, y) = (map(a, ch), map(b, ch));
                out.push(x + (y - x) * t);
            }
            pos += step;
        }
        self.last = chunk.samples[(frames - 1) * cin..frames * cin].to_vec();
        self.pos = pos - (frames - 1) as f64;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_continuously() {
        let mut r = Resampler::default();
        let chunk = |from: usize| AudioChunk { rate: 100, channels: 1, samples: (from..from + 10).map(|i| i as f32).collect(), time: 0.0 };
        // Doubling the rate: every half step, a ramp with no seam.
        let mut all = r.run(&chunk(0), 200, 2);
        all.extend(r.run(&chunk(10), 200, 2));
        let left: Vec<f32> = all.chunks(2).map(|f| f[0]).collect();
        for (i, v) in left.iter().enumerate() {
            assert!((v - i as f32 * 0.5).abs() < 1e-4, "{i}: {v}");
        }
        assert_eq!(left.len(), 39);
    }
}
