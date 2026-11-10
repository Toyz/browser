//! Playback: a decoding thread (which may wait on the download) feeding a
//! presenting one, which keeps time (by the sound heard, or the wall clock
//! for silent media), shows each picture when it's due and reports what a
//! media element tells its page.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::source::{Feeder, Growing};
use crate::{Demuxer, Info, Output, VideoFrame, audio};

/// What the page asks of a player.
pub enum Command {
    Play,
    Pause,
    Seek(f64),
    /// 0 to 1 (0 when muted).
    Volume(f32),
    Rate(f64),
    Loop(bool),
}

/// What a player tells the page.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The tracks and their length are known (`loadedmetadata`).
    Metadata(Info),
    /// The picture at the current time is showing (`loadeddata` the first
    /// time).
    Ready,
    /// Playing on (`playing`), after a start or a stall.
    Playing,
    /// Out of data mid-play (`waiting`).
    Waiting,
    /// Where it's at, now and then while playing (`timeupdate`).
    Time(f64),
    /// A seek landed (`seeked`).
    Seeked(f64),
    /// Played to the end (`ended`).
    Ended,
    /// Couldn't be decoded (`error`).
    Error(String),
}

/// A playing media file; dropping it stops everything.
pub struct Player {
    tx: mpsc::Sender<Command>,
    feeder: Feeder,
}

impl Player {
    /// Decode `source` (`hint`: its extension or MIME subtype), showing
    /// pictures through `show` and reporting through `events` (both called
    /// on the player's thread). It starts paused, showing its first picture.
    pub fn start(
        source: Growing,
        hint: Option<String>,
        show: impl FnMut(VideoFrame) + Send + 'static,
        events: impl FnMut(Event) + Send + 'static,
    ) -> Player {
        Self::start_with(source, hint, show, events, std::env::var_os("MEDIA_NO_AUDIO").is_none())
    }

    fn start_with(
        source: Growing,
        hint: Option<String>,
        show: impl FnMut(VideoFrame) + Send + 'static,
        events: impl FnMut(Event) + Send + 'static,
        sound: bool,
    ) -> Player {
        let feeder = source.feeder();
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("feather-media".into())
            .spawn(move || present(source, hint, rx, show, events, sound))
            .ok();
        Player { tx, feeder }
    }

    pub fn send(&self, c: Command) {
        self.tx.send(c).ok();
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // A decoder waiting on the download stops waiting.
        self.feeder.close();
    }
}

/// From the decoding thread: outputs of seek generation `.0` (`None`: the
/// end).
type Decoded = (u32, Result<Option<Output>, String>);

fn decode(mut d: Demuxer, seeks: mpsc::Receiver<(u32, f64)>, out: mpsc::SyncSender<Decoded>) {
    let mut generation = 0;
    let mut ended = false;
    loop {
        let seek = if ended { seeks.recv().ok() } else { seeks.try_recv().ok() };
        if ended && seek.is_none() {
            return;
        }
        // Only the latest of several queued seeks matters.
        if let Some(mut s) = seek {
            while let Ok(later) = seeks.try_recv() {
                s = later;
            }
            generation = s.0;
            if let Err(e) = d.seek(s.1) {
                // (Past the end, say: what follows is the end.)
                if std::env::var_os("MEDIA_DEBUG").is_some() {
                    eprintln!("media: seek to {}: {e}", s.1);
                }
            }
        }
        let next = d.decode_next().map_err(|e| e.to_string());
        ended = !matches!(next, Ok(Some(_)));
        if out.send((generation, next)).is_err() {
            return;
        }
    }
}

/// Decoded pictures held ahead of their time, at most.
const AHEAD_FRAMES: usize = 8;
/// Sound queued ahead, seconds.
const AHEAD_AUDIO: f64 = 0.5;

struct Clock {
    /// Media time when the clock (re)started.
    base: f64,
    /// Wall-clock start, while running without sound.
    since: Option<Instant>,
    rate: f64,
}

fn present(
    source: Growing,
    hint: Option<String>,
    commands: mpsc::Receiver<Command>,
    mut show: impl FnMut(VideoFrame),
    mut events: impl FnMut(Event),
    sound: bool,
) {
    let d = match Demuxer::open_source(Box::new(source), hint.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            events(Event::Error(format!("{e:#}")));
            return;
        }
    };
    let info = d.info();
    events(Event::Metadata(info));
    let (seek_tx, seek_rx) = mpsc::channel();
    let (out_tx, decoded) = mpsc::sync_channel::<Decoded>(16);
    std::thread::Builder::new().name("feather-decode".into()).spawn(move || decode(d, seek_rx, out_tx)).ok();

    let mut sound = if info.has_audio && sound {
        audio::Output::open().map_err(|e| eprintln!("media: no sound: {e}")).ok()
    } else {
        None
    };
    let mut clock = Clock { base: 0.0, since: None, rate: 1.0 };
    let mut playing = false;
    let mut looping = false;
    let mut generation = 0u32;
    let mut frames: VecDeque<VideoFrame> = VecDeque::new();
    let mut eof = false;
    // The next picture shows whatever its time (the first one, or a
    // seek's), then `Ready` / `Seeked` go out.
    let mut show_next = true;
    let mut seek_to: Option<f64> = None;
    let mut stalled = false;
    // `Playing` waits for the picture being decoded.
    let mut announce = false;
    let mut last_tick = Instant::now();
    let mut last_shown = 0.0;
    // Without a sound device: how far the (dropped) sound has been decoded.
    let mut heard_until = 0.0;

    let now = |clock: &Clock, sound: &Option<audio::Output>| -> f64 {
        match (sound, clock.since) {
            (Some(s), _) => clock.base + s.played(),
            (None, Some(t)) => clock.base + t.elapsed().as_secs_f64() * clock.rate,
            (None, None) => clock.base,
        }
    };

    loop {
        // Commands (waiting a little when there's nothing to do).
        let idle = (!playing && !show_next) || eof && frames.is_empty();
        let first = if idle { commands.recv_timeout(Duration::from_millis(50)) } else { commands.try_recv().map_err(|e| match e {
                mpsc::TryRecvError::Empty => mpsc::RecvTimeoutError::Timeout,
                mpsc::TryRecvError::Disconnected => mpsc::RecvTimeoutError::Disconnected,
            })
        };
        let mut pending: Vec<Command> = Vec::new();
        match first {
            Ok(c) => pending.push(c),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        pending.extend(commands.try_iter());
        for c in pending {
            match c {
                Command::Play if !playing => {
                    if eof && frames.is_empty() {
                        // Played out: from the start again.
                        pending_seek(0.0, &mut generation, &seek_tx, &mut frames, &mut sound, &mut clock, &mut eof);
                        seek_to = Some(0.0);
                    }
                    playing = true;
                    if sound.is_none() {
                        clock.since = Some(Instant::now());
                    }
                    if let Some(s) = &sound {
                        s.set_paused(false);
                    }
                    // (After the first picture, if it's still coming.)
                    if show_next {
                        announce = true;
                    } else {
                        events(Event::Playing);
                    }
                }
                Command::Pause if playing => {
                    clock.base = now(&clock, &sound);
                    if let Some(s) = &mut sound {
                        s.set_paused(true);
                        // Played time is kept in `base` from here.
                        clock.base -= s.played();
                    }
                    clock.since = None;
                    playing = false;
                    events(Event::Time(now(&clock, &sound)));
                }
                Command::Seek(t) => {
                    pending_seek(t, &mut generation, &seek_tx, &mut frames, &mut sound, &mut clock, &mut eof);
                    seek_to = Some(t);
                    show_next = true;
                    if playing && sound.is_none() {
                        clock.since = Some(Instant::now());
                    }
                }
                Command::Volume(v) => {
                    if let Some(s) = &sound {
                        s.set_volume(v);
                    }
                }
                Command::Rate(r) => {
                    clock.base = now(&clock, &sound);
                    if let Some(s) = &sound {
                        clock.base -= s.played();
                    }
                    if clock.since.is_some() {
                        clock.since = Some(Instant::now());
                    }
                    clock.rate = r.clamp(0.0625, 16.0);
                }
                Command::Loop(l) => looping = l,
                Command::Play | Command::Pause => {}
            }
        }

        // Take decoded outputs while short of them.
        loop {
            let audio_short = match &sound {
                Some(s) => s.buffered() < AHEAD_AUDIO,
                None => info.has_audio && heard_until < now(&clock, &sound) + AHEAD_AUDIO,
            };
            let video_short = info.has_video && frames.len() < AHEAD_FRAMES;
            if eof || !(audio_short || video_short) || frames.len() >= AHEAD_FRAMES * 4 {
                break;
            }
            let Ok((g, out)) = decoded.try_recv() else { break };
            if g != generation {
                continue;
            }
            match out {
                Ok(Some(Output::Video(f))) => {
                    // An accurate seek skips pictures before its time
                    // (keeping one, in case none come after).
                    if let Some(t) = seek_to
                        && f.time + 0.001 < t
                    {
                        frames.clear();
                    }
                    frames.push_back(f);
                }
                Ok(Some(Output::Audio(mut a))) => {
                    let channels = usize::from(a.channels.max(1));
                    heard_until = a.time + (a.samples.len() / channels) as f64 / f64::from(a.rate.max(1));
                    if let Some(t) = seek_to {
                        let skip = ((t - a.time) * f64::from(a.rate)).round();
                        if skip > 0.0 {
                            a.samples.drain(..(skip as usize * channels).min(a.samples.len()));
                        }
                    }
                    if let Some(s) = &mut sound
                        && !a.samples.is_empty()
                    {
                        s.push(&a);
                    }
                }
                Ok(None) => eof = true,
                Err(e) => {
                    events(Event::Error(e));
                    eof = true;
                }
            }
        }

        // The picture for now.
        if show_next {
            let ready = frames.iter().position(|f| seek_to.is_none_or(|t| f.time + 0.001 >= t));
            let done = !info.has_video || eof;
            if let Some(i) = ready.or(if done { frames.len().checked_sub(1) } else { None }) {
                frames.drain(..i);
                if let Some(f) = frames.pop_front() {
                    last_shown = f.time;
                    show(f);
                }
            }
            if ready.is_some() || done {
                show_next = false;
                match seek_to.take() {
                    Some(t) => {
                        clock.base = t;
                        events(Event::Seeked(t));
                    }
                    None => events(Event::Ready),
                }
                if std::mem::take(&mut announce) && playing {
                    events(Event::Playing);
                }
                if playing && sound.is_none() {
                    clock.since = Some(Instant::now());
                }
            } else {
                // Still decoding up to it.
                std::thread::sleep(Duration::from_millis(2));
            }
        } else if playing {
            let t = now(&clock, &sound);
            let mut due = None;
            while frames.front().is_some_and(|f| f.time <= t) {
                due = frames.pop_front();
            }
            if let Some(f) = due {
                last_shown = f.time;
                show(f);
            }
            // Out of pictures (or sound) mid-play: wait for the download.
            let starved = !eof && ((info.has_video && frames.is_empty()) || sound.as_ref().is_some_and(|s| s.buffered() == 0.0));
            if starved != stalled {
                stalled = starved;
                if starved {
                    clock.base = t;
                    if let Some(s) = &sound {
                        clock.base -= s.played();
                    }
                    clock.since = None;
                    events(Event::Waiting);
                } else {
                    if sound.is_none() {
                        clock.since = Some(Instant::now());
                    }
                    events(Event::Playing);
                }
            }
            let sound_done = sound.as_ref().is_none_or(|s| s.buffered() == 0.0);
            if eof && frames.is_empty() && sound_done {
                if looping {
                    pending_seek(0.0, &mut generation, &seek_tx, &mut frames, &mut sound, &mut clock, &mut eof);
                    seek_to = Some(0.0);
                    show_next = true;
                    if sound.is_none() {
                        clock.since = Some(Instant::now());
                    }
                    continue;
                }
                playing = false;
                clock.base = t.max(last_shown);
                if let Some(s) = &mut sound {
                    s.set_paused(true);
                    clock.base -= s.played();
                }
                clock.since = None;
                events(Event::Time(t.max(last_shown)));
                events(Event::Ended);
                continue;
            }
            if last_tick.elapsed() >= Duration::from_millis(250) {
                last_tick = Instant::now();
                events(Event::Time(t));
            }
            // Until the next picture is due (or a little, for sound).
            let next = frames.front().map_or(0.01, |f| ((f.time - t) / clock.rate).clamp(0.001, 0.01));
            std::thread::sleep(Duration::from_secs_f64(next));
        }
    }
}

fn pending_seek(
    t: f64,
    generation: &mut u32,
    seeks: &mpsc::Sender<(u32, f64)>,
    frames: &mut VecDeque<VideoFrame>,
    sound: &mut Option<audio::Output>,
    clock: &mut Clock,
    eof: &mut bool,
) {
    *generation += 1;
    seeks.send((*generation, t)).ok();
    frames.clear();
    if let Some(s) = sound {
        s.clear();
    }
    clock.base = t;
    *eof = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn plays_seeks_and_ends() {
        let shown: Arc<Mutex<Vec<f64>>> = Arc::default();
        let (etx, events) = mpsc::channel();
        let s = shown.clone();
        let source = Growing::whole(include_bytes!("../tests/data/test.mp4").to_vec());
        let player = Player::start_with(source, Some("mp4".into()), move |f| s.lock().unwrap().push(f.time), move |e| { etx.send(e).ok(); }, false);
        let next = || events.recv_timeout(Duration::from_secs(5)).expect("an event");
        assert!(matches!(next(), Event::Metadata(i) if i.has_video));
        assert_eq!(next(), Event::Ready);
        assert_eq!(shown.lock().unwrap().as_slice(), &[0.0], "the first picture, paused");
        player.send(Command::Seek(1.0));
        assert_eq!(next(), Event::Seeked(1.0));
        assert!(shown.lock().unwrap().last().is_some_and(|t| (t - 1.0).abs() < 0.02), "{:?}", shown.lock().unwrap());
        let started = Instant::now();
        player.send(Command::Play);
        assert_eq!(next(), Event::Playing);
        let ended = loop {
            match next() {
                Event::Ended => break started.elapsed(),
                Event::Time(_) | Event::Playing | Event::Waiting => {}
                other => panic!("{other:?}"),
            }
        };
        // The second second, in about a second, picture by picture.
        assert!(ended > Duration::from_millis(900) && ended < Duration::from_millis(1600), "{ended:?}");
        let n = shown.lock().unwrap().len();
        assert!(n > 25, "{n} pictures");
    }
}
