//! Web Workers: `new Worker(url)` runs the script in a context of its own
//! on its own thread. Messages go both ways structured-cloned
//! ([`Cloned`]: plain data), the page's arriving through its runtime's
//! queue (`ToJs::Worker`). A worker has timers, `importScripts`, `self`'s
//! events, `location`/`navigator`/`performance` and the page's non-DOM
//! built-ins (encoding, URL, crypto, Blob, Headers, Request, AbortController,
//! FormData, structuredClone).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Instant;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::builtins::JsArray;
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, Source, js_string};
use boa_gc::{Finalize, Trace};
use url::Url;

use crate::structured_clone::{Cloned, deserialize, serialize};
use crate::{FetchText, ToJs};

/// What a worker tells its page.
pub enum WorkerEvent {
    Message(Cloned),
    /// An uncaught error (or its script failed to load).
    Error(String),
}

pub(crate) enum ToWorker {
    Message(Cloned),
    /// A request the worker made finished (`fetch`, `XMLHttpRequest`).
    Fetched { id: u32, result: Result<crate::cors::ScriptResponse, String> },
    Terminate,
}

/// The page's end of a worker's queue: terminates it when the page lets
/// go (its `Worker` collected, or the page gone).
struct Control(mpsc::Sender<ToWorker>);

impl Drop for Control {
    fn drop(&mut self) {
        self.0.send(ToWorker::Terminate).ok();
    }
}

// ---- the page's side -------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
struct Worker {
    #[unsafe_ignore_trace]
    id: u32,
    #[unsafe_ignore_trace]
    tx: RefCell<Option<Control>>,
}

/// The page's running workers, by id (kept so their messages find them).
#[derive(Default)]
struct Registry {
    workers: RefCell<HashMap<u32, JsObject>>,
    next: Cell<u32>,
}

fn registry(cx: &mut Context) -> &Registry {
    if cx.get_data::<Registry>().is_none() {
        cx.insert_data(Registry::default());
    }
    cx.get_data::<Registry>().expect("inserted")
}

fn this_worker(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|o| o.downcast_ref::<Worker>().is_some())
        .ok_or_else(|| JsNativeError::typ().with_message("not a Worker").into())
}

impl Class for Worker {
    const NAME: &'static str = "Worker";
    const LENGTH: usize = 1;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        for name in ["onmessage", "onerror", "onmessageerror"] {
            class.property(JsString::from(name), JsValue::null(), Attribute::all());
        }
        class.method(js_string!("postMessage"), 1, NativeFunction::from_fn_ptr(|this, a, cx| {
            let o = this_worker(this)?;
            let data = serialize(&a.first().cloned().unwrap_or_default(), cx)?;
            if let Some(tx) = o.downcast_ref::<Worker>().expect("a Worker").tx.borrow().as_ref() {
                tx.0.send(ToWorker::Message(data)).ok();
            }
            Ok(JsValue::undefined())
        }));
        class.method(js_string!("terminate"), 0, NativeFunction::from_fn_ptr(|this, _, cx| {
            let o = this_worker(this)?;
            let id = {
                let w = o.downcast_ref::<Worker>().expect("a Worker");
                // (Dropping it terminates it.)
                w.tx.borrow_mut().take();
                w.id
            };
            registry(cx).workers.borrow_mut().remove(&id);
            Ok(JsValue::undefined())
        }));
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let href = a.first().cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
        let url = crate::dom::resolve(cx, &href).ok_or_else(|| crate::dom_exception::error("SyntaxError", &format!("Worker: invalid URL {href:?}"), cx))?;
        let module = match a.get(1).and_then(JsValue::as_object) {
            Some(o) => o.get(js_string!("type"), cx)?.to_string(cx)?.to_std_string_escaped() == "module",
            None => false,
        };
        let (out, fetch_text, debug) = {
            let d = crate::dom::page(cx);
            let dom = d.borrow();
            (dom.fetch_tx.clone(), dom.fetch_text.clone(), dom.debug())
        };
        let reg = registry(cx);
        let id = reg.next.get() + 1;
        reg.next.set(id);
        let (tx, rx) = mpsc::channel();
        let inbox = tx.clone();
        let spawned = std::thread::Builder::new().name(format!("feather-worker-{id}")).stack_size(64 << 20).spawn(move || {
            run(Setup { id, url, module, fetch_text, out, inbox, debug }, rx);
        });
        if let Err(e) = spawned {
            return Err(JsNativeError::error().with_message(format!("Worker: {e}")).into());
        }
        Ok(Worker { id, tx: RefCell::new(Some(Control(tx))) })
    }

    fn object_constructor(instance: &JsObject<Worker>, _: &[JsValue], cx: &mut Context) -> JsResult<()> {
        let o: JsObject = instance.clone().upcast();
        let id = o.downcast_ref::<Worker>().expect("a Worker").id;
        registry(cx).workers.borrow_mut().insert(id, o);
        Ok(())
    }
}

/// Fire `type` on `target`: its listeners (`dispatchEvent`), then its
/// `on<type>` property.
fn fire(target: &JsObject, event: JsValue, kind: &str, cx: &mut Context) -> JsResult<()> {
    let dispatch = target.get(js_string!("dispatchEvent"), cx)?;
    if let Some(dispatch) = dispatch.as_callable() {
        dispatch.call(&target.clone().into(), std::slice::from_ref(&event), cx)?;
    }
    let handler = target.get(JsString::from(format!("on{kind}")), cx)?;
    if let Some(handler) = handler.as_callable() {
        handler.call(&target.clone().into(), &[event], cx)?;
    }
    Ok(())
}

/// `new <class>(kind, init)`, or a plain object with `type` when there's
/// no such class.
fn event(class: &str, kind: &str, init: JsObject, cx: &mut Context) -> JsResult<JsValue> {
    let c = cx.global_object().get(JsString::from(class), cx)?;
    match c.as_constructor() {
        Some(c) => Ok(c.construct(&[JsString::from(kind).into(), init.into()], None, cx)?.into()),
        None => {
            init.set(js_string!("type"), JsString::from(kind), false, cx)?;
            Ok(init.into())
        }
    }
}

/// A worker's event, on the page: `message` or `error` at its `Worker`.
pub(crate) fn deliver(id: u32, ev: WorkerEvent, cx: &mut Context) -> JsResult<()> {
    let Some(target) = registry(cx).workers.borrow().get(&id).cloned() else { return Ok(()) };
    let init = JsObject::with_object_proto(cx.intrinsics());
    match ev {
        WorkerEvent::Message(data) => {
            let data = deserialize(&data, cx)?;
            init.set(js_string!("data"), data, false, cx)?;
            let e = event("MessageEvent", "message", init, cx)?;
            fire(&target, e, "message", cx)
        }
        WorkerEvent::Error(message) => {
            init.set(js_string!("message"), JsString::from(message), false, cx)?;
            let e = event("ErrorEvent", "error", init.clone(), cx)?;
            if let Some(o) = e.as_object() {
                o.set(js_string!("message"), init.get(js_string!("message"), cx)?, false, cx)?;
            }
            fire(&target, e, "error", cx)
        }
    }
}

/// Register `Worker`.
pub(crate) fn install(cx: &mut Context) {
    if let Err(e) = cx.register_global_class::<Worker>() {
        eprintln!("js: Worker: {e}");
    }
}

/// Make `Worker` an `EventTarget` (once the prelude has made that).
pub(crate) fn link(cx: &mut Context) {
    crate::dom::extend_event_target::<Worker>(cx);
}

// ---- the worker's side -----------------------------------------------------

/// A worker's script URL: what its relative URLs resolve against.
pub(crate) struct Base(pub Url);

struct Setup {
    id: u32,
    url: Url,
    module: bool,
    fetch_text: FetchText,
    out: mpsc::Sender<ToJs>,
    /// The worker's own queue (for its requests' results).
    inbox: mpsc::Sender<ToWorker>,
    debug: bool,
}

/// A worker's timers: when each is due.
struct Clock {
    due: RefCell<Vec<(Instant, u32)>>,
    started: Instant,
}

impl crate::timers::TimerHost for Clock {
    fn schedule(&self, at: Instant, id: u32) {
        self.due.borrow_mut().push((at, id));
    }
    fn cancel(&self, id: u32) {
        self.due.borrow_mut().retain(|(_, t)| *t != id);
    }
    fn now_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }
}

/// A request in flight, and who hears how it went.
pub(crate) enum Pending {
    Fetch { resolve: boa_engine::object::builtins::JsFunction, reject: boa_engine::object::builtins::JsFunction },
    Xhr(JsObject),
}

/// What the worker's natives need (in the context).
pub(crate) struct Scope {
    id: u32,
    out: mpsc::Sender<ToJs>,
    fetch_text: FetchText,
    pub(crate) inbox: mpsc::Sender<ToWorker>,
    pub(crate) base: Url,
    pub(crate) debug: bool,
    closing: Cell<bool>,
    /// `self`'s listeners: (type, function).
    listeners: RefCell<Vec<(String, JsObject)>>,
    pub(crate) pending: RefCell<HashMap<u32, Pending>>,
    pub(crate) next_request: Cell<u32>,
}

pub(crate) fn scope(cx: &Context) -> &Scope {
    cx.get_data::<Scope>().expect("a worker scope")
}

/// A script's source: a `blob:` URL's bytes, or fetched.
fn load(url: &Url, fetch_text: &FetchText) -> Option<String> {
    if url.scheme() == "blob" {
        return crate::blob::object_url_data(url.as_str()).map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned());
    }
    if url.scheme() == "data" {
        let (meta, data) = url.as_str()["data:".len()..].split_once(',')?;
        let bytes = if meta.ends_with(";base64") {
            crate::encoding::base64_decode(data)?
        } else {
            percent_decode(data)
        };
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    fetch_text(url)
}

/// `%XX` escapes as bytes.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c)))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Tell the page about an uncaught error (as its `ErrorEvent.message`
/// reads in browsers: "Uncaught Error: ...").
fn report(cx: &mut Context, what: &str, e: boa_engine::JsError) {
    let text = match e.into_opaque(cx) {
        Ok(v) => v.to_string(cx).map(|s| s.to_std_string_escaped()).unwrap_or_default(),
        Err(e) => e.to_string(),
    };
    report_text(cx, what, format!("Uncaught {text}"));
}

fn report_text(cx: &Context, what: &str, message: String) {
    let s = scope(cx);
    if s.debug {
        eprintln!("js: worker {}: {what}: {message}", s.id);
    }
    s.out.send(ToJs::Worker { id: s.id, event: WorkerEvent::Error(message) }).ok();
}

/// The worker thread: set up its context, run its script, then its event
/// loop until it's closed, terminated or its page is gone.
fn run(setup: Setup, rx: mpsc::Receiver<ToWorker>) {
    let mut cx = Context::default();
    cx.runtime_limits_mut().set_recursion_limit(10_000);
    cx.runtime_limits_mut().set_stack_size_limit(1 << 20);
    let clock = Rc::new(Clock { due: RefCell::new(Vec::new()), started: Instant::now() });
    let natives = JsObject::with_null_proto();
    let fire_timer = install_scope(&mut cx, &natives, &setup, clock.clone());
    cx.insert_data(Base(setup.url.clone()));
    cx.insert_data(Scope {
        id: setup.id,
        out: setup.out.clone(),
        fetch_text: setup.fetch_text.clone(),
        inbox: setup.inbox.clone(),
        base: setup.url.clone(),
        debug: setup.debug,
        closing: Cell::new(false),
        listeners: RefCell::new(Vec::new()),
        pending: RefCell::new(HashMap::new()),
        next_request: Cell::new(1),
    });

    match load(&setup.url, &setup.fetch_text) {
        Some(code) => {
            if setup.module && setup.debug {
                eprintln!("js: worker {}: module workers run as classic scripts", setup.id);
            }
            if let Err(e) = cx.eval(Source::from_bytes(&code)) {
                report(&mut cx, "script", e);
            }
            cx.run_jobs().ok();
        }
        None => {
            report_text(&cx, "load", format!("couldn't load {}", setup.url));
            return;
        }
    }

    loop {
        // Due timers, in order.
        let now = Instant::now();
        let mut due: Vec<(Instant, u32)> = {
            let mut all = clock.due.borrow_mut();
            let (due, later) = all.drain(..).partition(|(t, _)| *t <= now);
            *all = later;
            due
        };
        due.sort();
        for (_, id) in due {
            if let Err(e) = fire_timer.call(&JsValue::undefined(), &[id.into()], &mut cx) {
                report(&mut cx, "timer", e);
            }
            cx.run_jobs().ok();
        }
        if scope(&cx).closing.get() {
            break;
        }
        let next = clock.due.borrow().iter().map(|(t, _)| *t).min();
        let msg = match next {
            Some(t) => match rx.recv_timeout(t.saturating_duration_since(Instant::now())) {
                Ok(m) => m,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            },
        };
        match msg {
            ToWorker::Terminate => break,
            ToWorker::Message(data) => {
                if let Err(e) = dispatch_message(&data, &mut cx) {
                    report(&mut cx, "onmessage", e);
                }
                cx.run_jobs().ok();
            }
            ToWorker::Fetched { id, result } => {
                if let Err(e) = crate::worker_net::fetched(id, result, &mut cx) {
                    report(&mut cx, "request", e);
                }
                cx.run_jobs().ok();
            }
        }
    }
}

/// A message from the page, at `self`: its `message` listeners, then
/// `onmessage`.
fn dispatch_message(data: &Cloned, cx: &mut Context) -> JsResult<()> {
    let data = deserialize(data, cx)?;
    let global = cx.global_object();
    let ev = ObjectInitializer::new(cx)
        .property(js_string!("type"), js_string!("message"), Attribute::all())
        .property(js_string!("data"), data, Attribute::all())
        .property(js_string!("origin"), js_string!(""), Attribute::all())
        .property(js_string!("lastEventId"), js_string!(""), Attribute::all())
        .property(js_string!("target"), global.clone(), Attribute::all())
        .property(js_string!("currentTarget"), global.clone(), Attribute::all())
        .build();
    let ports = JsArray::new(cx)?;
    ev.set(js_string!("ports"), ports, false, cx)?;
    let listeners: Vec<JsObject> = scope(cx).listeners.borrow().iter().filter(|(t, _)| t == "message").map(|(_, f)| f.clone()).collect();
    for f in listeners {
        if let Err(e) = f.call(&global.clone().into(), &[ev.clone().into()], cx) {
            report(cx, "message listener", e);
        }
    }
    let handler = global.get(js_string!("onmessage"), cx)?;
    if let Some(handler) = handler.as_callable() {
        handler.call(&global.into(), &[ev.into()], cx)?;
    }
    Ok(())
}

type Native = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

fn global_fn(cx: &mut Context, name: &str, len: usize, f: Native) {
    let f = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(f)).name(JsString::from(name)).length(len).build();
    let global = cx.global_object();
    global.set(JsString::from(name), f, false, cx).ok();
}

fn text(v: Option<&JsValue>, cx: &mut Context) -> String {
    v.map(|v| v.to_string(cx).map(|s| s.to_std_string_escaped()).unwrap_or_default()).unwrap_or_default()
}

/// The worker's global scope. Returns the timer-firing native.
fn install_scope(cx: &mut Context, natives: &JsObject, setup: &Setup, clock: Rc<Clock>) -> JsObject {
    // The page's non-DOM built-ins.
    crate::dom_exception::install(cx);
    crate::structured_clone::install(cx);
    crate::web_url::register(cx);
    crate::encoding::register(cx, natives);
    crate::crypto::install(cx);
    crate::blob::install(natives, cx);
    crate::headers::install(cx);
    crate::form_data::install(cx);
    crate::abort::install(cx);
    crate::request::install(cx);
    crate::response::install(cx);
    crate::streams::install(cx);
    crate::worker_net::install(cx);
    cx.insert_data(clock.clone());
    crate::timers::install(natives, clock, cx);

    let global = cx.global_object();
    // Timers, on the global (the page's prelude does this for `window`).
    if let Some(timers) = natives.get(js_string!("timers"), cx).ok().and_then(|t| t.as_object()) {
        for name in ["setTimeout", "setInterval", "clearTimeout", "clearInterval"] {
            let f = timers.get(JsString::from(name), cx).unwrap_or_default();
            global.set(JsString::from(name), f, false, cx).ok();
        }
    }
    let fire_timer = natives.get(js_string!("fireTimer"), cx).ok().and_then(|f| f.as_object()).expect("timers installed");
    global.set(js_string!("self"), global.clone(), false, cx).ok();
    for name in ["onmessage", "onerror", "onmessageerror"] {
        global.set(JsString::from(name), JsValue::null(), false, cx).ok();
    }

    global_fn(cx, "postMessage", 1, |_, a, cx| {
        let data = serialize(&a.first().cloned().unwrap_or_default(), cx)?;
        let s = scope(cx);
        s.out.send(ToJs::Worker { id: s.id, event: WorkerEvent::Message(data) }).ok();
        Ok(JsValue::undefined())
    });
    global_fn(cx, "close", 0, |_, _, cx| {
        scope(cx).closing.set(true);
        Ok(JsValue::undefined())
    });
    global_fn(cx, "addEventListener", 2, |_, a, cx| {
        let kind = text(a.first(), cx);
        if let Some(f) = a.get(1).and_then(JsValue::as_object).filter(|f| f.is_callable()) {
            let mut list = scope(cx).listeners.borrow_mut();
            if !list.iter().any(|(t, g)| *t == kind && g == &f) {
                list.push((kind, f));
            }
        }
        Ok(JsValue::undefined())
    });
    global_fn(cx, "removeEventListener", 2, |_, a, cx| {
        let kind = text(a.first(), cx);
        if let Some(f) = a.get(1).and_then(JsValue::as_object) {
            scope(cx).listeners.borrow_mut().retain(|(t, g)| !(*t == kind && g == &f));
        }
        Ok(JsValue::undefined())
    });
    global_fn(cx, "importScripts", 1, |_, a, cx| {
        for v in a {
            let href = v.to_string(cx)?.to_std_string_escaped();
            let url = crate::dom::resolve(cx, &href).ok_or_else(|| crate::dom_exception::error("SyntaxError", &format!("importScripts: invalid URL {href:?}"), cx))?;
            let fetch_text = scope(cx).fetch_text.clone();
            let code = load(&url, &fetch_text).ok_or_else(|| crate::dom_exception::error("NetworkError", &format!("importScripts: couldn't load {url}"), cx))?;
            cx.eval(Source::from_bytes(&code))?;
        }
        Ok(JsValue::undefined())
    });
    global_fn(cx, "queueMicrotask", 1, |_, a, cx| {
        let f = a.first().and_then(JsValue::as_callable).ok_or_else(|| JsNativeError::typ().with_message("queueMicrotask needs a function"))?;
        let promise = boa_engine::object::builtins::JsPromise::resolve(JsValue::undefined(), cx)?;
        let f = boa_engine::object::builtins::JsFunction::from_object(f.clone()).expect("callable");
        promise.then(Some(f), None, cx)?;
        Ok(JsValue::undefined())
    });

    // console: to stderr when debugging.
    let console = JsObject::with_object_proto(cx.intrinsics());
    for name in ["log", "info", "warn", "error", "debug", "trace"] {
        let f = NativeFunction::from_copy_closure(move |_, a, cx| {
            if scope(cx).debug {
                let parts: Vec<String> = a.iter().map(|v| text(Some(v), cx)).collect();
                eprintln!("worker console.{name}: {}", parts.join(" "));
            }
            Ok(JsValue::undefined())
        });
        let f = FunctionObjectBuilder::new(cx.realm(), f).name(JsString::from(name)).build();
        console.set(JsString::from(name), f, false, cx).ok();
    }
    global.set(js_string!("console"), console, false, cx).ok();

    // location (a WorkerLocation), navigator, performance.
    let url = &setup.url;
    let location = ObjectInitializer::new(cx)
        .property(js_string!("href"), JsString::from(url.as_str()), Attribute::READONLY | Attribute::ENUMERABLE)
        .property(js_string!("origin"), JsString::from(url.origin().ascii_serialization()), Attribute::READONLY | Attribute::ENUMERABLE)
        .property(js_string!("protocol"), JsString::from(format!("{}:", url.scheme())), Attribute::READONLY | Attribute::ENUMERABLE)
        .property(js_string!("host"), JsString::from(url.host_str().map(|h| match url.port() { Some(p) => format!("{h}:{p}"), None => h.to_owned() }).unwrap_or_default()), Attribute::READONLY | Attribute::ENUMERABLE)
        .property(js_string!("hostname"), JsString::from(url.host_str().unwrap_or("")), Attribute::READONLY | Attribute::ENUMERABLE)
        .property(js_string!("pathname"), JsString::from(url.path()), Attribute::READONLY | Attribute::ENUMERABLE)
        .property(js_string!("search"), JsString::from(url.query().map(|q| format!("?{q}")).unwrap_or_default()), Attribute::READONLY | Attribute::ENUMERABLE)
        .property(js_string!("hash"), JsString::from(url.fragment().map(|f| format!("#{f}")).unwrap_or_default()), Attribute::READONLY | Attribute::ENUMERABLE)
        .build();
    let href = JsValue::from(JsString::from(url.as_str()));
    let to_string = NativeFunction::from_copy_closure_with_captures(|_, _, href: &JsValue, _| Ok(href.clone()), href);
    let to_string = FunctionObjectBuilder::new(cx.realm(), to_string).name(js_string!("toString")).build();
    location.set(js_string!("toString"), to_string, false, cx).ok();
    global.set(js_string!("location"), location, false, cx).ok();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as u32;
    let languages = JsArray::from_iter([js_string!("en-US").into()], cx);
    let navigator = ObjectInitializer::new(cx)
        .property(js_string!("userAgent"), JsString::from(crate::USER_AGENT), Attribute::READONLY | Attribute::ENUMERABLE)
        .property(js_string!("hardwareConcurrency"), threads, Attribute::READONLY | Attribute::ENUMERABLE)
        .property(js_string!("language"), js_string!("en-US"), Attribute::READONLY | Attribute::ENUMERABLE)
        .property(js_string!("languages"), languages, Attribute::READONLY | Attribute::ENUMERABLE)
        .property(js_string!("onLine"), true, Attribute::READONLY | Attribute::ENUMERABLE)
        .build();
    global.set(js_string!("navigator"), navigator, false, cx).ok();
    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1000.0);
    let now = NativeFunction::from_copy_closure(|_, _, cx| Ok(JsValue::from(crate::timers::TimerHost::now_ms(&*cx.get_data::<Rc<Clock>>().expect("a clock").clone()))));
    let now = FunctionObjectBuilder::new(cx.realm(), now).name(js_string!("now")).build();
    let performance = ObjectInitializer::new(cx).property(js_string!("timeOrigin"), started, Attribute::READONLY | Attribute::ENUMERABLE).build();
    performance.set(js_string!("now"), now, false, cx).ok();
    global.set(js_string!("performance"), performance, false, cx).ok();
    fire_timer
}
