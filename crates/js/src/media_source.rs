//! Media Source Extensions: `MediaSource` and `SourceBuffer`, native, over
//! [`media::mse::Mse`] (which keeps what's appended, by time, for the
//! element's player). A `MediaSource` is played by setting a media
//! element's `src` to its object URL (`URL.createObjectURL`).
//!
//! Appends and removals finish after the current task, with their
//! `updatestart` / `update` / `updateend`, as in a browser.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};
use media::mse::Mse;
use page::NodeId;

#[derive(Trace, Finalize, JsData)]
pub(crate) struct MediaSource {
    #[unsafe_ignore_trace]
    pub mse: Mse,
    /// "closed", "open", "ended".
    #[unsafe_ignore_trace]
    state: Cell<&'static str>,
    #[unsafe_ignore_trace]
    duration: Cell<f64>,
    buffers: GcRefCell<Vec<JsObject>>,
    /// The element playing it.
    #[unsafe_ignore_trace]
    element: Cell<Option<NodeId>>,
    #[unsafe_ignore_trace]
    live: Cell<Option<(f64, f64)>>,
}

#[derive(Trace, Finalize, JsData)]
pub(crate) struct SourceBuffer {
    #[unsafe_ignore_trace]
    mse: Mse,
    #[unsafe_ignore_trace]
    index: usize,
    source: JsObject,
    #[unsafe_ignore_trace]
    updating: Cell<bool>,
    #[unsafe_ignore_trace]
    sequence: Cell<bool>,
    #[unsafe_ignore_trace]
    offset: Cell<f64>,
    #[unsafe_ignore_trace]
    window: Cell<(f64, f64)>,
    #[unsafe_ignore_trace]
    removed: Cell<bool>,
}

enum Job {
    Append(Vec<u8>),
    Remove(f64, f64),
}

/// Work and events waiting for the end of the task, and object URLs.
#[derive(Default)]
struct Pending {
    jobs: RefCell<VecDeque<(JsObject, Job)>>,
    events: RefCell<Vec<(JsObject, &'static str)>>,
    urls: RefCell<HashMap<String, JsObject>>,
}

fn pending(cx: &mut Context) -> Rc<Pending> {
    if cx.get_data::<Rc<Pending>>().is_none() {
        cx.insert_data(Rc::new(Pending::default()));
    }
    cx.get_data::<Rc<Pending>>().expect("inserted").clone()
}

fn queue(target: &JsObject, kind: &'static str, cx: &mut Context) {
    pending(cx).events.borrow_mut().push((target.clone(), kind));
}

/// `target.dispatchEvent(new Event(kind))`.
pub(crate) fn dispatch(target: &JsObject, kind: &str, cx: &mut Context) {
    let run = |cx: &mut Context| -> JsResult<()> {
        let event = cx.global_object().get(js_string!("Event"), cx)?;
        let event = event.as_constructor().ok_or_else(|| JsNativeError::typ().with_message("no Event"))?;
        let ev = event.construct(&[JsValue::from(JsString::from(kind))], None, cx)?;
        let f = target.get(js_string!("dispatchEvent"), cx)?;
        if let Some(f) = f.as_callable() {
            f.call(&target.clone().into(), &[ev.into()], cx)?;
        }
        Ok(())
    };
    if let Err(e) = run(cx)
        && crate::dom::page(cx).borrow().debug()
    {
        eprintln!("js error in {kind} handler: {e}");
    }
}

fn this_of<T: Class>(this: &JsValue) -> JsResult<JsObject> {
    this.as_object().filter(|o| o.downcast_ref::<T>().is_some()).ok_or_else(|| JsNativeError::typ().with_message(format!("not a {}", T::NAME)).into())
}

fn ms(o: &JsObject) -> boa_engine::object::Ref<'_, MediaSource> {
    o.downcast_ref::<MediaSource>().expect("a MediaSource")
}

fn sb(o: &JsObject) -> boa_engine::object::Ref<'_, SourceBuffer> {
    o.downcast_ref::<SourceBuffer>().expect("a SourceBuffer")
}

fn invalid_state(message: &str, cx: &mut Context) -> boa_engine::JsError {
    crate::dom_exception::error("InvalidStateError", message, cx)
}

fn getter(class: &mut ClassBuilder<'_>, name: &str, get: NativeFunction, set: Option<NativeFunction>) {
    let realm = class.context().realm().clone();
    let get = FunctionObjectBuilder::new(&realm, get).name(JsString::from(format!("get {name}"))).build();
    let set = set.map(|s| FunctionObjectBuilder::new(&realm, s).name(JsString::from(format!("set {name}"))).build());
    class.accessor(JsString::from(name), Some(get), set, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
}

/// Whether MSE takes `type`: the formats and codecs decoded here, at
/// sizes software decoding keeps up with.
pub(crate) fn supported(kind: &str) -> bool {
    let lower = kind.to_ascii_lowercase();
    let mime = lower.split(';').next().unwrap_or("").trim();
    if !matches!(mime, "video/mp4" | "audio/mp4") || crate::media_element::can_play(kind).is_empty() {
        return false;
    }
    let param = |name: &str| -> Option<f64> {
        lower.split(';').find_map(|p| p.trim().strip_prefix(name)?.trim_start().strip_prefix('=')?.trim().trim_matches('"').parse().ok())
    };
    let (w, h) = (param("width").unwrap_or(0.0), param("height").unwrap_or(0.0));
    w * h <= 1920.0 * 1088.0 && param("framerate").unwrap_or(30.0) <= 60.0 && param("bitrate").unwrap_or(0.0) <= 20e6
}

impl Class for MediaSource {
    const NAME: &'static str = "MediaSource";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        for name in ["onsourceopen", "onsourceended", "onsourceclose"] {
            class.property(JsString::from(name), JsValue::null(), Attribute::all());
        }
        class.static_method(js_string!("isTypeSupported"), 1, NativeFunction::from_fn_ptr(|_, a, cx| {
            let kind = a.first().cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
            Ok(JsValue::from(supported(&kind)))
        }));
        class.static_property(js_string!("canConstructInDedicatedWorker"), false, Attribute::CONFIGURABLE);
        getter(class, "readyState", NativeFunction::from_fn_ptr(|this, _, _| Ok(JsValue::from(JsString::from(ms(&this_of::<MediaSource>(this)?).state.get())))), None);
        getter(
            class,
            "duration",
            NativeFunction::from_fn_ptr(|this, _, _| {
                let o = this_of::<MediaSource>(this)?;
                let m = ms(&o);
                Ok(JsValue::from(if m.state.get() == "closed" { f64::NAN } else { m.duration.get() }))
            }),
            Some(NativeFunction::from_fn_ptr(|this, a, cx| {
                let o = this_of::<MediaSource>(this)?;
                let d = a.first().cloned().unwrap_or_default().to_number(cx)?;
                if d.is_nan() || d < 0.0 {
                    return Err(JsNativeError::typ().with_message("The provided duration is invalid.").into());
                }
                if ms(&o).state.get() != "open" || ms(&o).buffers.borrow().iter().any(|b| sb(b).updating.get()) {
                    return Err(invalid_state("The MediaSource's readyState is not 'open', or a SourceBuffer is updating.", cx));
                }
                set_duration(&o, d, cx);
                Ok(JsValue::undefined())
            })),
        );
        for (name, active) in [("sourceBuffers", false), ("activeSourceBuffers", true)] {
            let _ = active;
            getter(
                class,
                name,
                NativeFunction::from_fn_ptr(|this, _, cx| {
                    let o = this_of::<MediaSource>(this)?;
                    let list: Vec<JsValue> = ms(&o).buffers.borrow().iter().map(|b| b.clone().into()).collect();
                    Ok(JsArray::from_iter(list, cx).into())
                }),
                None,
            );
        }
        class.method(js_string!("addSourceBuffer"), 1, NativeFunction::from_fn_ptr(|this, a, cx| {
            let o = this_of::<MediaSource>(this)?;
            let kind = a.first().cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
            if !supported(&kind) {
                return Err(crate::dom_exception::error("NotSupportedError", &format!("The type provided ('{kind}') is unsupported."), cx));
            }
            if ms(&o).state.get() != "open" {
                return Err(invalid_state("The MediaSource's readyState is not 'open'.", cx));
            }
            let mse = ms(&o).mse.clone();
            let index = mse.add_buffer();
            let b = SourceBuffer {
                mse,
                index,
                source: o.clone(),
                updating: Cell::new(false),
                sequence: Cell::new(false),
                offset: Cell::new(0.0),
                window: Cell::new((0.0, f64::INFINITY)),
                removed: Cell::new(false),
            };
            let b = SourceBuffer::from_data(b, cx)?;
            ms(&o).buffers.borrow_mut().push(b.clone());
            if crate::dom::page(cx).borrow().debug() {
                eprintln!("js: MediaSource.addSourceBuffer({kind})");
            }
            Ok(b.into())
        }));
        class.method(js_string!("removeSourceBuffer"), 1, NativeFunction::from_fn_ptr(|this, a, cx| {
            let o = this_of::<MediaSource>(this)?;
            let b = a.first().and_then(JsValue::as_object).filter(|b| b.downcast_ref::<SourceBuffer>().is_some());
            let Some(b) = b.filter(|b| ms(&o).buffers.borrow().iter().any(|x| x == b)) else {
                return Err(crate::dom_exception::error("NotFoundError", "The SourceBuffer provided is not a child of this MediaSource.", cx));
            };
            ms(&o).buffers.borrow_mut().retain(|x| x != &b);
            sb(&b).removed.set(true);
            sb(&b).mse.remove_buffer(sb(&b).index);
            Ok(JsValue::undefined())
        }));
        class.method(js_string!("endOfStream"), 0, NativeFunction::from_fn_ptr(|this, a, cx| {
            let o = this_of::<MediaSource>(this)?;
            if ms(&o).state.get() != "open" || ms(&o).buffers.borrow().iter().any(|b| sb(b).updating.get()) {
                return Err(invalid_state("The MediaSource's readyState is not 'open', or a SourceBuffer is updating.", cx));
            }
            let error = a.first().filter(|v| !v.is_undefined()).map(|v| v.to_string(cx)).transpose()?;
            ms(&o).state.set("ended");
            if error.is_none() {
                // The duration becomes the end of what's buffered.
                let end = ms(&o).mse.buffered_all().last().map(|r| r.1);
                if let Some(end) = end
                    && (ms(&o).duration.get().is_nan() || end > ms(&o).duration.get())
                {
                    set_duration(&o, end, cx);
                }
            }
            ms(&o).mse.set_ended(true);
            queue(&o, "sourceended", cx);
            Ok(JsValue::undefined())
        }));
        class.method(js_string!("setLiveSeekableRange"), 2, NativeFunction::from_fn_ptr(|this, a, cx| {
            let o = this_of::<MediaSource>(this)?;
            let s = a.first().cloned().unwrap_or_default().to_number(cx)?;
            let e = a.get(1).cloned().unwrap_or_default().to_number(cx)?;
            ms(&o).live.set(Some((s, e)));
            Ok(JsValue::undefined())
        }));
        class.method(js_string!("clearLiveSeekableRange"), 0, NativeFunction::from_fn_ptr(|this, _, _| {
            ms(&this_of::<MediaSource>(this)?).live.set(None);
            Ok(JsValue::undefined())
        }));
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Ok(MediaSource {
            mse: Mse::new(),
            state: Cell::new("closed"),
            duration: Cell::new(f64::NAN),
            buffers: GcRefCell::new(Vec::new()),
            element: Cell::new(None),
            live: Cell::new(None),
        })
    }
}

fn set_duration(o: &JsObject, d: f64, cx: &mut Context) {
    let m = ms(o);
    m.duration.set(d);
    m.mse.set_duration(d);
    let element = m.element.get();
    drop(m);
    if let Some(node) = element {
        crate::media_element::duration_changed(node, d, cx);
    }
}

impl Class for SourceBuffer {
    const NAME: &'static str = "SourceBuffer";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        for name in ["onupdatestart", "onupdate", "onupdateend", "onerror", "onabort"] {
            class.property(JsString::from(name), JsValue::null(), Attribute::all());
        }
        getter(class, "updating", NativeFunction::from_fn_ptr(|this, _, _| Ok(JsValue::from(sb(&this_of::<SourceBuffer>(this)?).updating.get()))), None);
        getter(
            class,
            "buffered",
            NativeFunction::from_fn_ptr(|this, _, cx| {
                let o = this_of::<SourceBuffer>(this)?;
                if sb(&o).removed.get() {
                    return Err(invalid_state("This SourceBuffer has been removed from the parent media source.", cx));
                }
                let ranges = sb(&o).mse.buffered(sb(&o).index);
                crate::media_element::time_ranges(ranges, cx)
            }),
            None,
        );
        getter(
            class,
            "mode",
            NativeFunction::from_fn_ptr(|this, _, _| Ok(JsValue::from(js_string!(if sb(&this_of::<SourceBuffer>(this)?).sequence.get() { "sequence" } else { "segments" })))),
            Some(NativeFunction::from_fn_ptr(|this, a, cx| {
                let o = this_of::<SourceBuffer>(this)?;
                let v = a.first().cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
                sb(&o).sequence.set(v == "sequence");
                Ok(JsValue::undefined())
            })),
        );
        getter(
            class,
            "timestampOffset",
            NativeFunction::from_fn_ptr(|this, _, _| Ok(JsValue::from(sb(&this_of::<SourceBuffer>(this)?).offset.get()))),
            Some(NativeFunction::from_fn_ptr(|this, a, cx| {
                let o = this_of::<SourceBuffer>(this)?;
                let v = a.first().cloned().unwrap_or_default().to_number(cx)?;
                if sb(&o).updating.get() {
                    return Err(invalid_state("This SourceBuffer is still processing an operation.", cx));
                }
                sb(&o).offset.set(v);
                Ok(JsValue::undefined())
            })),
        );
        for (name, end) in [("appendWindowStart", false), ("appendWindowEnd", true)] {
            let get = NativeFunction::from_copy_closure(move |this, _, _| {
                let w = sb(&this_of::<SourceBuffer>(this)?).window.get();
                Ok(JsValue::from(if end { w.1 } else { w.0 }))
            });
            let set = NativeFunction::from_copy_closure(move |this, a, cx| {
                let o = this_of::<SourceBuffer>(this)?;
                let v = a.first().cloned().unwrap_or_default().to_number(cx)?;
                let mut w = sb(&o).window.get();
                if end { w.1 = v } else { w.0 = v }
                sb(&o).window.set(w);
                Ok(JsValue::undefined())
            });
            getter(class, name, get, Some(set));
        }
        class.method(js_string!("appendBuffer"), 1, NativeFunction::from_fn_ptr(|this, a, cx| {
            let o = this_of::<SourceBuffer>(this)?;
            check_usable(&o, cx)?;
            let bytes = crate::blob::bytes_of(&a.first().cloned().unwrap_or_default(), cx)?;
            // Appending after endOfStream() reopens it.
            let source = sb(&o).source.clone();
            if ms(&source).state.get() == "ended" {
                ms(&source).state.set("open");
                ms(&source).mse.set_ended(false);
            }
            start(&o, Job::Append(bytes), cx);
            Ok(JsValue::undefined())
        }));
        class.method(js_string!("remove"), 2, NativeFunction::from_fn_ptr(|this, a, cx| {
            let o = this_of::<SourceBuffer>(this)?;
            check_usable(&o, cx)?;
            let s = a.first().cloned().unwrap_or_default().to_number(cx)?;
            let e = a.get(1).cloned().unwrap_or_default().to_number(cx)?;
            if s.is_nan() || s < 0.0 || !(e > s) {
                return Err(JsNativeError::typ().with_message("The start / end provided are invalid.").into());
            }
            start(&o, Job::Remove(s, e), cx);
            Ok(JsValue::undefined())
        }));
        class.method(js_string!("abort"), 0, NativeFunction::from_fn_ptr(|this, _, cx| {
            let o = this_of::<SourceBuffer>(this)?;
            if sb(&o).removed.get() || ms(&sb(&o).source).state.get() != "open" {
                return Err(invalid_state("This SourceBuffer can't be aborted now.", cx));
            }
            let p = pending(cx);
            let had = {
                let mut jobs = p.jobs.borrow_mut();
                let before = jobs.len();
                jobs.retain(|(b, j)| !(b == &o && matches!(j, Job::Append(_))));
                before != jobs.len()
            };
            if had {
                sb(&o).updating.set(false);
                queue(&o, "abort", cx);
                queue(&o, "updateend", cx);
            }
            sb(&o).mse.reset_parser(sb(&o).index);
            sb(&o).window.set((0.0, f64::INFINITY));
            Ok(JsValue::undefined())
        }));
        class.method(js_string!("changeType"), 1, NativeFunction::from_fn_ptr(|this, a, cx| {
            let o = this_of::<SourceBuffer>(this)?;
            let kind = a.first().cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
            if !supported(&kind) {
                return Err(crate::dom_exception::error("NotSupportedError", &format!("The type provided ('{kind}') is unsupported."), cx));
            }
            check_usable(&o, cx)?;
            Ok(JsValue::undefined())
        }));
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Err(JsNativeError::typ().with_message("Illegal constructor").into())
    }
}

fn check_usable(o: &JsObject, cx: &mut Context) -> JsResult<()> {
    if sb(o).removed.get() {
        return Err(invalid_state("This SourceBuffer has been removed from the parent media source.", cx));
    }
    if sb(o).updating.get() {
        return Err(invalid_state("This SourceBuffer is still processing an 'appendBuffer' or 'remove' operation.", cx));
    }
    if ms(&sb(o).source).state.get() == "closed" {
        return Err(invalid_state("The MediaSource is closed.", cx));
    }
    Ok(())
}

fn start(o: &JsObject, job: Job, cx: &mut Context) {
    sb(o).updating.set(true);
    let p = pending(cx);
    p.jobs.borrow_mut().push_back((o.clone(), job));
    p.events.borrow_mut().push((o.clone(), "updatestart"));
}

/// After each task: finish appends and removals, fire their events (and
/// what they set off, a few rounds).
pub(crate) fn tick(cx: &mut Context) -> bool {
    let Some(p) = cx.get_data::<Rc<Pending>>().cloned() else { return false };
    let mut did = false;
    for _ in 0..32 {
        let events = std::mem::take(&mut *p.events.borrow_mut());
        let job = p.jobs.borrow_mut().pop_front();
        if events.is_empty() && job.is_none() {
            return did;
        }
        did = true;
        for (target, kind) in events {
            dispatch(&target, kind, cx);
        }
        let Some((o, job)) = job else { continue };
        if sb(&o).removed.get() {
            continue;
        }
        let result = {
            let b = sb(&o);
            match job {
                Job::Append(bytes) => b.mse.append(b.index, &bytes, b.offset.get()).map_err(|e| e.to_string()),
                Job::Remove(s, e) => {
                    b.mse.remove(b.index, s, e);
                    Ok(())
                }
            }
        };
        sb(&o).updating.set(false);
        if crate::dom::page(cx).borrow().debug() {
            let b = sb(&o);
            eprintln!("js: SourceBuffer {} → {:?}, buffered {:?}", b.index, result.as_ref().map(|_| ()), b.mse.buffered(b.index));
        }
        match result {
            Ok(()) => {
                dispatch(&o, "update", cx);
                dispatch(&o, "updateend", cx);
            }
            Err(e) => {
                if crate::dom::page(cx).borrow().debug() {
                    eprintln!("js: SourceBuffer append failed: {e}");
                }
                dispatch(&o, "error", cx);
                dispatch(&o, "updateend", cx);
                // A decode error ends the stream.
                let source = sb(&o).source.clone();
                ms(&source).state.set("ended");
                if let Some(node) = ms(&source).element.get() {
                    crate::media_element::decode_error(node, cx);
                }
            }
        }
    }
    did
}

/// `URL.createObjectURL(mediaSource)`: its address, `None` if `o` isn't
/// one.
pub(crate) fn object_url(o: &JsObject, origin: &str, cx: &mut Context) -> Option<String> {
    o.downcast_ref::<MediaSource>()?;
    use ring::rand::SecureRandom;
    let mut b = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut b).ok();
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let url = format!("blob:{origin}/{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]);
    pending(cx).urls.borrow_mut().insert(url.clone(), o.clone());
    Some(url)
}

pub(crate) fn revoke(url: &str, cx: &mut Context) {
    pending(cx).urls.borrow_mut().remove(url);
}

/// The `MediaSource` at `url`, attached to `node`'s element: now open.
pub(crate) fn attach(url: &str, node: NodeId, cx: &mut Context) -> Option<(JsObject, Mse)> {
    let o = pending(cx).urls.borrow().get(url).cloned()?;
    let m = ms(&o);
    if m.state.get() != "closed" {
        return None;
    }
    m.state.set("open");
    m.element.set(Some(node));
    m.mse.reopen();
    let mse = m.mse.clone();
    drop(m);
    queue(&o, "sourceopen", cx);
    Some((o, mse))
}

/// The element let go of it.
pub(crate) fn detach(o: &JsObject, cx: &mut Context) {
    let m = ms(o);
    if m.state.get() == "closed" {
        return;
    }
    m.state.set("closed");
    m.element.set(None);
    m.mse.close();
    // Its SourceBuffers go with it.
    let buffers = std::mem::take(&mut *m.buffers.borrow_mut());
    drop(m);
    for b in buffers {
        sb(&b).removed.set(true);
    }
    queue(o, "sourceclose", cx);
}

/// The element's `seekable` while playing `o`.
pub(crate) fn seekable(o: &JsObject) -> Vec<(f64, f64)> {
    let m = ms(o);
    let d = m.duration.get();
    if d.is_nan() {
        return Vec::new();
    }
    if d.is_infinite() {
        if let Some(r) = m.live.get() {
            return vec![r];
        }
        let b = m.mse.buffered_all();
        return match (b.first(), b.last()) {
            (Some(f), Some(l)) => vec![(f.0, l.1)],
            _ => Vec::new(),
        };
    }
    vec![(0.0, d)]
}

pub(crate) fn install(cx: &mut Context) {
    cx.register_global_class::<MediaSource>().ok();
    cx.register_global_class::<SourceBuffer>().ok();
}

/// After the prelude: both are `EventTarget`s.
pub(crate) fn link(cx: &mut Context) {
    crate::dom::extend_event_target::<MediaSource>(cx);
    crate::dom::extend_event_target::<SourceBuffer>(cx);
}

/// Pixels per second the software decoder keeps up with: 1080p at 30fps
/// (Twitch's 1080p60 decoded at about 0.6x real time and played slow).
const SMOOTH_PIXEL_RATE: f64 = 1920.0 * 1080.0 * 30.0;

/// `navigator.mediaCapabilities`: what plays, and what plays smoothly here.
/// Players pick their quality by it (Twitch asked, got nothing, and went to
/// 1080p60, which this decoder can't keep up with).
pub(crate) fn media_capabilities(cx: &mut Context) -> JsObject {
    let decoding = NativeFunction::from_fn_ptr(|_, a, cx| {
        let config = a.first().cloned().unwrap_or_default();
        let get = |o: &JsValue, k: &str, cx: &mut Context| -> JsValue {
            o.as_object().and_then(|o| o.get(JsString::from(k), cx).ok()).unwrap_or_default()
        };
        let video = get(&config, "video", cx);
        let audio = get(&config, "audio", cx);
        let kind = |v: &JsValue, cx: &mut Context| get(v, "contentType", cx).to_string(cx).map(|s| s.to_std_string_escaped()).unwrap_or_default();
        let mut supported = true;
        let mut smooth = true;
        if !video.is_undefined() {
            supported &= self::supported(&kind(&video, cx));
            let num = |k: &str, cx: &mut Context| get(&video, k, cx).to_number(cx).ok().filter(|n| n.is_finite()).unwrap_or(0.0);
            let rate = num("width", cx) * num("height", cx) * num("framerate", cx).max(1.0);
            smooth = rate <= SMOOTH_PIXEL_RATE;
            if std::env::var_os("MEDIA_DEBUG").is_some() {
                eprintln!("media: decodingInfo {} {}x{}@{} -> smooth {smooth}", kind(&video, cx), num("width", cx), num("height", cx), num("framerate", cx));
            }
        }
        if !audio.is_undefined() {
            supported &= self::supported(&kind(&audio, cx));
        }
        let info = boa_engine::object::ObjectInitializer::new(cx)
            .property(js_string!("supported"), supported, Attribute::all())
            .property(js_string!("smooth"), supported && smooth, Attribute::all())
            .property(js_string!("powerEfficient"), false, Attribute::all())
            .property(js_string!("keySystemAccess"), JsValue::null(), Attribute::all())
            .property(js_string!("configuration"), config, Attribute::all())
            .build();
        Ok(boa_engine::object::builtins::JsPromise::resolve(info, cx)?.into())
    });
    let encoding = NativeFunction::from_fn_ptr(|_, a, cx| {
        let info = boa_engine::object::ObjectInitializer::new(cx)
            .property(js_string!("supported"), false, Attribute::all())
            .property(js_string!("smooth"), false, Attribute::all())
            .property(js_string!("powerEfficient"), false, Attribute::all())
            .property(js_string!("configuration"), a.first().cloned().unwrap_or_default(), Attribute::all())
            .build();
        Ok(boa_engine::object::builtins::JsPromise::resolve(info, cx)?.into())
    });
    boa_engine::object::ObjectInitializer::new(cx)
        .function(decoding, js_string!("decodingInfo"), 1)
        .function(encoding, js_string!("encodingInfo"), 1)
        .build()
}
