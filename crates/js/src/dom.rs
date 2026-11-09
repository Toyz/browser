        //! The script thread: a Boa context, the DOM it owns, and the native
//! functions the JS prelude builds the DOM API from.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute as PropAttr;
use boa_engine::{Context, JsArgs, JsObject, JsResult, JsString, JsValue, NativeFunction, Source, js_string};
use page::{Attribute, Document, ElementData, NodeData, NodeId};
use slotmap::{Key, KeyData};
use url::Url;

use crate::{FromJs, Options, ToJs, until};

const PRELUDE: &str = include_str!("prelude.js");
const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// Boa's host hooks: the real local time zone, and promise rejections
/// nobody handled (logged after each task when debugging).
#[derive(Default)]
struct Hooks {
    /// (promise, why), until a handler shows up.
    rejected: RefCell<Vec<(u64, String)>>,
}

fn promise_key(p: &JsObject<boa_engine::builtins::promise::Promise>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    h.finish()
}

impl boa_engine::context::HostHooks for Hooks {
    fn local_timezone_offset_seconds(&self, unix_time_seconds: i64) -> i32 {
        use chrono::{Offset, TimeZone};
        chrono::Local.timestamp_opt(unix_time_seconds, 0).single().map_or(0, |t| t.offset().fix().local_minus_utc())
    }

    fn promise_rejection_tracker(
        &self,
        promise: &JsObject<boa_engine::builtins::promise::Promise>,
        operation: boa_engine::builtins::promise::OperationType,
        cx: &mut Context,
    ) {
        use boa_engine::builtins::promise::{OperationType, PromiseState};
        let key = promise_key(promise);
        match operation {
            OperationType::Reject => {
                let state = boa_engine::object::builtins::JsPromise::from_object(promise.clone().upcast()).map(|p| p.state());
                if let Ok(PromiseState::Rejected(reason)) = state {
                    let stack = reason.as_object().and_then(|o| o.get(js_string!("stack"), cx).ok()).and_then(|s| s.as_string());
                    let message = reason.to_string(cx).map(|s| s.to_std_string_escaped()).unwrap_or_default();
                    let why = match stack {
                        Some(stack) if !stack.is_empty() => format!("{message}\n{}", stack.to_std_string_escaped()),
                        _ => message,
                    };
                    self.rejected.borrow_mut().push((key, why));
                }
            }
            OperationType::Handle => self.rejected.borrow_mut().retain(|(k, _)| *k != key),
        }
    }
}

/// How often (at most) a busy page sends the tab a new DOM.
const SNAPSHOT_EVERY: Duration = Duration::from_millis(80);

pub fn node_to_number(id: NodeId) -> f64 {
    id.data().as_ffi() as f64
}

pub fn node_from_number(n: f64) -> NodeId {
    NodeId::from(KeyData::from_ffi(n as u64))
}

/// A `<script>` we'd run: classic JavaScript (modules aren't supported).
pub(crate) fn runnable(e: &ElementData) -> bool {
    // Browsers with modules skip the fallbacks for those without.
    if e.attr("nomodule").is_some() {
        return false;
    }
    is_module(e)
        || match e.attr("type").map(|t| t.trim().to_ascii_lowercase()) {
            None => true,
            Some(t) => t.is_empty() || matches!(t.as_str(), "text/javascript" | "application/javascript" | "text/ecmascript" | "application/ecmascript" | "application/x-javascript"),
        }
}

pub(crate) fn is_module(e: &ElementData) -> bool {
    e.attr("type").is_some_and(|t| t.trim().eq_ignore_ascii_case("module"))
}

pub(crate) struct Dom {
    pub doc: Document,
    pub dirty: bool,
    /// (due, id)
    timers: Vec<(Instant, u32)>,
    /// Scripts inserted by scripts, to run after the current task.
    pending_scripts: Vec<NodeId>,
    ran: HashSet<NodeId>,
    navigate: Option<Url>,
    /// `form.submit()` for the tab to carry out.
    submit: Option<NodeId>,
    /// Form field values from the page (by node).
    values: HashMap<NodeId, String>,
    debug: bool,
    dark: bool,
    started: Instant,
    /// Shadow roots by host (and back).
    shadows: HashMap<NodeId, NodeId>,
    hosts: HashMap<NodeId, NodeId>,
    /// Where finished requests report back.
    fetch_tx: mpsc::Sender<ToJs>,
    /// Requests in flight.
    fetches: usize,
    /// The tab, for sessionStorage.
    session: u64,
    /// Responses in flight: whether the page reads them as streams.
    streams: HashMap<u32, Arc<std::sync::atomic::AtomicBool>>,
    /// Open EventSources: set to stop them.
    event_sources: HashMap<u32, Arc<std::sync::atomic::AtomicBool>>,
    /// Open WebSockets: where their outgoing frames go.
    sockets: HashMap<u32, mpsc::Sender<crate::websocket::Outgoing>>,
}

type Shared = Rc<RefCell<Dom>>;
/// A native function's body.
type NativeBody = Box<dyn Fn(&Shared, &[JsValue], &mut Context) -> JsResult<JsValue>>;

pub(crate) struct Runtime {
    cx: Context,
    dom: Shared,
    rx: mpsc::Receiver<ToJs>,
    updates: async_channel::Sender<FromJs>,
    listening: Arc<RwLock<HashSet<NodeId>>>,
    opts: Options,
    modules: Rc<crate::modules::WebModules>,
    hooks: Rc<Hooks>,
    last_sent: Instant,
    flush_at: Option<Instant>,
}

impl Runtime {
    pub fn new(
        doc: Document,
        opts: Options,
        rx: mpsc::Receiver<ToJs>,
        fetch_tx: mpsc::Sender<ToJs>,
        updates: async_channel::Sender<FromJs>,
        listening: Arc<RwLock<HashSet<NodeId>>>,
    ) -> Runtime {
        let dom = Rc::new(RefCell::new(Dom {
            fetch_tx,
            fetches: 0,
            sockets: HashMap::new(),
            streams: HashMap::new(),
            event_sources: HashMap::new(),
            session: opts.session,
            shadows: HashMap::new(),
            hosts: HashMap::new(),
            doc,
            dirty: false,
            timers: Vec::new(),
            pending_scripts: Vec::new(),
            ran: HashSet::new(),
            navigate: None,
            submit: None,
            values: HashMap::new(),
            debug: opts.debug,
            dark: opts.dark,
            started: Instant::now(),
        }));
        let modules = Rc::new(crate::modules::WebModules::new(opts.fetch_text.clone(), dom.borrow().doc.url.clone(), opts.debug));
        let hooks = Rc::new(Hooks::default());
        let mut cx = Context::builder().module_loader(modules.clone()).host_hooks(hooks.clone()).build().expect("a JS context");
        // Runaway loops end with an exception instead of a stuck thread.
        cx.runtime_limits_mut().set_loop_iteration_limit(200_000_000);
        cx.runtime_limits_mut().set_recursion_limit(2048);
        install_natives(&mut cx, &dom);
        if let Err(e) = cx.eval(Source::from_bytes(PRELUDE)) {
            eprintln!("js prelude failed: {e}");
        }
        let mut rt = Runtime { cx, dom, rx, updates, listening, opts, modules, hooks, last_sent: Instant::now(), flush_at: None };
        rt.dom.borrow_mut().dirty = false;
        rt.refresh_listening();
        rt
    }

    fn log_error(&self, what: &str, e: impl std::fmt::Display) {
        if self.opts.debug {
            eprintln!("js error in {what}: {e}");
        }
    }

    /// Run a script's source with `document.currentScript` set.
    fn exec(&mut self, node: Option<NodeId>, code: &str, what: &str) {
        let id = node.map_or(JsValue::null(), |n| JsValue::from(node_to_number(n)));
        self.call_global("__setCurrentScript", &[id]);
        if let Err(e) = self.cx.eval(Source::from_bytes(code)) {
            // With the JS stack, when there is one.
            let stack = e
                .clone()
                .into_opaque(&mut self.cx)
                .ok()
                .and_then(|v| v.as_object())
                .and_then(|o| o.get(js_string!("stack"), &mut self.cx).ok())
                .and_then(|s| s.as_string())
                .map(|s| s.to_std_string_escaped());
            match stack {
                Some(stack) => self.log_error(what, format!("{e}\n{stack}")),
                None => self.log_error(what, e),
            }
        }
        self.call_global("__setCurrentScript", &[JsValue::null()]);
        if let Err(e) = self.cx.run_jobs() {
            self.log_error("a promise job", e);
        }
    }

    fn call_global(&mut self, name: &str, args: &[JsValue]) -> JsValue {
        let global = self.cx.global_object();
        let f = global.get(JsString::from(name), &mut self.cx).ok().and_then(|f| f.as_callable());
        let Some(f) = f else { return JsValue::undefined() };
        match f.call(&JsValue::undefined(), args, &mut self.cx) {
            Ok(v) => v,
            Err(e) => {
                self.log_error(name, e);
                JsValue::undefined()
            }
        }
    }

    /// Run a module (it may import others, and await).
    fn exec_module(&mut self, code: &str, url: &Url) {
        self.call_global("__setCurrentScript", &[JsValue::null()]);
        let module = match self.modules.parse(code, url, &mut self.cx) {
            Ok(m) => m,
            Err(e) => return self.log_error(url.as_str(), e),
        };
        let promise = module.load_link_evaluate(&mut self.cx);
        self.cx.run_jobs().ok();
        if let boa_engine::builtins::promise::PromiseState::Rejected(e) = promise.state() {
            let text = e.as_object().and_then(|o| o.get(js_string!("stack"), &mut self.cx).ok()).and_then(|s| s.as_string());
            let message = e.to_string(&mut self.cx).map(|s| s.to_std_string_escaped()).unwrap_or_default();
            match text {
                Some(stack) => self.log_error(url.as_str(), format!("{message}\n{}", stack.to_std_string_escaped())),
                None => self.log_error(url.as_str(), message),
            }
        }
    }

    /// The script's source: inline text, or fetched (and its URL).
    fn script_source(&self, node: NodeId) -> Option<(String, String)> {
        let dom = self.dom.borrow();
        let e = dom.doc.element(node)?;
        if !runnable(e) {
            return None;
        }
        match e.attr("src") {
            Some(src) => {
                let url = dom.doc.resolve_url(src)?;
                drop(dom);
                let code = (self.opts.fetch_text)(&url)?;
                Some((code, url.to_string()))
            }
            None => Some((dom.doc.text_content(node), "inline script".into())),
        }
    }

    fn run_script(&mut self, node: NodeId) {
        if !self.dom.borrow_mut().ran.insert(node) {
            return;
        }
        let module = self.dom.borrow().doc.element(node).is_some_and(is_module);
        if let Some((code, what)) = self.script_source(node) {
            let t = Instant::now();
            if module {
                let url = Url::parse(&what).unwrap_or_else(|_| self.dom.borrow().doc.url.clone());
                self.exec_module(&code, &url);
            } else {
                self.exec(Some(node), &code, &what);
            }
            if self.opts.debug {
                eprintln!("js: ran {what} ({} bytes) in {:?}", code.len(), t.elapsed());
            }
        }
    }

    pub fn run_initial_scripts(&mut self) {
        let scripts: Vec<(NodeId, bool, bool)> = {
            let dom = self.dom.borrow();
            let doc = &dom.doc;
            doc.descendants(doc.root())
                .filter_map(|n| {
                    let e = doc.element(n).filter(|e| &**e.local_name() == "script" && runnable(e))?;
                    let external = e.attr("src").is_some();
                    // Modules are deferred unless async.
                    let module = is_module(e);
                    let is_async = (external || module) && e.attr("async").is_some();
                    Some((n, !is_async && (module || (external && e.attr("defer").is_some())), is_async))
                })
                .collect()
        };
        // Parser order, then deferred, then async ones.
        for &(n, defer, is_async) in &scripts {
            if !defer && !is_async {
                self.run_script(n);
                self.run_pending_scripts();
            }
        }
        for &(n, defer, _) in &scripts {
            if defer {
                self.run_script(n);
            }
        }
        self.call_global("__setReadyState", &[JsValue::from(js_string!("interactive"))]);
        self.call_global("__fireDocument", &[JsValue::from(js_string!("DOMContentLoaded"))]);
        for &(n, _, is_async) in &scripts {
            if is_async {
                self.run_script(n);
            }
        }
        self.run_pending_scripts();
        self.call_global("__setReadyState", &[JsValue::from(js_string!("complete"))]);
        self.call_global("__fireWindow", &[JsValue::from(js_string!("load"))]);
        // Zero-delay timers set during load still make the first paint.
        for _ in 0..8 {
            if !self.run_due_timers() {
                break;
            }
            self.run_pending_scripts();
        }
        self.cx.run_jobs().ok();
        self.run_pending_scripts();
        self.cx.run_jobs().ok();
    }

    fn run_pending_scripts(&mut self) {
        for _ in 0..64 {
            let pending = std::mem::take(&mut self.dom.borrow_mut().pending_scripts);
            if pending.is_empty() {
                return;
            }
            for n in pending {
                self.run_script(n);
                // `onload` for injected scripts (loaders chain on it).
                self.call_global("__fireNode", &[JsValue::from(node_to_number(n)), JsValue::from(js_string!("load"))]);
            }
        }
    }

    /// Fire the timers that are due; whether any were.
    fn run_due_timers(&mut self) -> bool {
        let now = Instant::now();
        let due: Vec<u32> = {
            let mut dom = self.dom.borrow_mut();
            let (due, later): (Vec<_>, Vec<_>) = dom.timers.drain(..).partition(|(t, _)| *t <= now);
            dom.timers = later;
            let mut due = due;
            due.sort_by_key(|(t, _)| *t);
            due.into_iter().map(|(_, id)| id).collect()
        };
        for id in &due {
            self.call_global("__fireTimer", &[JsValue::from(*id)]);
            self.cx.run_jobs().ok();
        }
        !due.is_empty()
    }

    /// The DOM as it is now, as the page renders it: shadow trees flattened
    /// into their hosts (for the first paint and the tab).
    pub fn snapshot_doc(&mut self) -> Document {
        self.refresh_listening();
        let styles = if self.dom.borrow().shadows.is_empty() { None } else { Some(self.call_global("__shadowStyles", &[])) };
        let styles = styles.map(|v| shadow_styles(v, &mut self.cx)).unwrap_or_default();
        let mut dom = self.dom.borrow_mut();
        dom.dirty = false;
        self.last_sent = Instant::now();
        flatten(&dom, styles)
    }

    fn refresh_listening(&mut self) {
        let ids = self.call_global("__listeningIds", &[]);
        let mut set = HashSet::new();
        if let Some(arr) = ids.as_object().and_then(|o| JsArray::from_object(o).ok()) {
            let len = arr.length(&mut self.cx).unwrap_or(0);
            for i in 0..len {
                if let Ok(v) = arr.get(i, &mut self.cx)
                    && let Some(n) = v.as_number()
                {
                    set.insert(node_from_number(n));
                }
            }
        }
        *self.listening.write().unwrap_or_else(|e| e.into_inner()) = set;
    }

    /// Send the tab the DOM if it changed (at most every `SNAPSHOT_EVERY`).
    fn flush(&mut self) {
        let navigate = self.dom.borrow_mut().navigate.take();
        if let Some(url) = navigate {
            self.updates.send_blocking(FromJs::Navigate(url)).ok();
        }
        let submit = self.dom.borrow_mut().submit.take();
        if let Some(form) = submit {
            // The tab reads the fields from the DOM it has: send that first.
            self.dom.borrow_mut().dirty = true;
            self.last_sent = Instant::now() - SNAPSHOT_EVERY;
            self.flush_doc();
            self.updates.send_blocking(FromJs::Submit(form)).ok();
            return;
        }
        self.flush_doc();
    }

    fn flush_doc(&mut self) {
        if !self.dom.borrow().dirty {
            self.flush_at = None;
            return;
        }
        let ready = self.last_sent + SNAPSHOT_EVERY;
        if Instant::now() < ready {
            self.flush_at = Some(ready);
            return;
        }
        self.flush_at = None;
        let doc = self.snapshot_doc();
        self.updates.send_blocking(FromJs::Document(Arc::new(doc))).ok();
    }

    pub fn event_loop(&mut self) {
        // Whatever the first paint missed.
        self.dom.borrow_mut().dirty = true;
        self.last_sent = Instant::now() - SNAPSHOT_EVERY;
        self.flush();
        loop {
            self.settle();
            let next_timer = self.dom.borrow().timers.iter().map(|(t, _)| *t).min();
            let deadline = match (next_timer, self.flush_at) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            match self.rx.recv_timeout(until(deadline)) {
                Ok(ToJs::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// One message from the tab, the network or a socket.
    fn handle(&mut self, msg: ToJs) {
        match msg {
            ToJs::Shutdown => {}
            ToJs::Click { node, reply } => {
                let prevented = self.call_global("__dispatchClick", &[JsValue::from(node_to_number(node))]);
                if let Some(reply) = reply {
                    reply.send(prevented.to_boolean()).ok();
                }
            }
            ToJs::FetchDone { id, result } => self.fetch_done(id, result),
            ToJs::FetchHead { id, head } => {
                let args = response_args(id, Ok(head), &mut self.cx);
                self.call_global("__fetchHead", &args);
            }
            ToJs::FetchChunk { id, bytes } => {
                self.call_global("__fetchChunk", &[JsValue::from(id), js_str(&latin1(&bytes))]);
            }
            ToJs::Sse { id, kind, event, data, last_id } => {
                self.call_global("__sseEvent", &[JsValue::from(id), js_str(kind), js_str(&event), js_str(&data), js_str(&last_id)]);
            }
            ToJs::Socket { id, event } => self.socket_event(id, event),
            ToJs::Checked { node, checked } => {
                self.call_global("__dispatchChecked", &[JsValue::from(node_to_number(node)), JsValue::from(checked)]);
            }
            ToJs::Submit { form, reply } => {
                let prevented = self.call_global("__dispatchSubmit", &[JsValue::from(node_to_number(form))]);
                reply.send(prevented.to_boolean()).ok();
            }
            ToJs::Input { node, value } => {
                self.dom.borrow_mut().values.insert(node, value);
                self.call_global("__dispatchInput", &[JsValue::from(node_to_number(node))]);
            }
        }
    }

    /// `__socketEvent(id, kind, data, code, reason, clean)`.
    fn socket_event(&mut self, id: u32, event: crate::websocket::SocketEvent) {
        use crate::websocket::SocketEvent as E;
        let latin1 = |b: &[u8]| -> String { b.iter().map(|&c| c as char).collect() };
        let (kind, data, code, reason, clean) = match event {
            E::Open { protocol } => ("open", protocol, 0, String::new(), true),
            E::Text(t) => ("text", t, 0, String::new(), true),
            E::Binary(b) => ("binary", latin1(&b), 0, String::new(), true),
            E::Error(e) => {
                if self.opts.debug {
                    eprintln!("js: websocket {id}: {e}");
                }
                ("error", e, 0, String::new(), false)
            }
            E::Close { code, reason, clean } => {
                self.dom.borrow_mut().sockets.remove(&id);
                ("close", String::new(), code, reason, clean)
            }
        };
        self.call_global(
            "__socketEvent",
            &[JsValue::from(id), js_str(kind), js_str(&data), JsValue::from(code), js_str(&reason), JsValue::from(clean)],
        );
    }

    /// Hand a finished request to its promise (or XHR).
    fn fetch_done(&mut self, id: u32, result: Result<crate::cors::ScriptResponse, String>) {
        let mut dom = self.dom.borrow_mut();
        dom.fetches = dom.fetches.saturating_sub(1);
        dom.streams.remove(&id);
        drop(dom);
        let args = response_args(id, result, &mut self.cx);
        self.call_global("__fetchDone", &args);
    }

    /// Before the first paint: keep running timers and requests for up to
    /// `budget` (or until nothing's pending).
    pub fn drain_for(&mut self, budget: Duration) {
        let deadline = Instant::now() + budget;
        loop {
            self.settle_quiet();
            let soon = self.dom.borrow().timers.iter().any(|(t, _)| *t < deadline);
            if self.dom.borrow().fetches == 0 && !soon {
                return;
            }
            let next_timer = self.dom.borrow().timers.iter().map(|(t, _)| *t).min();
            let wait = next_timer.map_or(deadline, |t| t.min(deadline));
            if Instant::now() >= deadline {
                return;
            }
            match self.rx.recv_timeout(until(Some(wait))) {
                Ok(ToJs::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Say which promises were rejected with nobody listening.
    fn report_rejections(&mut self) {
        let rejected = std::mem::take(&mut *self.hooks.rejected.borrow_mut());
        if self.opts.debug {
            for (_, why) in rejected {
                eprintln!("js: unhandled rejection: {why}");
            }
        }
    }

    /// `settle` without sending anything.
    fn settle_quiet(&mut self) {
        for _ in 0..32 {
            let timers = self.run_due_timers();
            let scripts = !self.dom.borrow().pending_scripts.is_empty();
            self.run_pending_scripts();
            self.cx.run_jobs().ok();
            if !timers && !scripts && self.dom.borrow().pending_scripts.is_empty() {
                break;
            }
        }
        self.report_rejections();
    }

    /// Do everything that's ready (timers due, inserted scripts, promise
    /// jobs, until none of them makes more), then send the DOM.
    fn settle(&mut self) {
        for _ in 0..32 {
            let timers = self.run_due_timers();
            let scripts = !self.dom.borrow().pending_scripts.is_empty();
            self.run_pending_scripts();
            self.cx.run_jobs().ok();
            if !timers && !scripts && self.dom.borrow().pending_scripts.is_empty() {
                break;
            }
        }
        self.report_rejections();
        self.flush();
    }
}

// ---------------------------------------------------------------------------
// Natives

fn arg_node(dom: &Dom, args: &[JsValue], i: usize) -> Option<NodeId> {
    let id = node_from_number(args.get_or_undefined(i).as_number()?);
    dom.doc.get(id).map(|_| id)
}

fn arg_string(args: &[JsValue], i: usize, cx: &mut Context) -> JsResult<String> {
    Ok(args.get_or_undefined(i).to_string(cx)?.to_std_string_escaped())
}

fn js_str(s: &str) -> JsValue {
    JsValue::from(JsString::from(s))
}

fn id_value(id: Option<NodeId>) -> JsValue {
    id.map_or(JsValue::null(), |n| JsValue::from(node_to_number(n)))
}

fn id_array(ids: impl IntoIterator<Item = NodeId>, cx: &mut Context) -> JsValue {
    JsArray::from_iter(ids.into_iter().map(|n| JsValue::from(node_to_number(n))), cx).into()
}

fn install_natives(cx: &mut Context, dom: &Shared) {
    let obj = JsObject::with_object_proto(cx.intrinsics());
    let mut add = |name: &str, len: usize, f: NativeBody| {
        let d = dom.clone();
        // SAFETY: the closure captures only the Rc'd DOM, which holds no
        // garbage-collected values.
        let func = unsafe { NativeFunction::from_closure(move |_this, args, cx| f(&d, args, cx)) };
        let fobj = FunctionObjectBuilder::new(cx.realm(), func).name(JsString::from(name)).length(len).build();
        obj.set(JsString::from(name), fobj, false, cx).ok();
    };

    add("root", 0, Box::new(|d, _, _| Ok(JsValue::from(node_to_number(d.borrow().doc.root())))));
    add("nodeType", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        let Some(n) = arg_node(&dom, a, 0) else { return Ok(JsValue::from(0)) };
        Ok(JsValue::from(match &dom.doc.node(n).data {
            NodeData::Element(_) => 1,
            NodeData::Text(_) => 3,
            NodeData::Comment(_) => 8,
            NodeData::Document => 9,
            NodeData::Doctype { .. } => 10,
            NodeData::Fragment => 11,
        }))
    }));
    add("tag", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        Ok(arg_node(&dom, a, 0).and_then(|n| dom.doc.element(n)).map_or(JsValue::null(), |e| js_str(&e.name.local)))
    }));
    add("parent", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        Ok(id_value(arg_node(&dom, a, 0).and_then(|n| dom.doc.parent(n))))
    }));
    add("children", 1, Box::new(|d, a, cx| {
        let dom = d.borrow();
        let kids = arg_node(&dom, a, 0).map(|n| dom.doc.children(n).to_vec()).unwrap_or_default();
        drop(dom);
        Ok(id_array(kids, cx))
    }));
    add("text", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        Ok(arg_node(&dom, a, 0).map_or(js_str(""), |n| match &dom.doc.node(n).data {
            NodeData::Text(t) | NodeData::Comment(t) => js_str(t),
            _ => js_str(&dom.doc.text_content(n)),
        }))
    }));
    add("setText", 2, Box::new(|d, a, cx| {
        let text = arg_string(a, 1, cx)?;
        let mut dom = d.borrow_mut();
        let Some(n) = arg_node(&dom, a, 0) else { return Ok(JsValue::undefined()) };
        dom.dirty = true;
        match dom.doc.data_mut(n) {
            Some(NodeData::Text(t)) | Some(NodeData::Comment(t)) => *t = text,
            _ => {
                for c in dom.doc.children(n).to_vec() {
                    dom.doc.detach(c);
                }
                if !text.is_empty() {
                    let t = dom.doc.create_text(text);
                    dom.doc.append(n, t);
                }
            }
        }
        Ok(JsValue::undefined())
    }));
    add("getAttr", 2, Box::new(|d, a, cx| {
        let name = arg_string(a, 1, cx)?.to_ascii_lowercase();
        let dom = d.borrow();
        Ok(arg_node(&dom, a, 0).and_then(|n| dom.doc.element(n)).and_then(|e| e.attr(&name)).map_or(JsValue::null(), js_str))
    }));
    add("setAttr", 3, Box::new(|d, a, cx| {
        let name = arg_string(a, 1, cx)?.to_ascii_lowercase();
        let value = arg_string(a, 2, cx)?;
        let mut dom = d.borrow_mut();
        let Some(n) = arg_node(&dom, a, 0) else { return Ok(JsValue::undefined()) };
        if let Some(e) = dom.doc.element_mut(n) {
            set_attr(e, &name, value);
            dom.dirty = true;
        }
        Ok(JsValue::undefined())
    }));
    add("removeAttr", 2, Box::new(|d, a, cx| {
        let name = arg_string(a, 1, cx)?.to_ascii_lowercase();
        let mut dom = d.borrow_mut();
        let Some(n) = arg_node(&dom, a, 0) else { return Ok(JsValue::undefined()) };
        if let Some(e) = dom.doc.element_mut(n) {
            let before = e.attrs.len();
            e.attrs.retain(|at| &*at.name.local != name.as_str());
            if e.attrs.len() != before {
                dom.dirty = true;
            }
        }
        Ok(JsValue::undefined())
    }));
    add("attrNames", 1, Box::new(|d, a, cx| {
        let dom = d.borrow();
        let names: Vec<JsValue> = arg_node(&dom, a, 0)
            .and_then(|n| dom.doc.element(n))
            .map(|e| e.attrs.iter().map(|at| js_str(&at.name.local)).collect())
            .unwrap_or_default();
        drop(dom);
        Ok(JsArray::from_iter(names, cx).into())
    }));
    // createElement(tag, namespace?): SVG elements keep their case.
    add("createElement", 2, Box::new(|d, a, cx| {
        let tag = arg_string(a, 0, cx)?;
        let svg = a.get_or_undefined(1).as_string().is_some_and(|ns| ns.to_std_string_escaped() == SVG_NS);
        let name = if svg {
            page::markup5ever::QualName::new(None, page::markup5ever::ns!(svg), tag.as_str().into())
        } else {
            page::html_name(&tag.to_ascii_lowercase())
        };
        let id = d.borrow_mut().doc.create_element(name, Vec::new());
        Ok(JsValue::from(node_to_number(id)))
    }));
    add("namespace", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        Ok(arg_node(&dom, a, 0).and_then(|n| dom.doc.element(n)).map_or(JsValue::null(), |e| js_str(&e.name.ns)))
    }));
    add("createText", 1, Box::new(|d, a, cx| {
        let text = arg_string(a, 0, cx)?;
        Ok(JsValue::from(node_to_number(d.borrow_mut().doc.create_text(text))))
    }));
    add("createComment", 1, Box::new(|d, a, cx| {
        let text = arg_string(a, 0, cx)?;
        Ok(JsValue::from(node_to_number(d.borrow_mut().doc.create(NodeData::Comment(text)))))
    }));
    add("createFragment", 0, Box::new(|d, _, _| Ok(JsValue::from(node_to_number(d.borrow_mut().doc.create(NodeData::Fragment))))));
    // insert(parent, child, before|null): moves the child (a fragment's children).
    add("insert", 3, Box::new(|d, a, _| {
        let mut dom = d.borrow_mut();
        let (Some(parent), Some(child)) = (arg_node(&dom, a, 0), arg_node(&dom, a, 1)) else { return Ok(JsValue::undefined()) };
        let before = arg_node(&dom, a, 2).filter(|b| dom.doc.parent(*b) == Some(parent));
        // Never into itself.
        let mut p = Some(parent);
        while let Some(x) = p {
            if x == child {
                return Ok(JsValue::undefined());
            }
            p = dom.doc.parent(x);
        }
        let moving: Vec<NodeId> = if matches!(dom.doc.node(child).data, NodeData::Fragment) {
            dom.doc.children(child).to_vec()
        } else {
            vec![child]
        };
        for m in moving {
            dom.doc.detach(m);
            match before {
                Some(b) => dom.doc.insert_before(b, m),
                None => dom.doc.append(parent, m),
            }
            queue_scripts(&mut dom, m);
        }
        dom.dirty = true;
        Ok(JsValue::undefined())
    }));
    add("remove", 1, Box::new(|d, a, _| {
        let mut dom = d.borrow_mut();
        if let Some(n) = arg_node(&dom, a, 0)
            && dom.doc.parent(n).is_some()
        {
            dom.doc.detach(n);
            dom.dirty = true;
        }
        Ok(JsValue::undefined())
    }));
    add("query", 3, Box::new(|d, a, cx| {
        let sel = arg_string(a, 1, cx)?;
        let all = a.get_or_undefined(2).to_boolean();
        let Some(selectors) = style::query::Selectors::parse(&sel) else {
            return Err(boa_engine::JsNativeError::syntax().with_message(format!("'{sel}' is not a valid selector")).into());
        };
        let dom = d.borrow();
        let Some(root) = arg_node(&dom, a, 0) else { return Ok(JsValue::null()) };
        if all {
            let found = selectors.query_all(&dom.doc, root);
            drop(dom);
            Ok(id_array(found, cx))
        } else {
            let found = dom.doc.descendants(root).find(|&n| n != root && selectors.matches(&dom.doc, n));
            Ok(id_value(found))
        }
    }));
    add("matches", 2, Box::new(|d, a, cx| {
        let sel = arg_string(a, 1, cx)?;
        let dom = d.borrow();
        let ok = arg_node(&dom, a, 0)
            .zip(style::query::Selectors::parse(&sel))
            .is_some_and(|(n, s)| s.matches(&dom.doc, n));
        Ok(JsValue::from(ok))
    }));
    add("innerHTML", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        let mut out = String::new();
        if let Some(n) = arg_node(&dom, a, 0) {
            for &c in dom.doc.children(n) {
                serialize(&dom.doc, c, &mut out);
            }
        }
        Ok(js_str(&out))
    }));
    add("outerHTML", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        let mut out = String::new();
        if let Some(n) = arg_node(&dom, a, 0) {
            serialize(&dom.doc, n, &mut out);
        }
        Ok(js_str(&out))
    }));
    // parseHTML(context tag, html) → a fragment holding the parsed nodes.
    add("parseHTML", 2, Box::new(|d, a, cx| {
        let tag = arg_string(a, 0, cx)?;
        let html = arg_string(a, 1, cx)?;
        let mut dom = d.borrow_mut();
        let svg = a.get_or_undefined(2).as_string().is_some_and(|ns| ns.to_std_string_escaped() == SVG_NS);
        let parsed = html::parse_fragment(&html, if tag.is_empty() { "div" } else { &tag }, svg, dom.doc.url.clone());
        let frag = dom.doc.create(NodeData::Fragment);
        // The fragment parser puts the nodes under an <html> element.
        let holder = parsed.children(parsed.root()).first().copied();
        if let Some(holder) = holder {
            for &c in parsed.children(holder) {
                let copy = dom.doc.import(&parsed, c);
                dom.doc.append(frag, copy);
            }
        }
        Ok(JsValue::from(node_to_number(frag)))
    }));
    add("clone", 2, Box::new(|d, a, _| {
        let deep = a.get_or_undefined(1).to_boolean();
        let mut dom = d.borrow_mut();
        let Some(n) = arg_node(&dom, a, 0) else { return Ok(JsValue::null()) };
        let copy = if deep {
            let snapshot = dom.doc.clone();
            dom.doc.import(&snapshot, n)
        } else {
            let data = dom.doc.node(n).data.clone();
            dom.doc.create(data)
        };
        Ok(JsValue::from(node_to_number(copy)))
    }));
    add("setTimer", 2, Box::new(|d, a, _| {
        let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
        let ms = a.get_or_undefined(1).as_number().unwrap_or(0.0).max(0.0);
        d.borrow_mut().timers.push((Instant::now() + Duration::from_secs_f64(ms / 1000.0), id));
        Ok(JsValue::undefined())
    }));
    add("clearTimer", 1, Box::new(|d, a, _| {
        let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
        d.borrow_mut().timers.retain(|(_, t)| *t != id);
        Ok(JsValue::undefined())
    }));
    add("log", 2, Box::new(|d, a, cx| {
        let level = arg_string(a, 0, cx)?;
        let text = arg_string(a, 1, cx)?;
        if d.borrow().debug {
            eprintln!("console.{level}: {text}");
        }
        Ok(JsValue::undefined())
    }));
    add("url", 0, Box::new(|d, _, _| Ok(js_str(d.borrow().doc.url.as_str()))));
    add("submitForm", 1, Box::new(|d, a, _| {
        let mut dom = d.borrow_mut();
        dom.submit = arg_node(&dom, a, 0);
        Ok(JsValue::undefined())
    }));
    add("navigate", 1, Box::new(|d, a, cx| {
        let href = arg_string(a, 0, cx)?;
        let mut dom = d.borrow_mut();
        if let Some(url) = dom.doc.resolve_url(&href) {
            dom.navigate = Some(url);
        }
        Ok(JsValue::undefined())
    }));
    add("resolve", 1, Box::new(|d, a, cx| {
        let href = arg_string(a, 0, cx)?;
        Ok(d.borrow().doc.resolve_url(&href).map_or(JsValue::null(), |u| js_str(u.as_str())))
    }));
    // resolveAgainst(href, base|null): `new URL(href, base)`.
    add("resolveAgainst", 2, Box::new(|d, a, cx| {
        let href = arg_string(a, 0, cx)?;
        let base = match a.get_or_undefined(1) {
            v if v.is_null_or_undefined() => None,
            v => Some(v.to_string(cx)?.to_std_string_escaped()),
        };
        let resolved = match base {
            Some(b) => Url::parse(&b).ok().and_then(|b| b.join(&href).ok()),
            None => Url::parse(&href).ok().or_else(|| d.borrow().doc.resolve_url(&href)),
        };
        Ok(resolved.map_or(JsValue::null(), |u| js_str(u.as_str())))
    }));
    // A separate (detached) document with <html><head></head><body></body>:
    // `createHTMLDocument`, `DOMParser` (jQuery parses snippets in one).
    add("createDocument", 0, Box::new(|d, _, _| {
        let mut dom = d.borrow_mut();
        let doc = &mut dom.doc;
        let root = doc.create(NodeData::Document);
        let html = doc.create_element(page::html_name("html"), Vec::new());
        let head = doc.create_element(page::html_name("head"), Vec::new());
        let body = doc.create_element(page::html_name("body"), Vec::new());
        doc.append(root, html);
        doc.append(html, head);
        doc.append(html, body);
        Ok(JsValue::from(node_to_number(root)))
    }));
    // The system's IANA time zone ("Europe/Paris"): Boa reports UTC.
    add("timeZone", 0, Box::new(|_, _, _| {
        let tz = std::env::var("TZ").ok().filter(|t| t.contains('/')).or_else(|| {
            let link = std::fs::read_link("/etc/localtime").ok()?;
            let s = link.to_string_lossy().into_owned();
            s.split("zoneinfo/").nth(1).map(str::to_owned)
        });
        Ok(js_str(tz.as_deref().unwrap_or("UTC")))
    }));
    // attachShadow(host) → the new shadow root (a fragment we know about).
    add("attachShadow", 1, Box::new(|d, a, _| {
        let mut dom = d.borrow_mut();
        let Some(host) = arg_node(&dom, a, 0) else { return Ok(JsValue::null()) };
        if let Some(&root) = dom.shadows.get(&host) {
            return Ok(JsValue::from(node_to_number(root)));
        }
        let root = dom.doc.create(NodeData::Fragment);
        dom.shadows.insert(host, root);
        dom.hosts.insert(root, host);
        dom.dirty = true;
        Ok(JsValue::from(node_to_number(root)))
    }));
    // In the document (through shadow hosts)?
    add("connected", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        let Some(mut n) = arg_node(&dom, a, 0) else { return Ok(JsValue::from(false)) };
        loop {
            if n == dom.doc.root() {
                return Ok(JsValue::from(true));
            }
            match dom.doc.parent(n).or_else(|| dom.hosts.get(&n).copied()) {
                Some(p) => n = p,
                None => return Ok(JsValue::from(false)),
            }
        }
    }));
    add("shadowHost", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        Ok(id_value(arg_node(&dom, a, 0).and_then(|n| dom.hosts.get(&n).copied())))
    }));
    // Something only scripts can see changed how the page looks (a
    // constructed stylesheet): send a new DOM.
    add("touch", 0, Box::new(|d, _, _| {
        d.borrow_mut().dirty = true;
        Ok(JsValue::undefined())
    }));
    add("trace", 0, Box::new(|_, _, _| Ok(JsValue::from(std::env::var_os("FEATHER_JS_TRACE").is_some()))));
    add("dark", 0, Box::new(|d, _, _| Ok(JsValue::from(d.borrow().dark))));
    add("now", 0, Box::new(|d, _, _| Ok(JsValue::from(d.borrow().started.elapsed().as_secs_f64() * 1000.0))));
    add("value", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        let Some(n) = arg_node(&dom, a, 0) else { return Ok(JsValue::null()) };
        Ok(dom.values.get(&n).map_or(JsValue::null(), |v| js_str(v)))
    }));
    add("setValue", 2, Box::new(|d, a, cx| {
        let v = arg_string(a, 1, cx)?;
        let mut dom = d.borrow_mut();
        if let Some(n) = arg_node(&dom, a, 0) {
            dom.values.insert(n, v.clone());
            // Reflected into the attribute, which the page renders.
            if let Some(e) = dom.doc.element_mut(n) {
                set_attr(e, "value", v);
            }
            dom.dirty = true;
        }
        Ok(JsValue::undefined())
    }));
    // fetch(id, method, url, [[name, value]...], body|null, mode, credentials):
    // runs on its own thread, answers through `__fetchDone`.
    add("fetch", 7, Box::new(|d, a, cx| {
        let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
        let req = script_request(d, a, cx)?;
        let mut dom = d.borrow_mut();
        let page = dom.doc.url.clone();
        let tx = dom.fetch_tx.clone();
        dom.fetches += 1;
        let debug = dom.debug;
        // Set when the page asks for the body as a stream.
        let streaming = Arc::new(std::sync::atomic::AtomicBool::new(false));
        dom.streams.insert(id, streaming.clone());
        drop(dom);
        let Some(req) = req else {
            tx.send(ToJs::FetchDone { id, result: Err("invalid URL".into()) }).ok();
            return Ok(JsValue::undefined());
        };
        std::thread::Builder::new()
            .name("feather-fetch".into())
            .spawn(move || {
                let t = Instant::now();
                let what = format!("{} {}", req.method, req.url);
                let mut head = None;
                let mut body: Vec<u8> = Vec::new();
                let mut sent = 0;
                let result = crate::cors::fetch_streaming(
                    &page,
                    req,
                    |h| {
                        tx.send(ToJs::FetchHead { id, head: h.clone() }).ok();
                        head = Some(h);
                    },
                    |chunk| {
                        body.extend_from_slice(chunk);
                        if streaming.load(std::sync::atomic::Ordering::Relaxed) {
                            tx.send(ToJs::FetchChunk { id, bytes: body[sent..].to_vec() }).ok();
                            sent = body.len();
                        }
                        true
                    },
                );
                if streaming.load(std::sync::atomic::Ordering::Relaxed) && sent < body.len() {
                    tx.send(ToJs::FetchChunk { id, bytes: body[sent..].to_vec() }).ok();
                }
                let result = result.and_then(|()| {
                    let mut r = head.ok_or_else(|| "no response".to_owned())?;
                    r.body = body;
                    Ok(r)
                });
                if debug {
                    match &result {
                        Ok(r) => eprintln!("js: fetch {what} → {} ({} bytes) in {:?}", r.status, r.body.len(), t.elapsed()),
                        Err(e) => eprintln!("js: fetch {what} failed: {e}"),
                    }
                }
                tx.send(ToJs::FetchDone { id, result }).ok();
            })
            .ok();
        Ok(JsValue::undefined())
    }));
    // The page wants this response's body as a stream from now on.
    add("fetchStream", 1, Box::new(|d, a, _| {
        let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
        if let Some(flag) = d.borrow().streams.get(&id) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(JsValue::undefined())
    }));
    // sseOpen(id, url, withCredentials): an EventSource connection.
    add("sseOpen", 3, Box::new(|d, a, cx| {
        let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
        let href = arg_string(a, 1, cx)?;
        let credentials = a.get_or_undefined(2).to_boolean();
        let mut dom = d.borrow_mut();
        let Some(url) = dom.doc.resolve_url(&href) else {
            return Err(boa_engine::JsNativeError::syntax().with_message(format!("invalid EventSource URL {href}")).into());
        };
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        dom.event_sources.insert(id, closed.clone());
        crate::sse::open(id, dom.doc.url.clone(), url.clone(), credentials, closed, dom.fetch_tx.clone());
        Ok(js_str(url.as_str()))
    }));
    add("sseClose", 1, Box::new(|d, a, _| {
        let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
        if let Some(flag) = d.borrow_mut().event_sources.remove(&id) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(JsValue::undefined())
    }));
    // wsOpen(id, url, [protocols]): connect in the background.
    add("wsOpen", 3, Box::new(|d, a, cx| {
        let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
        let href = arg_string(a, 1, cx)?;
        let mut protocols = Vec::new();
        if let Some(list) = a.get_or_undefined(2).as_object().and_then(|o| JsArray::from_object(o).ok()) {
            for i in 0..list.length(cx)? {
                protocols.push(list.get(i, cx)?.to_string(cx)?.to_std_string_escaped());
            }
        }
        let mut dom = d.borrow_mut();
        let url = dom.doc.resolve_url(&href).map(|mut u| {
            // http(s): URLs mean ws(s):.
            let scheme = match u.scheme() {
                "http" => Some("ws"),
                "https" => Some("wss"),
                _ => None,
            };
            if let Some(s) = scheme {
                let _ = u.set_scheme(s);
            }
            u
        });
        let Some(url) = url.filter(|u| matches!(u.scheme(), "ws" | "wss")) else {
            return Err(boa_engine::JsNativeError::syntax().with_message(format!("The URL '{href}' is invalid.")).into());
        };
        let origin = dom.doc.url.origin().ascii_serialization();
        let tx = crate::websocket::open(id, url.to_string(), protocols, origin, dom.fetch_tx.clone());
        dom.sockets.insert(id, tx);
        Ok(js_str(url.as_str()))
    }));
    // wsSend(id, data, binary): binary data comes as a latin1 string.
    add("wsSend", 3, Box::new(|d, a, cx| {
        let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
        let data = arg_string(a, 1, cx)?;
        let binary = a.get_or_undefined(2).to_boolean();
        let dom = d.borrow();
        if let Some(tx) = dom.sockets.get(&id) {
            let msg = if binary {
                crate::websocket::Outgoing::Binary(data.chars().map(|c| c as u32 as u8).collect())
            } else {
                crate::websocket::Outgoing::Text(data)
            };
            tx.send(msg).ok();
        }
        Ok(JsValue::undefined())
    }));
    add("wsClose", 3, Box::new(|d, a, cx| {
        let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
        let code = a.get_or_undefined(1).as_number().unwrap_or(1000.0) as u16;
        let reason = arg_string(a, 2, cx)?;
        if let Some(tx) = d.borrow().sockets.get(&id) {
            tx.send(crate::websocket::Outgoing::Close(code, reason)).ok();
        }
        Ok(JsValue::undefined())
    }));
    // ---- storage: storage(kind "local"|"session", op, key, value) ----
    add("storage", 4, Box::new(|d, a, cx| {
        use crate::storage::{Kind, with};
        let dom = d.borrow();
        let kind = if arg_string(a, 0, cx)? == "session" { Kind::Session(dom.session) } else { Kind::Local };
        // Opaque origins (data:) get nothing that lasts.
        let origin = match dom.doc.url.origin() {
            o if o.is_tuple() => o.ascii_serialization(),
            _ if dom.doc.url.scheme() == "file" => "file://".to_owned(),
            _ => return Ok(JsValue::null()),
        };
        drop(dom);
        let op = arg_string(a, 1, cx)?;
        let key = arg_string(a, 2, cx)?;
        Ok(match op.as_str() {
            "get" => with(kind, &origin, |area| (area.get(&key).cloned(), false)).map_or(JsValue::null(), |v| js_str(&v)),
            "set" => {
                let value = arg_string(a, 3, cx)?;
                with(kind, &origin, |area| ((), area.insert(key, value.clone()).as_ref() != Some(&value)));
                JsValue::undefined()
            }
            "remove" => {
                with(kind, &origin, |area| ((), area.remove(&key).is_some()));
                JsValue::undefined()
            }
            "clear" => {
                with(kind, &origin, |area| {
                    let had = !area.is_empty();
                    area.clear();
                    ((), had)
                });
                JsValue::undefined()
            }
            _ => {
                let keys: Vec<JsValue> = with(kind, &origin, |area| (area.keys().map(|k| js_str(k)).collect(), false));
                JsArray::from_iter(keys, cx).into()
            }
        })
    }));
    add("cookie", 0, Box::new(|d, _, _| Ok(js_str(&net::script_cookies(&d.borrow().doc.url)))));
    add("setCookie", 1, Box::new(|d, a, cx| {
        let text = arg_string(a, 0, cx)?;
        let url = d.borrow().doc.url.clone();
        net::set_script_cookie(&url, &text);
        Ok(JsValue::undefined())
    }));
    // ---- crypto (bytes travel as latin1 strings) ----
    add("randomBytes", 1, Box::new(|_, a, _| {
        use ring::rand::SecureRandom;
        let n = a.get_or_undefined(0).as_number().unwrap_or(0.0).clamp(0.0, 65536.0) as usize;
        let mut buf = vec![0u8; n];
        ring::rand::SystemRandom::new().fill(&mut buf).ok();
        Ok(js_str(&latin1(&buf)))
    }));
    add("digest", 2, Box::new(|_, a, cx| {
        let alg = arg_string(a, 0, cx)?.to_ascii_uppercase().replace('-', "");
        let data = bytes_arg(a, 1, cx)?;
        let alg = match alg.as_str() {
            "SHA1" => &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
            "SHA256" => &ring::digest::SHA256,
            "SHA384" => &ring::digest::SHA384,
            "SHA512" => &ring::digest::SHA512,
            _ => return Err(boa_engine::JsNativeError::typ().with_message(format!("unsupported digest {alg}")).into()),
        };
        Ok(js_str(&latin1(ring::digest::digest(alg, &data).as_ref())))
    }));
    // hmac(hash, key, data) → signature
    add("hmac", 3, Box::new(|_, a, cx| {
        let alg = match arg_string(a, 0, cx)?.to_ascii_uppercase().replace('-', "").as_str() {
            "SHA1" => ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
            "SHA384" => ring::hmac::HMAC_SHA384,
            "SHA512" => ring::hmac::HMAC_SHA512,
            _ => ring::hmac::HMAC_SHA256,
        };
        let key = ring::hmac::Key::new(alg, &bytes_arg(a, 1, cx)?);
        Ok(js_str(&latin1(ring::hmac::sign(&key, &bytes_arg(a, 2, cx)?).as_ref())))
    }));
    // ed25519Generate() → [pkcs8, public key]
    add("ed25519Generate", 0, Box::new(|_, _, cx| {
        use ring::signature::KeyPair;
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| boa_engine::JsNativeError::error().with_message("key generation failed"))?;
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| boa_engine::JsNativeError::error().with_message("bad key"))?;
        Ok(JsArray::from_iter([js_str(&latin1(pkcs8.as_ref())), js_str(&latin1(pair.public_key().as_ref()))], cx).into())
    }));
    // ed25519Public(pkcs8) → public key
    add("ed25519Public", 1, Box::new(|_, a, cx| {
        use ring::signature::KeyPair;
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(&bytes_arg(a, 0, cx)?)
            .map_err(|_| boa_engine::JsNativeError::typ().with_message("invalid Ed25519 private key"))?;
        Ok(js_str(&latin1(pair.public_key().as_ref())))
    }));
    add("ed25519Sign", 2, Box::new(|_, a, cx| {
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(&bytes_arg(a, 0, cx)?)
            .map_err(|_| boa_engine::JsNativeError::typ().with_message("invalid Ed25519 private key"))?;
        Ok(js_str(&latin1(pair.sign(&bytes_arg(a, 1, cx)?).as_ref())))
    }));
    // ed25519Verify(public key, data, signature) → bool
    add("ed25519Verify", 3, Box::new(|_, a, cx| {
        let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, bytes_arg(a, 0, cx)?);
        Ok(JsValue::from(key.verify(&bytes_arg(a, 1, cx)?, &bytes_arg(a, 2, cx)?).is_ok()))
    }));
    // The same, blocking (synchronous XMLHttpRequest): the response arguments.
    add("fetchSync", 7, Box::new(|d, a, cx| {
        let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
        let page = d.borrow().doc.url.clone();
        let result = match script_request(d, a, cx)? {
            Some(req) => crate::cors::fetch(&page, req),
            None => Err("invalid URL".into()),
        };
        let args = response_args(id, result, cx);
        Ok(JsArray::from_iter(args, cx).into())
    }));
    cx.register_global_property(js_string!("__native"), obj, PropAttr::all()).ok();
}

/// The request in a `fetch`/`fetchSync` call's arguments (`None`: bad URL).
fn script_request(d: &Shared, a: &[JsValue], cx: &mut Context) -> JsResult<Option<crate::cors::ScriptRequest>> {
    let method = arg_string(a, 1, cx)?;
    let href = arg_string(a, 2, cx)?;
    let mut headers = Vec::new();
    if let Some(list) = a.get_or_undefined(3).as_object().and_then(|o| JsArray::from_object(o).ok()) {
        let len = list.length(cx)?;
        for i in 0..len {
            let pair = list.get(i, cx)?;
            if let Some(pair) = pair.as_object() {
                let k = pair.get(0, cx)?.to_string(cx)?.to_std_string_escaped();
                let v = pair.get(1, cx)?.to_string(cx)?.to_std_string_escaped();
                headers.push((k, v));
            }
        }
    }
    let body = match a.get_or_undefined(4) {
        v if v.is_null_or_undefined() => None,
        v => Some(v.to_string(cx)?.to_std_string_escaped().into_bytes()),
    };
    let mode = arg_string(a, 5, cx)?;
    let credentials = arg_string(a, 6, cx)?;
    let url = d.borrow().doc.resolve_url(&href);
    Ok(url.map(|url| crate::cors::ScriptRequest { method, url, headers, body, mode, credentials }))
}

/// `__fetchDone`'s arguments: (id, error|null, status, statusText, url,
/// [[name, value]...], body text, type, redirected).
fn response_args(id: u32, result: Result<crate::cors::ScriptResponse, String>, cx: &mut Context) -> Vec<JsValue> {
    match result {
        Err(e) => vec![JsValue::from(id), js_str(&e)],
        Ok(r) => {
            let headers: Vec<JsValue> = r
                .headers
                .iter()
                .map(|(k, v)| JsArray::from_iter([js_str(k), js_str(v)], cx).into())
                .collect();
            vec![
                JsValue::from(id),
                JsValue::null(),
                JsValue::from(r.status),
                js_str(&r.status_text),
                js_str(r.url.as_str()),
                JsArray::from_iter(headers, cx).into(),
                js_str(&String::from_utf8_lossy(&r.body)),
                js_str(r.kind),
                JsValue::from(r.redirected),
            ]
        }
    }
}

fn set_attr(e: &mut ElementData, name: &str, value: String) {
    match e.attrs.iter_mut().find(|at| &*at.name.local == name) {
        Some(at) => at.value = value,
        None => e.attrs.push(Attribute { name: page::markup5ever::QualName::new(None, page::markup5ever::ns!(), name.into()), value }),
    }
}

/// Scripts that just got connected to the document run after this task.
fn queue_scripts(dom: &mut Dom, id: NodeId) {
    // Connected?
    let mut p = Some(id);
    let mut connected = false;
    while let Some(x) = p {
        if x == dom.doc.root() {
            connected = true;
            break;
        }
        p = dom.doc.parent(x);
    }
    if !connected {
        if dom.debug && dom.doc.element(id).is_some_and(|e| &**e.local_name() == "script") {
            eprintln!("js: inserted script not connected");
        }
        return;
    }
    let found: Vec<NodeId> = dom
        .doc
        .descendants(id)
        .filter(|&n| dom.doc.element(n).is_some_and(|e| &**e.local_name() == "script" && runnable(e)))
        .filter(|n| !dom.ran.contains(n))
        .collect();
    if dom.debug && !found.is_empty() {
        eprintln!("js: queued {} inserted script(s)", found.len());
    }
    dom.pending_scripts.extend(found);
}

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"];

fn serialize(doc: &Document, id: NodeId, out: &mut String) {
    match &doc.node(id).data {
        NodeData::Text(t) => {
            let raw = doc.parent(id).and_then(|p| doc.element(p)).is_some_and(|e| matches!(&*e.name.local, "script" | "style"));
            if raw {
                out.push_str(t);
            } else {
                escape(t, false, out);
            }
        }
        NodeData::Comment(t) => {
            out.push_str("<!--");
            out.push_str(t);
            out.push_str("-->");
        }
        NodeData::Element(e) => {
            out.push('<');
            out.push_str(&e.name.local);
            for at in &e.attrs {
                out.push(' ');
                out.push_str(&at.name.local);
                out.push_str("=\"");
                escape(&at.value, true, out);
                out.push('"');
            }
            out.push('>');
            if VOID.contains(&&*e.name.local) {
                return;
            }
            for &c in doc.children(id) {
                serialize(doc, c, out);
            }
            out.push_str("</");
            out.push_str(&e.name.local);
            out.push('>');
        }
        NodeData::Document | NodeData::Fragment => {
            for &c in doc.children(id) {
                serialize(doc, c, out);
            }
        }
        NodeData::Doctype { name } => {
            out.push_str("<!DOCTYPE ");
            out.push_str(name);
            out.push('>');
        }
    }
}

fn escape(s: &str, attr: bool, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' if !attr => out.push_str("&lt;"),
            '>' if !attr => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
}

/// `__shadowStyles()`'s answer: [[host id | null, css], ...] (null: the
/// document's own adopted sheets).
fn shadow_styles(v: JsValue, cx: &mut Context) -> Vec<(Option<NodeId>, String)> {
    let mut out = Vec::new();
    let Some(arr) = v.as_object().and_then(|o| JsArray::from_object(o).ok()) else { return out };
    let len = arr.length(cx).unwrap_or(0);
    for i in 0..len {
        let Some(pair) = arr.get(i, cx).ok().and_then(|p| p.as_object()) else { continue };
        let host = pair.get(0, cx).ok().and_then(|h| h.as_number()).map(node_from_number);
        let css = pair.get(1, cx).ok().and_then(|c| c.as_string()).map(|c| c.to_std_string_escaped()).unwrap_or_default();
        out.push((host, css));
    }
    out
}

/// A copy of the DOM as it renders: each shadow host's children are its
/// shadow root's, `<slot>`s hold the light children assigned to them, and
/// the shadow trees' (scoped) styles are `<style>` elements in their hosts.
/// Node ids are the real ones, so clicks and hovers land on real nodes.
fn flatten(dom: &Dom, styles: Vec<(Option<NodeId>, String)>) -> Document {
    let mut copy = dom.doc.clone();
    if dom.shadows.is_empty() {
        return copy;
    }
    let src = &dom.doc;
    let is_tag = |n: NodeId, t: &str| src.element(n).is_some_and(|e| &*e.name.local == t);
    for (&host, &root) in &dom.shadows {
        if src.get(host).is_none() {
            continue;
        }
        // Light children by the slot they ask for ("" = the default slot).
        let mut assigned: HashMap<String, Vec<NodeId>> = HashMap::new();
        for &c in src.children(host) {
            let name = src.element(c).and_then(|e| e.attr("slot")).unwrap_or("").to_owned();
            assigned.entry(name).or_default().push(c);
        }
        // Slots in this shadow tree (not in shadow trees nested in it).
        let mut stack: Vec<NodeId> = src.children(root).to_vec();
        while let Some(n) = stack.pop() {
            if is_tag(n, "slot") {
                let name = src.element(n).and_then(|e| e.attr("name")).unwrap_or("").to_owned();
                if let Some(nodes) = assigned.remove(&name) {
                    copy.set_children(n, nodes);
                }
                continue;
            }
            stack.extend(src.children(n).iter().copied());
        }
        // The shadow tree's own <style>s are replaced by scoped copies.
        let kids: Vec<NodeId> = src.children(root).iter().copied().filter(|&c| !is_tag(c, "style")).collect();
        copy.set_children(host, kids);
        if let Some(e) = copy.element_mut(host) {
            set_attr(e, "data-feather-host", format!("{}", node_to_number(host)));
        }
    }
    // Scoped shadow styles at the top of their hosts; the document's
    // adopted sheets in <head>.
    let head = copy.descendants(copy.root()).find(|&n| copy.element(n).is_some_and(|e| &*e.name.local == "head"));
    for (host, css) in styles {
        if css.trim().is_empty() {
            continue;
        }
        let style = copy.create_element(page::html_name("style"), Vec::new());
        let text = copy.create_text(css);
        copy.append(style, text);
        if let Some(parent) = host.or(head) {
            let mut kids = copy.children(parent).to_vec();
            kids.insert(0, style);
            copy.set_children(parent, kids);
        }
    }
    copy
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// A latin1-string argument as bytes.
fn bytes_arg(a: &[JsValue], i: usize, cx: &mut Context) -> JsResult<Vec<u8>> {
    Ok(arg_string(a, i, cx)?.chars().map(|c| c as u32 as u8).collect())
}
