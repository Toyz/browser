//! `<video>` and `<audio>`: `HTMLMediaElement`'s state and methods, native,
//! over a [`media::Player`] per element. The file downloads while it plays;
//! pictures go to the tab the way a canvas's pixels do (the element shows
//! as its `canvas_url`), and the player's news comes back as `ToJs::Media`
//! (see [`deliver`]).
//!
//! Sound needs the page to have been interacted with first, as browsers do:
//! an unmuted `play()` before then is refused (`NotAllowedError`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use boa_engine::builtins::promise::ResolvingFunctions;
use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::JsPromise;
use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, Trace};
use page::NodeId;

use crate::dom::{node_from_number, node_to_number};
use crate::{FromJs, ToJs};

/// What changed about a media element (`Dom::media_dirty`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Change {
    /// Put in the document (loads if it hasn't).
    Inserted,
    /// A `<source>` child came (loads if nothing has).
    Source,
    /// `loop` set or removed.
    Loop,
    /// `src` set: loads again.
    Src,
}

const NETWORK_EMPTY: u8 = 0;
const NETWORK_IDLE: u8 = 1;
const NETWORK_LOADING: u8 = 2;
const NETWORK_NO_SOURCE: u8 = 3;
const HAVE_NOTHING: u8 = 0;
const HAVE_METADATA: u8 = 1;
const HAVE_ENOUGH_DATA: u8 = 4;

/// One element's playback state.
struct El {
    /// Bumped by each load: news from an older player is dropped.
    generation: u32,
    player: Option<media::Player>,
    download: Option<media::Feeder>,
    src: String,
    network: u8,
    ready: u8,
    paused: bool,
    ended: bool,
    seeking: bool,
    time: f64,
    duration: f64,
    volume: f64,
    /// Set by script (`None`: the `muted` attribute says).
    muted: Option<bool>,
    rate: f64,
    default_rate: f64,
    size: (u32, u32),
    /// `MediaError` code.
    error: Option<u16>,
    /// `play()` promises waiting for playback to start.
    plays: Vec<ResolvingFunctions>,
    /// Pictures shown.
    shown: Arc<AtomicU32>,
    loaded_data: bool,
    has_audio: bool,
    /// The `MediaSource` it plays, if it does.
    source: Option<JsObject>,
    mse: Option<media::mse::Mse>,
}

impl Default for El {
    fn default() -> El {
        El {
            generation: 0,
            player: None,
            download: None,
            src: String::new(),
            network: NETWORK_EMPTY,
            ready: HAVE_NOTHING,
            paused: true,
            ended: false,
            seeking: false,
            time: 0.0,
            duration: f64::NAN,
            volume: 1.0,
            muted: None,
            rate: 1.0,
            default_rate: 1.0,
            size: (0, 0),
            error: None,
            plays: Vec::new(),
            shown: Arc::default(),
            loaded_data: false,
            has_audio: false,
            source: None,
            mse: None,
        }
    }
}

impl El {
    fn send(&self, c: media::Command) {
        if let Some(p) = &self.player {
            p.send(c);
        }
    }
}

/// The page's media elements, and where their pictures go.
pub(crate) struct Media {
    els: RefCell<HashMap<NodeId, El>>,
    frames: crate::CanvasFrames,
    updates: async_channel::Sender<FromJs>,
    /// Events to fire after the current task (`play`, `pause`...).
    queued: RefCell<Vec<(NodeId, &'static str)>>,
    scanned: std::cell::Cell<bool>,
    /// The user muted the tab.
    tab_muted: std::cell::Cell<bool>,
    /// What the tab was last told about sound playing.
    audible: std::cell::Cell<bool>,
}

fn state(cx: &Context) -> Rc<Media> {
    cx.get_data::<Rc<Media>>().expect("media installed").clone()
}

fn node_of(this: &JsValue, cx: &mut Context) -> JsResult<NodeId> {
    let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a media element"))?;
    let id = o.get(js_string!("__featherNode"), cx)?.as_number().ok_or_else(|| JsNativeError::typ().with_message("not a media element"))?;
    Ok(node_from_number(id))
}

/// Read (or change) an element's state.
fn with<R>(this: &JsValue, cx: &mut Context, f: impl FnOnce(&mut El, NodeId) -> R) -> JsResult<R> {
    let node = node_of(this, cx)?;
    let m = state(cx);
    let mut els = m.els.borrow_mut();
    Ok(f(els.entry(node).or_default(), node))
}

fn has_attr(node: NodeId, name: &str, cx: &Context) -> bool {
    let d = crate::dom::page(cx);
    let dom = d.borrow();
    dom.doc.element(node).is_some_and(|e| e.attr(name).is_some())
}

fn fire(node: NodeId, kinds: &[&str], cx: &mut Context) {
    let d = crate::dom::page(cx);
    for kind in kinds {
        crate::dom::call_global(&d, "__fireNode", &[JsValue::from(node_to_number(node)), JsValue::from(JsString::from(*kind))], cx);
    }
}

fn queue(node: NodeId, kinds: &[&'static str], cx: &Context) {
    state(cx).queued.borrow_mut().extend(kinds.iter().map(|k| (node, *k)));
}

fn settle(plays: Vec<ResolvingFunctions>, outcome: Result<(), (&str, &str)>, cx: &mut Context) {
    for p in plays {
        match outcome {
            Ok(()) => p.resolve.call(&JsValue::undefined(), &[], cx).ok(),
            Err((name, message)) => {
                let e = crate::dom_exception::error(name, message, cx).into_opaque(cx).unwrap_or_default();
                p.reject.call(&JsValue::undefined(), &[e], cx).ok()
            }
        };
    }
}

// ---- loading ---------------------------------------------------------------

/// The resource to play: `src`, or the first playable `<source>`; and a
/// hint at its format.
fn pick_source(node: NodeId, cx: &Context) -> (Option<url::Url>, Option<String>, bool) {
    let d = crate::dom::page(cx);
    let dom = d.borrow();
    let ext = |u: &url::Url| u.path().rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).filter(|e| e.len() <= 4);
    if let Some(src) = dom.doc.element(node).and_then(|e| e.attr("src")) {
        let url = dom.doc.resolve_url(src);
        let hint = url.as_ref().and_then(ext);
        return (url, hint, true);
    }
    for &c in dom.doc.children(node) {
        let Some(e) = dom.doc.element(c).filter(|e| &**e.local_name() == "source") else { continue };
        let Some(src) = e.attr("src") else { continue };
        let kind = e.attr("type").unwrap_or("");
        if !kind.is_empty() && can_play(kind).is_empty() {
            continue;
        }
        let url = dom.doc.resolve_url(src);
        let hint = kind.split(';').next().and_then(|t| t.split('/').nth(1)).map(str::trim).map(str::to_owned).or_else(|| url.as_ref().and_then(ext));
        return (url, hint, true);
    }
    (None, None, false)
}

/// The media element load algorithm: drop what was playing, start on the
/// current source.
fn load(node: NodeId, cx: &mut Context) {
    let m = state(cx);
    let (url, hint, any) = pick_source(node, cx);
    {
        let d = crate::dom::page(cx);
        let mut dom = d.borrow_mut();
        // What this load takes in: no need to load again for it.
        dom.media_dirty.retain(|(n, c)| *n != node || *c == Change::Loop);
        if dom.debug() {
            eprintln!("js: media load {node:?}: {:?}", url.as_ref().map(|u| u.as_str()));
        }
    }
    let (aborted, emptied, generation, old_source) = {
        let mut els = m.els.borrow_mut();
        let el = els.entry(node).or_default();
        let emptied = el.network != NETWORK_EMPTY;
        let aborted = el.network == NETWORK_LOADING;
        el.player = None;
        if let Some(d) = el.download.take() {
            d.close();
        }
        let old_source = el.source.take();
        el.generation += 1;
        let keep = (el.volume, el.muted, el.default_rate, el.generation, std::mem::take(&mut el.plays));
        *el = El { volume: keep.0, muted: keep.1, default_rate: keep.2, rate: keep.2, generation: keep.3, plays: keep.4, ..El::default() };
        (aborted, emptied, el.generation, old_source)
    };
    let (aborted, emptied, generation, old_source) = (aborted, emptied, generation, old_source);
    if let Some(o) = old_source {
        crate::media_source::detach(&o, cx);
    }
    {
        let d = crate::dom::page(cx);
        let mut dom = d.borrow_mut();
        if dom.media_frames.remove(&node) {
            dom.changed_at(node);
        }
    }
    let plays = m.els.borrow_mut().get_mut(&node).map(|e| std::mem::take(&mut e.plays)).unwrap_or_default();
    settle(plays, Err(("AbortError", "The play() request was interrupted by a new load request.")), cx);
    if aborted {
        queue(node, &["abort"], cx);
    }
    if emptied {
        queue(node, &["emptied"], cx);
    }
    let Some(url) = url else {
        let mut els = m.els.borrow_mut();
        let el = els.entry(node).or_default();
        if any {
            // A source that can't be resolved, or none playable.
            el.network = NETWORK_NO_SOURCE;
            el.error = Some(4);
            drop(els);
            queue(node, &["error"], cx);
        }
        return;
    };
    queue(node, &["loadstart"], cx);
    let (player, feeder, source) = start(node, generation, url.clone(), hint, cx);
    let looping = has_attr(node, "loop", cx);
    let autoplay = has_attr(node, "autoplay", cx);
    let muted_attr = has_attr(node, "muted", cx);
    let mut els = m.els.borrow_mut();
    let el = els.entry(node).or_default();
    el.src = url.to_string();
    el.network = NETWORK_LOADING;
    el.download = feeder;
    if let Some((o, mse)) = source {
        el.source = Some(o);
        el.mse = Some(mse);
    }
    let muted = el.muted.unwrap_or(muted_attr);
    player.send(media::Command::Volume(if muted || m.tab_muted.get() { 0.0 } else { el.volume as f32 }));
    player.send(media::Command::Loop(looping));
    if el.rate != 1.0 {
        player.send(media::Command::Rate(el.rate));
    }
    el.shown.store(0, Ordering::Relaxed);
    el.player = Some(player);
    drop(els);
    if autoplay && may_play(node, cx) {
        let mut els = m.els.borrow_mut();
        let el = els.entry(node).or_default();
        el.paused = false;
        el.send(media::Command::Play);
        drop(els);
        queue(node, &["play"], cx);
    }
}

/// Start downloading and playing `url` for `node`.
fn start(node: NodeId, generation: u32, url: url::Url, hint: Option<String>, cx: &mut Context) -> (media::Player, Option<media::Feeder>, Option<(JsObject, media::mse::Mse)>) {
    let m = state(cx);
    let d = crate::dom::page(cx);
    let (page, news, debug) = {
        let dom = d.borrow();
        (dom.doc.url.clone(), dom.fetch_tx.clone(), dom.debug())
    };
    // A MediaSource's address: play what the page appends to it.
    let attached = if url.scheme() == "blob" { crate::media_source::attach(url.as_str(), node, cx) } else { None };
    let (feeder, source) = media::growing(None);
    if attached.is_some() {
        feeder.close();
    } else if url.scheme() == "blob"
        && let Some((bytes, _)) = crate::blob::object_url_data(url.as_str())
    {
        feeder.push(&bytes);
        feeder.finish();
    } else {
        let feed = feeder.clone();
        // Without `crossorigin`, a media element's fetch is no-cors: the
        // response is opaque to scripts, but the element plays it (through
        // the script-facing path it came with no body: GitHub's hero
        // video, cross-origin, "failed to load"). With it, CORS applies.
        let cors = has_attr(node, "crossorigin", cx);
        let req = crate::cors::ScriptRequest {
            method: "GET".into(),
            url: url.clone(),
            headers: Vec::new(),
            body: None,
            mode: if cors { "cors" } else { "no-cors" }.into(),
            credentials: "include".into(),
        };
        std::thread::Builder::new()
            .name("feather-media-fetch".into())
            .spawn(move || {
                let ok = std::cell::Cell::new(true);
                let result = if cors || (url.scheme() == "file" && page.scheme() != "file") {
                    crate::cors::fetch_streaming(
                        &page,
                        req,
                        |head| ok.set((200..300).contains(&head.status)),
                        |chunk| {
                            feed.push(chunk);
                            ok.get() && !feed.abandoned()
                        },
                    )
                } else {
                    let http = net::HttpRequest { method: "GET".into(), url: url.clone(), headers: Vec::new(), body: None, cookies: true };
                    net::request_streaming(
                        &http,
                        |head| {
                            ok.set((200..300).contains(&head.status));
                            ok.get()
                        },
                        |chunk| {
                            feed.push(chunk);
                            !feed.abandoned()
                        },
                    )
                    .map(|_| ())
                    .map_err(|e| format!("{e:#}"))
                };
                let ok = ok.get();
                if debug {
                    eprintln!("js: media {url}: {:?} ({} bytes)", result.as_ref().map(|_| ok), feed.progress().0);
                }
                if result.is_ok() && ok { feed.finish() } else { feed.close() }
            })
            .ok();
    }
    let frames = m.frames.clone();
    let updates = m.updates.clone();
    let shown = m.els.borrow_mut().entry(node).or_default().shown.clone();
    let show = move |mut f: media::VideoFrame| {
        // Opaque RGBA to BGRA here, off the tab's thread (the hardware
        // decoder's come as BGRA).
        if !f.bgra {
            for px in f.rgba.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        let mut out = frames.lock().unwrap_or_else(|e| e.into_inner());
        // The tab is told once until it takes them.
        let tell = out.is_empty();
        out.insert(node, crate::CanvasPixels { width: f.width, height: f.height, pixels: f.rgba, bgra: true });
        drop(out);
        shown.fetch_add(1, Ordering::Relaxed);
        if tell {
            updates.send_blocking(FromJs::CanvasFrames).ok();
        }
    };
    let events = move |event| {
        news.send(ToJs::Media { node, generation, event }).ok();
    };
    let input = match &attached {
        Some((_, mse)) => media::Input::Mse(mse.clone()),
        None => media::Input::File(source, hint),
    };
    let feeder = attached.is_none().then_some(feeder);
    (media::Player::start(input, show, events), feeder, attached)
}

/// Whether `node` may start playing now: muted, or after the user
/// interacted with the page.
fn may_play(node: NodeId, cx: &Context) -> bool {
    let muted_attr = has_attr(node, "muted", cx);
    let m = state(cx);
    let audible = m.els.borrow().get(&node).is_none_or(|e| !e.muted.unwrap_or(muted_attr) && e.volume > 0.0);
    !audible || crate::dom::page(cx).borrow().activated
}

/// News from a player.
pub(crate) fn deliver(node: NodeId, generation: u32, event: media::Event, cx: &mut Context) {
    let m = state(cx);
    let mut fire_now: Vec<&'static str> = Vec::new();
    let mut plays = None;
    let mut picture = false;
    {
        let mut els = m.els.borrow_mut();
        let Some(el) = els.get_mut(&node).filter(|e| e.generation == generation) else { return };
        if crate::dom::page(cx).borrow().debug() && !matches!(event, media::Event::Time(_)) {
            eprintln!("js: media {node:?}: {event:?}");
        }
        match event {
            media::Event::Metadata(info) => {
                // (A MediaSource's says itself.)
                if el.source.is_none() {
                    el.duration = info.duration.unwrap_or(f64::INFINITY);
                }
                el.size = (info.width, info.height);
                el.has_audio = info.has_audio;
                el.ready = el.ready.max(HAVE_METADATA);
                fire_now.extend(["durationchange"]);
                if info.has_video {
                    fire_now.push("resize");
                }
                fire_now.push("loadedmetadata");
                // A time set before the metadata came.
                if el.time > 0.0 {
                    let t = el.time;
                    el.send(media::Command::Seek(t));
                }
            }
            media::Event::Ready => {
                picture = true;
                if !el.loaded_data {
                    el.loaded_data = true;
                    el.ready = HAVE_ENOUGH_DATA;
                    el.network = NETWORK_IDLE;
                    fire_now.extend(["loadeddata", "canplay", "canplaythrough"]);
                }
            }
            media::Event::Playing => {
                if !el.paused {
                    fire_now.push("playing");
                    plays = Some(std::mem::take(&mut el.plays));
                }
            }
            media::Event::Waiting => fire_now.push("waiting"),
            media::Event::Time(t) => {
                el.time = t;
                fire_now.push("timeupdate");
            }
            media::Event::Seeked(t) => {
                picture = true;
                el.time = t;
                el.seeking = false;
                // A time set before the first picture: that picture is the
                // data loaded (YouTube resumes so; it stayed at
                // HAVE_METADATA while playing).
                if !el.loaded_data {
                    el.loaded_data = true;
                    el.ready = HAVE_ENOUGH_DATA;
                    el.network = NETWORK_IDLE;
                    fire_now.extend(["loadeddata", "canplay", "canplaythrough"]);
                }
                fire_now.extend(["timeupdate", "seeked"]);
            }
            media::Event::Ended => {
                el.ended = true;
                el.paused = true;
                fire_now.extend(["pause", "ended"]);
            }
            media::Event::Error(e) => {
                if crate::dom::page(cx).borrow().debug() {
                    eprintln!("js: media {}: {e}", el.src);
                }
                el.error = Some(if el.loaded_data { 3 } else { 4 });
                el.network = if el.loaded_data { NETWORK_IDLE } else { NETWORK_NO_SOURCE };
                el.paused = true;
                plays = Some(std::mem::take(&mut el.plays));
                fire_now.push("error");
            }
        }
    }
    // The element shows its pictures from now on.
    if picture {
        let d = crate::dom::page(cx);
        let mut dom = d.borrow_mut();
        if dom.doc.element(node).is_some_and(|e| &**e.local_name() == "video") && dom.media_frames.insert(node) {
            dom.changed_at(node);
        }
    }
    if let Some(plays) = plays {
        let failed = m.els.borrow().get(&node).is_some_and(|e| e.error.is_some());
        settle(plays, if failed { Err(("NotSupportedError", "The element has no supported sources.")) } else { Ok(()) }, cx);
    }
    fire(node, &fire_now, cx);
    update_audible(cx);
}

/// The user muted or unmuted the tab.
pub(crate) fn mute_tab(muted: bool, cx: &mut Context) {
    let m = state(cx);
    m.tab_muted.set(muted);
    let d = crate::dom::page(cx);
    let dom = d.borrow();
    for (&node, e) in m.els.borrow().iter() {
        let own = e.muted.unwrap_or_else(|| dom.doc.element(node).is_some_and(|el| el.attr("muted").is_some()));
        e.send(media::Command::Volume(if muted || own { 0.0 } else { e.volume as f32 }));
    }
}

/// Tell the tab whether sound is playing (when that changes): an element
/// playing, with sound, not muted by the page.
fn update_audible(cx: &mut Context) {
    let m = state(cx);
    let d = crate::dom::page(cx);
    let audible = {
        let dom = d.borrow();
        m.els.borrow().iter().any(|(&node, e)| {
            let muted = e.muted.unwrap_or_else(|| dom.doc.element(node).is_some_and(|el| el.attr("muted").is_some()));
            e.player.is_some() && !e.paused && !e.ended && e.has_audio && !muted && e.volume > 0.0 && dom.doc.get(node).is_some()
        })
    };
    if m.audible.replace(audible) != audible {
        m.updates.send_blocking(FromJs::Audible(audible)).ok();
    }
}

/// A `MediaSource`'s duration changed.
pub(crate) fn duration_changed(node: NodeId, d: f64, cx: &mut Context) {
    let m = state(cx);
    if let Some(e) = m.els.borrow_mut().get_mut(&node) {
        e.duration = d;
    }
    queue(node, &["durationchange"], cx);
}

/// What was appended to its `MediaSource` couldn't be decoded.
pub(crate) fn decode_error(node: NodeId, cx: &mut Context) {
    let m = state(cx);
    let plays = {
        let mut els = m.els.borrow_mut();
        let Some(e) = els.get_mut(&node) else { return };
        e.error = Some(if e.loaded_data { 3 } else { 4 });
        e.paused = true;
        std::mem::take(&mut e.plays)
    };
    settle(plays, Err(("NotSupportedError", "The element has no supported sources.")), cx);
    queue(node, &["error"], cx);
}

/// After each task: start loading what changed, fire queued events.
pub(crate) fn tick(cx: &mut Context) -> bool {
    let mut did = crate::media_source::tick(cx);
    let m = state(cx);
    let d = crate::dom::page(cx);
    let mut changes = std::mem::take(&mut d.borrow_mut().media_dirty);
    did |= !changes.is_empty();
    if !m.scanned.replace(true) {
        let dom = d.borrow();
        changes.extend(dom.doc.descendants(dom.doc.root()).filter(|&n| is_media(&dom, n)).map(|n| (n, Change::Inserted)));
    }
    if !changes.is_empty() {
        // The strongest change per element, in document order.
        changes.sort();
        changes.dedup_by(|b, a| {
            if a.0 == b.0 {
                a.1 = a.1.max(b.1);
                true
            } else {
                false
            }
        });
        for (node, change) in changes {
            if !is_media(&d.borrow(), node) {
                continue;
            }
            let started = m.els.borrow().get(&node).is_some_and(|e| e.network != NETWORK_EMPTY);
            match change {
                Change::Src => load(node, cx),
                Change::Inserted | Change::Source if !started => {
                    let preload_none = d.borrow().doc.element(node).and_then(|e| e.attr("preload")).is_some_and(|p| p.eq_ignore_ascii_case("none"));
                    if !preload_none || has_attr(node, "autoplay", cx) {
                        load(node, cx);
                    }
                }
                Change::Loop => {
                    let looping = has_attr(node, "loop", cx);
                    if let Some(el) = m.els.borrow().get(&node) {
                        el.send(media::Command::Loop(looping));
                    }
                }
                _ => {}
            }
        }
    }
    let queued = std::mem::take(&mut *m.queued.borrow_mut());
    did |= !queued.is_empty();
    for (node, kind) in queued {
        fire(node, &[kind], cx);
    }
    // (A load just opened a MediaSource: `sourceopen` now.)
    let did = did | crate::media_source::tick(cx);
    update_audible(cx);
    did
}

fn is_media(dom: &crate::dom::Dom, n: NodeId) -> bool {
    dom.doc.element(n).is_some_and(|e| e.is_html() && matches!(&**e.local_name(), "video" | "audio"))
}

/// What `canPlayType` answers: `"probably"`, `"maybe"` or `""`.
pub(crate) fn can_play(kind: &str) -> &'static str {
    let mut parts = kind.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let codecs: Vec<String> = parts
        .filter_map(|p| p.trim().strip_prefix("codecs="))
        .flat_map(|c| c.trim_matches(|ch| ch == '"' || ch == '\'').split(',').map(|c| c.trim().to_ascii_lowercase()).collect::<Vec<_>>())
        .collect();
    let (container_ok, codec_ok): (bool, fn(&str) -> bool) = match mime.as_str() {
        "video/mp4" | "audio/mp4" | "audio/x-m4a" | "audio/aac" | "video/quicktime" => {
            (true, |c| c.starts_with("avc1") || c.starts_with("avc3") || c.starts_with("mp4a") || c == "mp3")
        }
        "audio/mpeg" | "audio/mp3" => (true, |c| c == "mp3" || c == "mp4a.69" || c == "mp4a.6b"),
        "audio/ogg" | "application/ogg" => (true, |c| c == "vorbis" || c == "flac"),
        "audio/flac" | "audio/x-flac" => (true, |c| c == "flac"),
        "audio/wav" | "audio/wave" | "audio/x-wav" => (true, |c| c == "1"),
        // Matroska with the codecs decoded here (not VP8/VP9/AV1/Opus yet).
        "audio/webm" => (true, |c| c == "vorbis"),
        _ => (false, |_| false),
    };
    if !container_ok {
        return "";
    }
    if codecs.is_empty() {
        return if mime == "audio/webm" { "" } else { "maybe" };
    }
    if codecs.iter().all(|c| codec_ok(c)) { "probably" } else { "" }
}

// ---- TimeRanges and MediaError ---------------------------------------------

#[derive(Trace, Finalize, JsData)]
pub(crate) struct TimeRanges {
    #[unsafe_ignore_trace]
    ranges: Vec<(f64, f64)>,
}

impl Class for TimeRanges {
    const NAME: &'static str = "TimeRanges";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let length = NativeFunction::from_fn_ptr(|this, _, _| Ok(JsValue::from(ranges_of(this)?.len() as u32)));
        let length = FunctionObjectBuilder::new(class.context().realm(), length).name(js_string!("get length")).build();
        class.accessor(js_string!("length"), Some(length), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        for (name, end) in [("start", false), ("end", true)] {
            class.method(JsString::from(name), 1, NativeFunction::from_copy_closure(move |this, a, cx| {
                let ranges = ranges_of(this)?;
                let i = a.first().cloned().unwrap_or_default().to_u32(cx)? as usize;
                match ranges.get(i) {
                    Some(r) => Ok(JsValue::from(if end { r.1 } else { r.0 })),
                    None => Err(crate::dom_exception::error("IndexSizeError", &format!("The index provided ({i}) is greater than the maximum bound."), cx)),
                }
            }));
        }
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(JsNativeError::typ().with_message("Illegal constructor").into())
    }
}

fn ranges_of(this: &JsValue) -> JsResult<Vec<(f64, f64)>> {
    this.as_object()
        .and_then(|o| o.downcast_ref::<TimeRanges>().map(|t| t.ranges.clone()))
        .ok_or_else(|| JsNativeError::typ().with_message("not a TimeRanges").into())
}

pub(crate) fn time_ranges(ranges: Vec<(f64, f64)>, cx: &mut Context) -> JsResult<JsValue> {
    Ok(TimeRanges::from_data(TimeRanges { ranges }, cx)?.into())
}

#[derive(Trace, Finalize, JsData)]
pub(crate) struct MediaError {
    #[unsafe_ignore_trace]
    code: u16,
}

const MEDIA_ERRORS: [(&str, u16); 4] = [("MEDIA_ERR_ABORTED", 1), ("MEDIA_ERR_NETWORK", 2), ("MEDIA_ERR_DECODE", 3), ("MEDIA_ERR_SRC_NOT_SUPPORTED", 4)];

impl Class for MediaError {
    const NAME: &'static str = "MediaError";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        for (name, value) in MEDIA_ERRORS {
            class.static_property(JsString::from(name), value, Attribute::ENUMERABLE);
            class.property(JsString::from(name), value, Attribute::ENUMERABLE);
        }
        for (name, message) in [("code", false), ("message", true)] {
            let get = NativeFunction::from_copy_closure(move |this, _, _| {
                let code = this.as_object().and_then(|o| o.downcast_ref::<MediaError>().map(|e| e.code)).unwrap_or(0);
                Ok(if !message {
                    JsValue::from(code)
                } else {
                    JsValue::from(JsString::from(match code {
                        1 => "MEDIA_ELEMENT_ERROR: Aborted",
                        2 => "MEDIA_ELEMENT_ERROR: Network error",
                        3 => "MEDIA_ELEMENT_ERROR: Decode error",
                        _ => "MEDIA_ELEMENT_ERROR: Format error",
                    }))
                })
            });
            let get = FunctionObjectBuilder::new(class.context().realm(), get).name(JsString::from(format!("get {name}"))).build();
            class.accessor(JsString::from(name), Some(get), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        }
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(JsNativeError::typ().with_message("Illegal constructor").into())
    }
}

// ---- the element's API -------------------------------------------------------

type Native = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

fn arg_f64(a: &[JsValue], cx: &mut Context) -> JsResult<f64> {
    a.first().cloned().unwrap_or_default().to_number(cx)
}

const GETTERS: &[(&str, Native, Option<Native>)] = &[
    ("currentSrc", |t, _, cx| with(t, cx, |e, _| JsValue::from(JsString::from(e.src.as_str()))), None),
    ("currentTime", |t, _, cx| with(t, cx, |e, _| JsValue::from(e.time)), Some(set_current_time)),
    ("duration", |t, _, cx| with(t, cx, |e, _| JsValue::from(e.duration)), None),
    ("paused", |t, _, cx| with(t, cx, |e, _| JsValue::from(e.paused)), None),
    ("ended", |t, _, cx| with(t, cx, |e, _| JsValue::from(e.ended)), None),
    ("seeking", |t, _, cx| with(t, cx, |e, _| JsValue::from(e.seeking)), None),
    ("readyState", |t, _, cx| with(t, cx, |e, _| JsValue::from(e.ready)), None),
    ("networkState", |t, _, cx| {
        let node = node_of(t, cx)?;
        let state = state(cx).els.borrow().get(&node).map(|e| e.network);
        // Never loaded: whether there's a source to.
        Ok(JsValue::from(state.unwrap_or(if pick_source(node, cx).2 { NETWORK_NO_SOURCE } else { NETWORK_EMPTY })))
    }, None),
    ("error", |t, _, cx| {
        let code = with(t, cx, |e, _| e.error)?;
        match code {
            Some(code) => Ok(MediaError::from_data(MediaError { code }, cx)?.into()),
            None => Ok(JsValue::null()),
        }
    }, None),
    ("volume", |t, _, cx| with(t, cx, |e, _| JsValue::from(e.volume)), Some(|t, a, cx| {
        let v = arg_f64(a, cx)?;
        if !(0.0..=1.0).contains(&v) {
            return Err(crate::dom_exception::error("IndexSizeError", &format!("The volume provided ({v}) is outside the range [0, 1]."), cx));
        }
        volume_changed(t, cx, |e| e.volume = v)
    })),
    ("muted", |t, _, cx| {
        let node = node_of(t, cx)?;
        let muted_attr = has_attr(node, "muted", cx);
        Ok(JsValue::from(state(cx).els.borrow().get(&node).and_then(|e| e.muted).unwrap_or(muted_attr)))
    }, Some(|t, a, cx| {
        let v = a.first().cloned().unwrap_or_default().to_boolean();
        volume_changed(t, cx, |e| e.muted = Some(v))
    })),
    ("playbackRate", |t, _, cx| with(t, cx, |e, _| JsValue::from(e.rate)), Some(|t, a, cx| {
        let v = arg_f64(a, cx)?;
        let (node, changed) = with(t, cx, |e, node| {
            let changed = e.rate != v;
            e.rate = v;
            e.send(media::Command::Rate(v));
            (node, changed)
        })?;
        if changed {
            queue(node, &["ratechange"], cx);
        }
        Ok(JsValue::undefined())
    })),
    ("defaultPlaybackRate", |t, _, cx| with(t, cx, |e, _| JsValue::from(e.default_rate)), Some(|t, a, cx| {
        let v = arg_f64(a, cx)?;
        let node = with(t, cx, |e, node| {
            e.default_rate = v;
            node
        })?;
        queue(node, &["ratechange"], cx);
        Ok(JsValue::undefined())
    })),
    ("buffered", |t, _, cx| {
        if let Some(mse) = with(t, cx, |e, _| e.mse.clone())? {
            return time_ranges(mse.buffered_all(), cx);
        }
        let ranges = with(t, cx, |e, _| match &e.download {
            Some(d) if e.duration.is_finite() => {
                let (got, total) = d.progress();
                let part = total.filter(|&n| n > 0).map_or(0.0, |n| got as f64 / n as f64);
                if part > 0.0 { vec![(0.0, e.duration * part.min(1.0))] } else { Vec::new() }
            }
            _ => Vec::new(),
        })?;
        time_ranges(ranges, cx)
    }, None),
    ("seekable", |t, _, cx| {
        if let Some(o) = with(t, cx, |e, _| e.source.clone())? {
            return time_ranges(crate::media_source::seekable(&o), cx);
        }
        let ranges = with(t, cx, |e, _| if e.ready >= HAVE_METADATA && e.duration.is_finite() { vec![(0.0, e.duration)] } else { Vec::new() })?;
        time_ranges(ranges, cx)
    }, None),
    ("played", |_, _, cx| time_ranges(Vec::new(), cx), None),
];

const VIDEO_GETTERS: &[(&str, Native)] = &[
    ("videoWidth", |t, _, cx| with(t, cx, |e, _| JsValue::from(e.size.0))),
    ("videoHeight", |t, _, cx| with(t, cx, |e, _| JsValue::from(e.size.1))),
];

fn set_current_time(t: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let v = arg_f64(a, cx)?;
    if !v.is_finite() {
        return Err(JsNativeError::typ().with_message("The provided double value is non-finite.").into());
    }
    let seek = with(t, cx, |e, node| {
        let v = if e.duration.is_finite() { v.clamp(0.0, e.duration) } else { v.max(0.0) };
        e.time = v;
        if e.ready == HAVE_NOTHING {
            // Applied once the metadata comes.
            return None;
        }
        e.seeking = true;
        e.ended = false;
        e.send(media::Command::Seek(v));
        Some(node)
    })?;
    if let Some(node) = seek {
        fire(node, &["seeking"], cx);
    }
    Ok(JsValue::undefined())
}

fn volume_changed(t: &JsValue, cx: &mut Context, f: impl FnOnce(&mut El)) -> JsResult<JsValue> {
    let node = node_of(t, cx)?;
    let muted_attr = has_attr(node, "muted", cx);
    let m = state(cx);
    {
        let mut els = m.els.borrow_mut();
        let e = els.entry(node).or_default();
        f(e);
        let muted = e.muted.unwrap_or(muted_attr) || m.tab_muted.get();
        e.send(media::Command::Volume(if muted { 0.0 } else { e.volume as f32 }));
    }
    queue(node, &["volumechange"], cx);
    Ok(JsValue::undefined())
}

const METHODS: &[(&str, usize, Native)] = &[
    ("play", 0, |t, _, cx| {
        let node = node_of(t, cx)?;
        let (promise, resolvers) = JsPromise::new_pending(cx);
        let m = state(cx);
        if m.els.borrow().get(&node).is_none_or(|e| e.network == NETWORK_EMPTY) {
            load(node, cx);
        }
        let failed = m.els.borrow().get(&node).is_none_or(|e| e.error == Some(4) || e.network == NETWORK_EMPTY);
        let refusal = if failed {
            Some(("NotSupportedError", "The element has no supported sources."))
        } else if !may_play(node, cx) {
            Some(("NotAllowedError", "play() failed because the user didn't interact with the document first."))
        } else {
            None
        };
        if let Some(refusal) = refusal {
            settle(vec![resolvers], Err(refusal), cx);
            return Ok(promise.into());
        }
        let mut els = m.els.borrow_mut();
        let e = els.entry(node).or_default();
        let was_paused = e.paused;
        e.paused = false;
        e.ended = false;
        e.send(media::Command::Play);
        if was_paused || e.ready < HAVE_ENOUGH_DATA {
            e.plays.push(resolvers);
            drop(els);
            if was_paused {
                queue(node, &["play"], cx);
            }
        } else {
            // Already playing.
            drop(els);
            settle(vec![resolvers], Ok(()), cx);
        }
        Ok(promise.into())
    }),
    ("pause", 0, |t, _, cx| {
        let node = node_of(t, cx)?;
        let m = state(cx);
        if m.els.borrow().get(&node).is_none_or(|e| e.network == NETWORK_EMPTY) {
            load(node, cx);
        }
        let plays = {
            let mut els = m.els.borrow_mut();
            let e = els.entry(node).or_default();
            if e.paused {
                return Ok(JsValue::undefined());
            }
            e.paused = true;
            e.send(media::Command::Pause);
            std::mem::take(&mut e.plays)
        };
        queue(node, &["timeupdate", "pause"], cx);
        settle(plays, Err(("AbortError", "The play() request was interrupted by a call to pause().")), cx);
        Ok(JsValue::undefined())
    }),
    ("load", 0, |t, _, cx| {
        let node = node_of(t, cx)?;
        load(node, cx);
        Ok(JsValue::undefined())
    }),
    ("canPlayType", 1, |_, a, cx| {
        let kind = a.first().cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
        Ok(JsValue::from(JsString::from(can_play(&kind))))
    }),
    ("fastSeek", 1, |t, a, cx| set_current_time(t, a, cx)),
];

fn define(proto: &JsObject, name: &str, get: Native, set: Option<Native>, cx: &mut Context) -> JsResult<()> {
    let get = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(get)).name(JsString::from(format!("get {name}"))).build();
    let mut desc = PropertyDescriptor::builder().get(get).configurable(true).enumerable(true);
    if let Some(set) = set {
        let set = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(set)).name(JsString::from(format!("set {name}"))).build();
        desc = desc.set(set);
    }
    proto.define_property_or_throw(JsString::from(name), desc, cx)?;
    Ok(())
}

fn prototype_of(class: &str, cx: &mut Context) -> Option<JsObject> {
    let c = cx.global_object().get(JsString::from(class), cx).ok()?.as_object()?;
    c.get(js_string!("prototype"), cx).ok()?.as_object()
}

/// After the prelude: the media element API on its classes.
pub(crate) fn link(frames: crate::CanvasFrames, updates: async_channel::Sender<FromJs>, cx: &mut Context) {
    cx.insert_data(Rc::new(Media {
        els: RefCell::default(),
        frames,
        updates,
        queued: RefCell::default(),
        scanned: Default::default(),
        tab_muted: Default::default(),
        audible: Default::default(),
    }));
    cx.register_global_class::<TimeRanges>().ok();
    cx.register_global_class::<MediaError>().ok();
    let Some(proto) = prototype_of("HTMLMediaElement", cx) else { return };
    for &(name, get, set) in GETTERS {
        define(&proto, name, get, set, cx).ok();
    }
    for &(name, len, f) in METHODS {
        let f = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(f)).name(JsString::from(name)).length(len).build();
        proto.set(JsString::from(name), f, true, cx).ok();
    }
    if let Some(video) = prototype_of("HTMLVideoElement", cx) {
        for &(name, get) in VIDEO_GETTERS {
            define(&video, name, get, None, cx).ok();
        }
        let quality = NativeFunction::from_fn_ptr(|t, _, cx| {
            let (shown, dropped) = with(t, cx, |e, _| (e.shown.load(Ordering::Relaxed), e.player.as_ref().map_or(0, |p| p.dropped())))?;
            let now = crate::dom::now_ms(&crate::dom::page(cx));
            let o = boa_engine::object::ObjectInitializer::new(cx)
                .property(js_string!("creationTime"), now, Attribute::all())
                .property(js_string!("droppedVideoFrames"), dropped, Attribute::all())
                .property(js_string!("totalVideoFrames"), shown + dropped, Attribute::all())
                .property(js_string!("corruptedVideoFrames"), 0, Attribute::all())
                .build();
            Ok(o.into())
        });
        let quality = FunctionObjectBuilder::new(cx.realm(), quality).name(js_string!("getVideoPlaybackQuality")).build();
        video.set(js_string!("getVideoPlaybackQuality"), quality, true, cx).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::can_play;

    #[test]
    fn can_play_types() {
        assert_eq!(can_play("video/mp4"), "maybe");
        assert_eq!(can_play(r#"video/mp4; codecs="avc1.42E01E, mp4a.40.2""#), "probably");
        assert_eq!(can_play(r#"video/webm; codecs="vp9""#), "");
        assert_eq!(can_play(r#"video/mp4; codecs="av01.0.05M.08""#), "");
        assert_eq!(can_play("audio/mpeg"), "maybe");
        assert_eq!(can_play("application/vnd.apple.mpegurl"), "");
    }
}
