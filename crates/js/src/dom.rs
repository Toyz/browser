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

impl Dom {
    /// The document changed: send it, and recompute styles asked of it.
    pub(crate) fn debug(&self) -> bool {
        self.debug
    }

    /// The DOM changed, somewhere (for a forced layout: possibly anything).
    pub(crate) fn changed(&mut self) {
        self.mark_dirty();
        self.touched_all = true;
    }

    /// The DOM changed at `n` (its attributes, text or children).
    pub(crate) fn changed_at(&mut self, n: NodeId) {
        self.mark_dirty();
        if self.touched.len() < 256 {
            self.touched.insert(n);
        } else {
            self.touched_all = true;
        }
    }

    /// The tab needs a new snapshot (nothing scripts read changed).
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
        self.generation += 1;
    }

    /// `n`'s parent for layout: a shadow root's is its host.
    fn layout_parent(&self, n: NodeId) -> Option<NodeId> {
        self.doc.parent(n).or_else(|| self.hosts.get(&n).copied())
    }

    /// Whether what changed since the last snapshot can move or resize
    /// `n`: it, an ancestor or a descendant changed.
    pub(crate) fn changed_near(&self, n: NodeId) -> bool {
        if self.touched_all {
            return true;
        }
        let mut chain = HashSet::new();
        let mut at = Some(n);
        while let Some(x) = at {
            if self.touched.contains(&x) {
                return true;
            }
            chain.insert(x);
            at = self.layout_parent(x);
        }
        self.touched.iter().any(|&t| {
            let mut at = self.layout_parent(t);
            while let Some(x) = at {
                if x == n {
                    return true;
                }
                if chain.contains(&x) {
                    return false;
                }
                at = self.layout_parent(x);
            }
            false
        })
    }
}

#[path = "geometry.rs"]
pub(crate) mod geometry;
#[path = "observers.rs"]
pub(crate) mod observers;
#[path = "context2d.rs"]
pub(crate) mod context2d;

/// The page's DOM, for natives that capture nothing (class methods): kept
/// in the context.
struct PageDom(Shared);

pub(crate) fn page(cx: &Context) -> Shared {
    cx.get_data::<PageDom>().expect("natives installed").0.clone()
}

/// `href` resolved against the page's base URL (a worker's script URL in a
/// worker).
pub(crate) fn resolve(cx: &Context, href: &str) -> Option<Url> {
    match cx.get_data::<PageDom>() {
        Some(p) => p.0.borrow().doc.resolve_url(href),
        None => cx.get_data::<crate::worker::Base>()?.0.join(href).ok(),
    }
}

/// Make native class `C` an `EventTarget` (the prelude's; in a worker,
/// whatever `EventTarget` there is): its prototype and constructor chain.
pub(crate) fn extend_event_target<C: boa_engine::class::Class>(cx: &mut Context) {
    let Some(target) = cx.global_object().get(js_string!("EventTarget"), cx).ok().and_then(|t| t.as_object()) else { return };
    let Some(proto) = target.get(js_string!("prototype"), cx).ok().and_then(|p| p.as_object()) else { return };
    if let Some(class) = cx.get_global_class::<C>() {
        class.prototype().set_prototype(Some(proto));
        class.constructor().set_prototype(Some(target));
    }
}

pub(crate) fn is_module(e: &ElementData) -> bool {
    e.attr("type").is_some_and(|t| t.trim().eq_ignore_ascii_case("module"))
}

/// Node sets the tab reads (shared with it).
pub(crate) struct Published {
    listening: Arc<RwLock<HashSet<NodeId>>>,
    measured: Arc<RwLock<HashSet<NodeId>>>,
    mouse: Arc<RwLock<HashSet<NodeId>>>,
}

pub(crate) struct Dom {
    pub doc: Document,
    pub dirty: bool,
    /// Bumped on every change (computed styles are good until it moves).
    pub generation: u64,
    /// For `getComputedStyle`.
    pub(crate) styles: crate::computed::StyleCache,
    /// `<style>` sheets as scripts changed them through the CSSOM, for
    /// `getComputedStyle` (refreshed when `cssom_dirty`).
    cssom: HashMap<NodeId, String>,
    cssom_dirty: bool,
    /// (due, id)
    pub(crate) timers: Vec<(Instant, u32)>,
    /// Scripts inserted by scripts, to run after the current task.
    pending_scripts: Vec<NodeId>,
    ran: HashSet<NodeId>,
    navigate: Option<Url>,
    /// `<canvas>` bitmaps (2D contexts' drawing, replayed).
    canvases: HashMap<NodeId, crate::canvas::Canvas>,
    /// `pushState` / `replaceState` since the last flush: (url, replace).
    url_changes: Vec<(Url, bool)>,
    /// `history.go(n)` asked of the tab.
    traverse: Option<i32>,
    /// `form.submit()` for the tab to carry out.
    submit: Option<NodeId>,
    /// Where scripts moved focus since the last flush (`Some(None)`: blurred).
    focus: Option<Option<NodeId>>,
    /// Form field values from the page (by node).
    values: HashMap<NodeId, String>,
    debug: bool,
    dark: bool,
    started: Instant,
    /// To the tab (`locate`'s questions, scrolling).
    tab: async_channel::Sender<FromJs>,
    /// What the tab reads between snapshots: elements with listeners,
    /// measured ones, ones with mouse listeners.
    published: Published,
    /// When the tab was last sent the DOM.
    last_sent: Instant,
    /// For workers' scripts.
    pub(crate) fetch_text: crate::FetchText,
    /// Where the DOM changed since then (`touched_all`: anywhere).
    touched: HashSet<NodeId>,
    touched_all: bool,
    /// Canvases with drawing recorded since their pixels were last sent.
    canvases_dirty: bool,
    /// Drawing recorded on each canvas since it was last replayed.
    canvas_ops: HashMap<NodeId, Vec<crate::canvas::Op>>,
    /// Canvases drawn on since their pixels were last sent.
    drawn: HashSet<NodeId>,
    /// When each canvas was last drawn on (detached ones are let go of,
    /// beyond the few most recent: see `forget_canvases`).
    canvas_used: HashMap<NodeId, Instant>,
    /// How far the page is scrolled (`window.scrollX`/`scrollY`), px.
    scroll: (f32, f32),
    /// The tab has sent a layout: it's showing the page and answers
    /// `locate` (before that, nobody's reading the questions).
    laid_out: bool,
    /// Where the tab laid measured elements out, and the viewport.
    geometry: geometry::Geometry,
    /// Shadow roots by host (and back).
    shadows: HashMap<NodeId, NodeId>,
    hosts: HashMap<NodeId, NodeId>,
    /// Where finished requests report back.
    pub(crate) fetch_tx: mpsc::Sender<ToJs>,
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

pub(crate) type Shared = Rc<RefCell<Dom>>;
/// A native function's body.
type NativeBody = Box<dyn Fn(&Shared, &[JsValue], &mut Context) -> JsResult<JsValue>>;

pub(crate) struct Runtime {
    /// Canvas pixels for the tab (see `FromJs::CanvasFrames`).
    canvas_out: crate::CanvasFrames,
    /// The element under the pointer (for `mouseenter` and friends).
    hover: Option<NodeId>,
    cx: Context,
    dom: Shared,
    rx: mpsc::Receiver<ToJs>,
    /// Messages taken off `rx` while picking out user input mid-task (see
    /// `take_input`), still to be handled in order.
    backlog: std::collections::VecDeque<ToJs>,
    updates: async_channel::Sender<FromJs>,
    opts: Options,
    modules: Rc<crate::modules::WebModules>,
    hooks: Rc<Hooks>,
    flush_at: Option<Instant>,
    /// When scripts started: past the first-paint budget (the tab shows
    /// what it has), the initial scripts' progress is sent as it happens.
    started: Instant,
}

impl Runtime {
    pub fn new(
        doc: Document,
        opts: Options,
        rx: mpsc::Receiver<ToJs>,
        fetch_tx: mpsc::Sender<ToJs>,
        updates: async_channel::Sender<FromJs>,
        listening: Arc<RwLock<HashSet<NodeId>>>,
        measured: Arc<RwLock<HashSet<NodeId>>>,
        mouse_listening: Arc<RwLock<HashSet<NodeId>>>,
        canvas_out: crate::CanvasFrames,
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
            generation: 0,
            styles: Default::default(),
            cssom: HashMap::new(),
            cssom_dirty: false,
            timers: Vec::new(),
            pending_scripts: Vec::new(),
            ran: HashSet::new(),
            navigate: None,
            focus: None,
            url_changes: Vec::new(),
            traverse: None,
            canvases: HashMap::new(),
            submit: None,
            values: HashMap::new(),
            debug: opts.debug,
            dark: opts.dark,
            started: Instant::now(),
            tab: updates.clone(),
            published: Published { listening: listening.clone(), measured: measured.clone(), mouse: mouse_listening.clone() },
            last_sent: Instant::now(),
            fetch_text: opts.fetch_text.clone(),
            touched: HashSet::new(),
            touched_all: false,
            scroll: (0.0, 0.0),
            laid_out: false,
            geometry: Default::default(),
            canvases_dirty: false,
            canvas_ops: HashMap::new(),
            drawn: HashSet::new(),
            canvas_used: HashMap::new(),
        }));
        let modules = Rc::new(crate::modules::WebModules::new(opts.fetch_text.clone(), dom.borrow().doc.url.clone(), opts.debug));
        let hooks = Rc::new(Hooks::default());
        let mut cx = Context::builder().module_loader(modules.clone()).host_hooks(hooks.clone()).build().expect("a JS context");
        // Runaway loops end with an exception instead of a stuck thread.
        cx.runtime_limits_mut().set_loop_iteration_limit(std::env::var("JS_LOOP_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(200_000_000));
        // Deep enough for big framework bundles (Boa's value stack defaults
        // to 10k entries; browsers allow ~10k nested calls).
        cx.runtime_limits_mut().set_recursion_limit(10_000);
        cx.runtime_limits_mut().set_stack_size_limit(1 << 20);
        declarative_shadow_roots(&mut dom.borrow_mut());
        install_natives(&mut cx, &dom);
        crate::web_url::register(&mut cx);
        if let Some(native) = cx.global_object().get(js_string!("__native"), &mut cx).ok().and_then(|n| n.as_object()) {
            crate::encoding::register(&mut cx, &native);
        }
        if let Err(e) = cx.eval(Source::from_bytes(PRELUDE)) {
            eprintln!("js prelude failed: {e}");
        }
        crate::abort::link(&mut cx);
        crate::worker::link(&mut cx);
        let mut rt = Runtime { canvas_out, hover: None, cx, dom, rx, backlog: Default::default(), updates, opts, modules, hooks, flush_at: None, started: Instant::now() };
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
        // Named by URL, so stack traces say where (inline scripts: the page).
        let path = if what.contains("://") { what.to_owned() } else { format!("{} (inline)", self.dom.borrow().doc.url) };
        if let Err(e) = self.cx.eval(Source::from_bytes(code).with_path(std::path::Path::new(&path))) {
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
    fn exec_module(&mut self, code: &str, url: &Url, external: bool) {
        self.call_global("__setCurrentScript", &[JsValue::null()]);
        let parsed = if external { self.modules.entry(code, url, &mut self.cx) } else { self.modules.parse(code, url, &mut self.cx) };
        let module = match parsed {
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
                let t = Instant::now();
                let code = (self.opts.fetch_text)(&url)?;
                if self.opts.debug && t.elapsed() > Duration::from_millis(20) {
                    eprintln!("js: waited {:?} for {url}", t.elapsed());
                }
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
                let parsed = Url::parse(&what);
                let external = parsed.is_ok();
                let url = parsed.unwrap_or_else(|_| self.dom.borrow().doc.url.clone());
                self.exec_module(&code, &url, external);
            } else {
                self.exec(Some(node), &code, &what);
            }
            if self.opts.debug {
                eprintln!("js: ran {what} ({} bytes) in {:?}", code.len(), t.elapsed());
            }
        }
        // A page whose scripts take longer than the first paint waits for
        // shows them as they go, not all at the end (Microsoft's ~400 took
        // 13s before anything changed on screen).
        if self.started.elapsed() > self.opts.budget {
            self.flush_doc();
        }
    }

    pub fn run_initial_scripts(&mut self) {
        // Import maps first: they say what the modules' bare imports mean.
        let maps: Vec<String> = {
            let dom = self.dom.borrow();
            let doc = &dom.doc;
            doc.descendants(doc.root())
                .filter(|&n| {
                    doc.element(n).is_some_and(|e| {
                        &**e.local_name() == "script" && e.attr("type").is_some_and(|t| t.trim().eq_ignore_ascii_case("importmap"))
                    })
                })
                .map(|n| doc.text_content(n))
                .collect()
        };
        for map in maps {
            self.modules.add_import_map(&map);
        }
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
            self.take_input();
        }
        !due.is_empty()
    }

    /// Element ancestors of `n`, itself first, across shadow boundaries.
    fn composed_chain(&self, n: NodeId) -> Vec<NodeId> {
        let dom = self.dom.borrow();
        let mut out = Vec::new();
        let mut node = Some(n);
        while let Some(c) = node {
            if dom.doc.element(c).is_some() {
                out.push(c);
            }
            node = dom.doc.parent(c).or_else(|| dom.hosts.get(&c).copied());
        }
        out
    }

    /// The pointer moved onto `target`: `out` / `over` (bubbling) at the
    /// old and new elements, `leave` / `enter` at each one it left or
    /// entered (parents before children).
    fn move_hover(&mut self, target: NodeId, x: f32, y: f32, buttons: u16) {
        let next = {
            let dom = self.dom.borrow();
            if dom.doc.element(target).is_some() { Some(target) } else { dom.doc.parent(target) }
        };
        let prev = self.hover;
        if prev == next {
            return;
        }
        self.hover = next;
        let was = prev.map(|p| self.composed_chain(p)).unwrap_or_default();
        let now = next.map(|n| self.composed_chain(n)).unwrap_or_default();
        let id = |n: Option<NodeId>| n.map_or(JsValue::null(), |n| JsValue::from(node_to_number(n)));
        let fire = |rt: &mut Self, kind: &str, at: NodeId, bubbles: bool, related: Option<NodeId>| {
            let args = [
                js_str(kind),
                JsValue::from(node_to_number(at)),
                JsValue::from(x),
                JsValue::from(y),
                JsValue::from(0),
                JsValue::from(buttons),
                JsValue::from(0),
                JsValue::from(0),
                JsValue::from(bubbles),
                id(related),
            ];
            rt.call_global("__dispatchMouse", &args);
        };
        if let Some(p) = prev.filter(|_| !was.is_empty()) {
            fire(self, "pointerout", p, true, next);
            fire(self, "mouseout", p, true, next);
            for &t in was.iter().take_while(|t| !now.contains(t)) {
                fire(self, "pointerleave", t, false, next);
                fire(self, "mouseleave", t, false, next);
            }
        }
        if let Some(n) = next {
            fire(self, "pointerover", n, true, prev);
            fire(self, "mouseover", n, true, prev);
            let entered: Vec<NodeId> = now.iter().copied().take_while(|t| !was.contains(t)).collect();
            for &t in entered.iter().rev() {
                fire(self, "pointerenter", t, false, prev);
                fire(self, "mouseenter", t, false, prev);
            }
        }
    }

    /// The DOM as it is now, as the page renders it (see [`snapshot`]).
    pub fn snapshot_doc(&mut self) -> Document {
        snapshot(&self.dom, &mut self.cx)
    }

    fn refresh_listening(&mut self) {
        refresh_listening(&self.dom, &mut self.cx);
    }

    /// Send the tab the DOM if it changed (at most every `SNAPSHOT_EVERY`).
    fn flush(&mut self) {
        let changes = std::mem::take(&mut self.dom.borrow_mut().url_changes);
        for (url, replace) in changes {
            self.updates.send_blocking(FromJs::UrlChanged { url, replace }).ok();
        }
        let traverse = self.dom.borrow_mut().traverse.take();
        if let Some(delta) = traverse {
            self.updates.send_blocking(FromJs::Traverse(delta)).ok();
        }
        let focus = self.dom.borrow_mut().focus.take();
        if let Some(node) = focus {
            self.updates.send_blocking(FromJs::Focus(node)).ok();
        }
        let navigate = self.dom.borrow_mut().navigate.take();
        if let Some(url) = navigate {
            self.updates.send_blocking(FromJs::Navigate(url)).ok();
        }
        self.flush_canvases();
        let submit = self.dom.borrow_mut().submit.take();
        if let Some(form) = submit {
            // The tab reads the fields from the DOM it has: send that first.
            self.dom.borrow_mut().changed();
            self.dom.borrow_mut().last_sent = Instant::now() - SNAPSHOT_EVERY;
            self.flush_doc();
            self.updates.send_blocking(FromJs::Submit(form)).ok();
            return;
        }
        self.flush_doc();
    }

    /// Draw what scripts drew on canvases since last time, and send the
    /// tab their pixels (no DOM snapshot: only a new canvas, or a resized
    /// one, changes the document).
    fn flush_canvases(&mut self) {
        if std::mem::take(&mut self.dom.borrow_mut().canvases_dirty) {
            context2d::draw_pending(&self.dom);
        }
        let nodes = std::mem::take(&mut self.dom.borrow_mut().drawn);
        if nodes.is_empty() {
            return;
        }
        forget_canvases(&mut self.dom.borrow_mut(), &nodes);
        let dom = self.dom.borrow();
        let mut out = self.canvas_out.lock().unwrap_or_else(|e| e.into_inner());
        // The tab is told once until it takes them.
        let tell = out.is_empty();
        for node in nodes {
            if let Some(c) = dom.canvases.get(&node) {
                let (width, height) = c.size();
                // (A frame still waiting gives its buffer to this one.)
                let mut pixels = out.remove(&node).map(|old| old.pixels).unwrap_or_default();
                pixels.clear();
                pixels.extend_from_slice(c.pixels());
                out.insert(node, crate::CanvasPixels { width, height, pixels });
            }
        }
        drop(out);
        if tell {
            self.updates.send_blocking(FromJs::CanvasFrames).ok();
        }
    }

    fn flush_doc(&mut self) {
        if !self.dom.borrow().dirty {
            self.flush_at = None;
            return;
        }
        let ready = self.dom.borrow().last_sent + SNAPSHOT_EVERY;
        if Instant::now() < ready {
            self.flush_at = Some(ready);
            return;
        }
        self.flush_at = None;
        let t = Instant::now();
        let doc = self.snapshot_doc();
        if self.opts.debug {
            eprintln!("perf: snapshot {} nodes in {:?}", doc.len(), t.elapsed());
        }
        self.updates.send_blocking(FromJs::Document(Arc::new(doc))).ok();
    }

    pub fn event_loop(&mut self) {
        // Whatever the first paint missed.
        self.dom.borrow_mut().changed();
        self.dom.borrow_mut().last_sent = Instant::now() - SNAPSHOT_EVERY;
        self.flush();
        loop {
            self.settle();
            let next_timer = self.dom.borrow().timers.iter().map(|(t, _)| *t).min();
            let deadline = match (next_timer, self.flush_at) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            match self.next_message(until(deadline)) {
                Ok(ToJs::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// The next message: what `take_input` set aside first, then the channel.
    fn next_message(&mut self, timeout: Duration) -> Result<ToJs, RecvTimeoutError> {
        match self.backlog.pop_front() {
            Some(msg) => Ok(msg),
            None => self.rx.recv_timeout(timeout),
        }
    }

    /// Between tasks, handle the user's input now (a click waits on whether
    /// scripts cancel it; a busy page's timers shouldn't make it wait), and
    /// set everything else aside, in order, for the event loop.
    fn take_input(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                ToJs::Click { .. }
                | ToJs::Submit { .. }
                | ToJs::Key { .. }
                | ToJs::Mouse { .. }
                | ToJs::Focus { .. }
                | ToJs::Input { .. }
                | ToJs::Checked { .. }
                    if self.backlog.is_empty() =>
                {
                    self.handle(msg)
                }
                other => self.backlog.push_back(other),
            }
        }
    }

    /// One message from the tab, the network or a socket.
    fn handle(&mut self, msg: ToJs) {
        match msg {
            ToJs::Shutdown => {}
            ToJs::Click { node, reply } => {
                let t = Instant::now();
                let prevented = self.call_global("__dispatchClick", &[JsValue::from(node_to_number(node))]);
                if self.opts.debug {
                    eprintln!("js: click on {node:?}: prevented {} in {:?}", prevented.to_boolean(), t.elapsed());
                }
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
                let chunk = crate::blob::array_buffer(&bytes, &mut self.cx).unwrap_or_default();
                self.call_global("__fetchChunk", &[JsValue::from(id), chunk]);
            }
            ToJs::Sse { id, kind, event, data, last_id } => {
                self.call_global("__sseEvent", &[JsValue::from(id), js_str(kind), js_str(&event), js_str(&data), js_str(&last_id)]);
            }
            ToJs::Socket { id, event } => self.socket_event(id, event),
            ToJs::Worker { id, event } => {
                if let Err(e) = crate::worker::deliver(id, event, &mut self.cx) {
                    self.log_error("worker message", e);
                }
            }
            ToJs::Key { kind, key, code, target, modifiers } => {
                let target = target.map_or(JsValue::null(), |n| JsValue::from(node_to_number(n)));
                let args = [
                    js_str(kind),
                    js_str(&key),
                    js_str(&code),
                    target,
                    JsValue::from(modifiers[0]),
                    JsValue::from(modifiers[1]),
                    JsValue::from(modifiers[2]),
                    JsValue::from(modifiers[3]),
                ];
                self.call_global("__dispatchKey", &args);
            }
            ToJs::Mouse { kind, node, x, y, button, buttons, delta } => {
                if kind == "mousemove" {
                    self.move_hover(node, x, y, buttons);
                }
                let args = [
                    js_str(kind),
                    JsValue::from(node_to_number(node)),
                    JsValue::from(x),
                    JsValue::from(y),
                    JsValue::from(button),
                    JsValue::from(buttons),
                    JsValue::from(delta.0),
                    JsValue::from(delta.1),
                ];
                self.call_global("__dispatchMouse", &args);
            }
            ToJs::Layout { rects, gone, viewport, scroll } => {
                let scrolled = std::mem::replace(&mut self.dom.borrow_mut().scroll, scroll) != scroll;
                let resized = {
                    let mut dom = self.dom.borrow_mut();
                    dom.laid_out = true;
                    dom.geometry.apply(&rects, &gone, viewport)
                };
                observers::deliver(&mut self.cx);
                self.call_global("__layout", &[JsValue::from(resized)]);
                if scrolled {
                    self.call_global("__fireWindow", &[js_str("scroll")]);
                }
            }
            ToJs::Focus { node } => {
                let id = node.map_or(JsValue::null(), |n| JsValue::from(node_to_number(n)));
                self.call_global("__setFocus", &[id]);
            }
            ToJs::Traverse { delta } => {
                self.call_global("__traverse", &[JsValue::from(delta)]);
            }
            ToJs::Image { id, size } => {
                {
                    let mut dom = self.dom.borrow_mut();
                    dom.fetches = dom.fetches.saturating_sub(1);
                }
                let (w, h) = size.unwrap_or((0, 0));
                self.call_global("__imageEvent", &[JsValue::from(id), JsValue::from(size.is_some()), JsValue::from(w), JsValue::from(h)]);
            }
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
        let mut binary = None;
        let (kind, data, code, reason, clean) = match event {
            E::Open { protocol } => ("open", protocol, 0, String::new(), true),
            E::Text(t) => ("text", t, 0, String::new(), true),
            E::Binary(b) => {
                binary = Some(b);
                ("binary", String::new(), 0, String::new(), true)
            }
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
        // Binary messages as ArrayBuffers.
        let data = match binary {
            Some(b) => crate::blob::array_buffer(&b, &mut self.cx).unwrap_or_default(),
            None => js_str(&data),
        };
        self.call_global(
            "__socketEvent",
            &[JsValue::from(id), js_str(kind), data, JsValue::from(code), js_str(&reason), JsValue::from(clean)],
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
            match self.next_message(until(Some(wait))) {
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

/// A value as a string (`String(v)`; "" where that throws).
pub(crate) fn js_value_string(v: &JsValue, cx: &mut Context) -> String {
    v.to_string(cx).map(|s| s.to_std_string_escaped()).unwrap_or_default()
}

/// The document's address becomes `href` (same origin only); `mode` 1
/// tells the tab it's a new history entry, 2 that it replaces the current
/// one, 0 nothing (a traversal the tab made).
pub(crate) fn set_url(d: &Shared, href: &str, mode: u8) -> JsResult<Url> {
    let mut dom = d.borrow_mut();
    let Some(url) = dom.doc.url.join(href).ok() else {
        return Err(boa_engine::JsNativeError::typ().with_message(format!("invalid URL {href}")).into());
    };
    if url.origin() != dom.doc.url.origin() {
        return Err(boa_engine::JsNativeError::typ()
            .with_message(format!("SecurityError: {url} isn't same-origin with {}", dom.doc.url))
            .into());
    }
    dom.doc.url = url.clone();
    if mode != 0 {
        dom.url_changes.push((url.clone(), mode == 2));
    }
    Ok(url)
}

/// In the document (through shadow hosts)?
fn is_connected(dom: &Dom, mut n: NodeId) -> bool {
    loop {
        if n == dom.doc.root() {
            return true;
        }
        match dom.doc.parent(n).or_else(|| dom.hosts.get(&n).copied()) {
            Some(p) => n = p,
            None => return false,
        }
    }
}

/// The media scripts see: the viewport the tab last reported.
fn media_env(dom: &Dom) -> style::MediaEnv {
    style::MediaEnv { width: dom.geometry.viewport.0, height: dom.geometry.viewport.1, dark: dom.dark, scripting: true }
}

/// Bring the CSSOM-changed sheets `getComputedStyle` sees up to date.
fn refresh_cssom(d: &Shared, cx: &mut Context) {
    if !std::mem::take(&mut d.borrow_mut().cssom_dirty) {
        return;
    }
    let Some(f) = cx.global_object().get(js_string!("__cssomStyles"), cx).ok().and_then(|f| f.as_callable()) else { return };
    let Ok(v) = f.call(&JsValue::undefined(), &[], cx) else { return };
    let texts = node_texts(v, cx).into_iter().filter_map(|(id, css)| Some((id?, css))).collect();
    d.borrow_mut().cssom = texts;
}

/// An element's computed `prop` (`size`: as laid out, for `width`/`height`).
fn computed_value(dom: &mut Dom, id: NodeId, prop: &str, size: Option<(f32, f32)>) -> String {
    let env = media_env(dom);
    let Dom { doc, styles, generation, dark, cssom, .. } = dom;
    let fetch = |u: &Url| net::fetch_text_cached(u);
    let Some(style) = styles.style_of(doc, id, *generation, env, &fetch, cssom) else { return String::new() };
    crate::computed::value(&style, prop, size, *dark)
}

/// The page's address.
pub(crate) fn url_of(dom: &Dom) -> Url {
    dom.doc.url.clone()
}

/// The tab's session (for `sessionStorage`).
pub(crate) fn session_of(dom: &Dom) -> u64 {
    dom.session
}

/// Milliseconds since the page started (`performance.now()`).
pub(crate) fn now_ms(d: &Shared) -> f64 {
    now_ms_of(&d.borrow())
}

/// ms since the page started (`performance.now()`).
pub(crate) fn now_ms_of(dom: &Dom) -> f64 {
    dom.started.elapsed().as_secs_f64() * 1000.0
}

/// Traverse session history by `delta` (the tab does it, then tells us).
pub(crate) fn request_traverse(d: &Shared, delta: i32) {
    d.borrow_mut().traverse = Some(delta);
}

/// Are scripts' console messages printed (debugging)?
pub(crate) fn debug_logs(d: &Shared) -> bool {
    d.borrow().debug
}

/// Go to `href` (resolved against the document), at the next flush.
pub(crate) fn navigate_to(d: &Shared, href: &str) {
    let mut dom = d.borrow_mut();
    if let Some(url) = dom.doc.resolve_url(href) {
        dom.navigate = Some(url);
    }
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
        // A text node's change is its element's.
        let at = if dom.doc.element(n).is_none() { dom.layout_parent(n).unwrap_or(n) } else { n };
        dom.changed_at(at);
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
    // CSSOM text: a sheet's top-level rules, a block's declarations.
    add("cssRules", 1, Box::new(|_, a, cx| {
        let css = arg_string(a, 0, cx)?;
        let rules: Vec<JsValue> = crate::cssom::rules(&css).iter().map(|r| js_str(r)).collect();
        Ok(JsArray::from_iter(rules, cx).into())
    }));
    add("cssDecls", 1, Box::new(|_, a, cx| {
        let block = arg_string(a, 0, cx)?;
        let decls: Vec<JsValue> = crate::cssom::declarations(&block)
            .into_iter()
            .map(|(name, value, important)| JsArray::from_iter([js_str(&name), js_str(&value), JsValue::from(important)], cx).into())
            .collect();
        Ok(JsArray::from_iter(decls, cx).into())
    }));
    add("getAttr", 2, Box::new(|d, a, cx| {
        let raw = arg_string(a, 1, cx)?;
        let dom = d.borrow();
        let Some(e) = arg_node(&dom, a, 0).and_then(|n| dom.doc.element(n)) else { return Ok(JsValue::null()) };
        let name = attr_name(e, raw);
        Ok(e.attr(&name).map_or(JsValue::null(), js_str))
    }));
    add("setAttr", 3, Box::new(|d, a, cx| {
        let raw = arg_string(a, 1, cx)?;
        let value = arg_string(a, 2, cx)?;
        let mut dom = d.borrow_mut();
        let Some(n) = arg_node(&dom, a, 0) else { return Ok(JsValue::undefined()) };
        if let Some(e) = dom.doc.element_mut(n) {
            let name = attr_name(e, raw);
            set_attr(e, &name, value);
            dom.changed_at(n);
        }
        Ok(JsValue::undefined())
    }));
    add("removeAttr", 2, Box::new(|d, a, cx| {
        let raw = arg_string(a, 1, cx)?;
        let mut dom = d.borrow_mut();
        let Some(n) = arg_node(&dom, a, 0) else { return Ok(JsValue::undefined()) };
        if let Some(e) = dom.doc.element_mut(n) {
            let name = attr_name(e, raw);
            let before = e.attrs.len();
            e.attrs.retain(|at| &*at.name.local != name.as_str());
            if e.attrs.len() != before {
                dom.changed_at(n);
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
        dom.changed_at(parent);
        Ok(JsValue::undefined())
    }));
    add("remove", 1, Box::new(|d, a, _| {
        let mut dom = d.borrow_mut();
        if let Some(n) = arg_node(&dom, a, 0)
            && let Some(parent) = dom.doc.parent(n)
        {
            dom.doc.detach(n);
            dom.changed_at(parent);
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
            dom.doc.clone_subtree(n)
        } else {
            let mut data = dom.doc.node(n).data.clone();
            // A shallow copy of a template starts with empty contents.
            if let NodeData::Element(e) = &mut data {
                e.template_contents = None;
            }
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
        if d.borrow().debug || (text.starts_with("IODBG") && std::env::var_os("FEATHER_DEBUG_IO").is_some()) {
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
        navigate_to(d, &arg_string(a, 0, cx)?);
        Ok(JsValue::undefined())
    }));
    // setUrl(href, mode): the document's address becomes `href` (same
    // origin only); mode 1 tells the tab it's a new history entry, 2 that
    // it replaces the current one, 0 nothing (popstate). Returns the URL.
    add("setUrl", 2, Box::new(|d, a, cx| {
        let href = arg_string(a, 0, cx)?;
        let mode = a.get_or_undefined(1).as_number().unwrap_or(0.0) as u8;
        Ok(js_str(set_url(d, &href, mode)?.as_str()))
    }));
    add("traverse", 1, Box::new(|d, a, _| {
        let delta = a.get_or_undefined(0).as_number().unwrap_or(0.0) as i32;
        d.borrow_mut().traverse = Some(delta);
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
        dom.changed_at(host);
        Ok(JsValue::from(node_to_number(root)))
    }));
    // [host, root, host, root...]: the shadow roots that exist (declarative
    // ones, made before the prelude ran).
    add("shadowPairs", 0, Box::new(|d, _, cx| {
        let dom = d.borrow();
        let values: Vec<JsValue> = dom.shadows.iter().flat_map(|(&h, &r)| [JsValue::from(node_to_number(h)), JsValue::from(node_to_number(r))]).collect();
        Ok(JsArray::from_iter(values, cx).into())
    }));
    // A parsed <template>'s contents fragment.
    add("templateContent", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        let Some(t) = arg_node(&dom, a, 0) else { return Ok(JsValue::null()) };
        Ok(dom.doc.element(t).and_then(|e| e.template_contents).map_or(JsValue::null(), |c| JsValue::from(node_to_number(c))))
    }));
    // In the document (through shadow hosts)?
    add("connected", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        Ok(JsValue::from(arg_node(&dom, a, 0).is_some_and(|n| is_connected(&dom, n))))
    }));
    // window.scrollX / scrollY (getters) and scrollTo / scroll / scrollBy:
    // `(x, y)` or `({ left, top, behavior })`.
    add("scrollX", 0, Box::new(|d, _, _| Ok(JsValue::from(d.borrow().scroll.0))));
    add("scrollY", 0, Box::new(|d, _, _| Ok(JsValue::from(d.borrow().scroll.1))));
    fn scroll_target(d: &Shared, a: &[JsValue], cx: &mut Context, relative: bool) -> JsResult<(f32, f32, bool)> {
        let now = d.borrow().scroll;
        let base = if relative { now } else { (0.0, 0.0) };
        let num = |v: &JsValue, cx: &mut Context| -> JsResult<Option<f32>> {
            if v.is_undefined() { Ok(None) } else { Ok(Some(v.to_number(cx)? as f32)).map(|n| n.filter(|n| n.is_finite())) }
        };
        let (x, y, smooth) = match a.first().and_then(|v| v.as_object()) {
            Some(o) => {
                let left = o.get(js_string!("left"), cx)?;
                let top = o.get(js_string!("top"), cx)?;
                let behavior = o.get(js_string!("behavior"), cx)?;
                let smooth = behavior.as_string().is_some_and(|b| b.to_std_string_escaped() == "smooth");
                (num(&left, cx)?, num(&top, cx)?, smooth)
            }
            None => (num(a.first().unwrap_or(&JsValue::undefined()), cx)?, num(a.get(1).unwrap_or(&JsValue::undefined()), cx)?, false),
        };
        Ok((x.map_or(now.0, |x| base.0 + x).max(0.0), y.map_or(now.1, |y| base.1 + y).max(0.0), smooth))
    }
    for (name, relative) in [("scrollTo", false), ("scrollBy", true)] {
        add(name, 2, Box::new(move |d, a, cx| {
            let (x, y, smooth) = scroll_target(d, a, cx, relative)?;
            let tab = {
                let mut dom = d.borrow_mut();
                // Scripts reading it right after see where they put it.
                dom.scroll = (x, y);
                dom.tab.clone()
            };
            tab.send_blocking(FromJs::ScrollTo { x, y, smooth }).ok();
            Ok(JsValue::undefined())
        }));
    }
    add("shadowHost", 1, Box::new(|d, a, _| {
        let dom = d.borrow();
        Ok(id_value(arg_node(&dom, a, 0).and_then(|n| dom.hosts.get(&n).copied())))
    }));
    // Something only scripts can see changed how the page looks (a
    // constructed stylesheet): send a new DOM.
    // Scripts focused an element (or blurred, with null): the tab follows.
    add("setFocus", 1, Box::new(|d, a, _| {
        let mut dom = d.borrow_mut();
        let node = arg_node(&dom, a, 0);
        dom.focus = Some(node);
        Ok(JsValue::undefined())
    }));
    // A sheet's rules changed through the CSSOM.
    add("cssomChanged", 0, Box::new(|d, _, _| {
        let mut dom = d.borrow_mut();
        dom.cssom_dirty = true;
        dom.changed();
        Ok(JsValue::undefined())
    }));
    add("touch", 0, Box::new(|d, _, _| {
        d.borrow_mut().changed();
        Ok(JsValue::undefined())
    }));
    // FEATHER_EVAL=probe.js (FEATHER_EVAL_DELAY ms): [source, delay] or null.
    add("debugScript", 0, Box::new(|_, _, cx| {
        let Some(src) = std::env::var_os("FEATHER_EVAL").and_then(|p| std::fs::read_to_string(p).ok()) else {
            return Ok(JsValue::null());
        };
        let delay: f64 = std::env::var("FEATHER_EVAL_DELAY").ok().and_then(|d| d.parse().ok()).unwrap_or(5000.0);
        Ok(JsArray::from_iter([JsValue::from(js_string!(src)), JsValue::from(delay)], cx).into())
    }));
    add("debugIO", 0, Box::new(|_, _, _| Ok(JsValue::from(std::env::var_os("FEATHER_DEBUG_IO").is_some()))));
    add("trace", 0, Box::new(|_, _, _| Ok(JsValue::from(std::env::var_os("FEATHER_JS_TRACE").is_some()))));
    add("dark", 0, Box::new(|d, _, _| Ok(JsValue::from(d.borrow().dark))));
    // getComputedStyle: (id, property); `width` and `height` as the tab
    // laid the element out (before that, a block's estimate).
    add("computedStyle", 2, Box::new(|d, a, cx| {
        let prop = arg_string(a, 1, cx)?;
        let Some(id) = arg_node(&d.borrow(), a, 0) else { return Ok(js_str("")) };
        refresh_cssom(d, cx);
        let size = match prop.as_str() {
            "width" | "height" => geometry::laid_out_size(d, id),
            _ => None,
        };
        Ok(js_str(&computed_value(&mut d.borrow_mut(), id, &prop, size)))
    }));
    // CSS.supports(property, value) / CSS.supports(condition).
    add("cssSupports", 2, Box::new(|_, a, cx| {
        let first = arg_string(a, 0, cx)?;
        let ok = match a.get(1).filter(|v| !v.is_undefined()) {
            Some(_) => style::supports::supports_declaration(&first, &arg_string(a, 1, cx)?),
            None => style::supports::supports(&first) || style::supports::supports(&format!("({first})")),
        };
        Ok(JsValue::from(ok))
    }));
    // matchMedia: a media query list against the viewport.
    add("mediaMatches", 1, Box::new(|d, a, cx| {
        let query = arg_string(a, 0, cx)?;
        let env = media_env(&d.borrow());
        Ok(JsValue::from(style::media::matches(&query, &env)))
    }));
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
            dom.changed_at(n);
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
    // imageLoad(id, url) / resourceLoad(id, url): fetch an image (or a
    // <link>'s resource) in the background.
    for (name, any) in [("imageLoad", false), ("resourceLoad", true)] {
        add(name, 2, Box::new(move |d, a, cx| {
            let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
            let href = arg_string(a, 1, cx)?;
            let mut dom = d.borrow_mut();
            // Counted like a fetch: the first paint waits a little for them.
            dom.fetches += 1;
            match dom.doc.resolve_url(&href) {
                Some(url) => crate::images::load(id, url, any, dom.fetch_tx.clone()),
                None => {
                    dom.fetch_tx.send(ToJs::Image { id, size: None }).ok();
                }
            }
            Ok(JsValue::undefined())
        }));
    }
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
    // wsSend(id, data): a string, or binary data.
    add("wsSend", 3, Box::new(|d, a, cx| {
        let id = a.get_or_undefined(0).as_number().unwrap_or(0.0) as u32;
        // Binary (ArrayBuffer, view, Blob) or text.
        let msg = match crate::blob::binary_of(a.get_or_undefined(1), cx)? {
            Some(bytes) => crate::websocket::Outgoing::Binary(bytes),
            None => crate::websocket::Outgoing::Text(arg_string(a, 1, cx)?),
        };
        let dom = d.borrow();
        if let Some(tx) = dom.sockets.get(&id) {
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
    // `document.all`: an object that's falsy and `== undefined` (Annex B's
    // [[IsHTMLDDA]]), so feature checks treat it as absent but `x ===
    // document.all` doesn't match `undefined`.
    add("htmlAll", 0, Box::new(|_, _, cx| {
        let proto = cx.intrinsics().constructors().object().prototype();
        Ok(JsObject::from_proto_and_data(Some(proto), boa_engine::builtins::is_html_dda::IsHTMLDDA).into())
    }));
    crate::dom_exception::install(cx);
    crate::dom_geometry::install(cx);
    crate::structured_clone::install(cx);
    crate::form_data::install(cx);
    crate::abort::install(cx);
    crate::request::install(cx);
    crate::worker::install(cx);
    crate::streams::install(cx);
    crate::window_api::install(&obj, dom, cx);
    crate::timers::install(&obj, dom.clone(), cx);
    crate::web_storage::install(&obj, dom, cx);
    geometry::install(&obj, dom, cx);
    cx.insert_data(PageDom(dom.clone()));
    observers::install(cx);
    context2d::install(&obj, cx);
    crate::crypto::install(cx);
    crate::blob::install(&obj, cx);
    crate::aria::install(&obj, cx);
    crate::headers::install(cx);
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
        // An ArrayBuffer (`encodeBody`'s binary bodies), else text.
        v => match crate::blob::binary_of(v, cx)? {
            Some(bytes) => Some(bytes),
            None => Some(v.to_string(cx)?.to_std_string_escaped().into_bytes()),
        },
    };
    let mode = arg_string(a, 5, cx)?;
    let credentials = arg_string(a, 6, cx)?;
    let url = d.borrow().doc.resolve_url(&href);
    Ok(url.map(|url| crate::cors::ScriptRequest { method, url, headers, body, mode, credentials }))
}

/// `__fetchDone`'s arguments: (id, error|null, status, statusText, url,
/// [[name, value]...], body (an ArrayBuffer), type, redirected).
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
                crate::blob::array_buffer(&r.body, cx).unwrap_or_default(),
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
    // Start downloading them now, together: each is otherwise fetched only
    // when it runs, one after another (a page inserting a few dozen spent
    // seconds waiting on them in turn).
    let urls: Vec<Url> = found
        .iter()
        .filter_map(|&n| dom.doc.element(n)?.attr("src").and_then(|src| dom.doc.resolve_url(src)))
        .collect();
    if !urls.is_empty() {
        net::prefetch(urls);
    }
    dom.pending_scripts.extend(found);
}

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"];

/// The whole document as HTML (debugging: what the tab renders).
pub fn to_html(doc: &Document) -> String {
    let mut out = String::new();
    for &c in doc.children(doc.root()) {
        serialize(doc, c, &mut out);
    }
    out
}

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

/// `[[node id | null, text], ...]` from the prelude (`__adoptedSheets`,
/// `__cssomStyles`).
fn node_texts(v: JsValue, cx: &mut Context) -> Vec<(Option<NodeId>, String)> {
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

/// `<template shadowrootmode>` (declarative shadow DOM): the template's
/// contents become its parent's shadow root, before any script runs.
fn declarative_shadow_roots(dom: &mut Dom) {
    let mut scopes = vec![dom.doc.root()];
    while let Some(scope) = scopes.pop() {
        let templates: Vec<NodeId> = dom.doc.descendants(scope).filter(|&n| is_shadow_template(&dom.doc, n)).collect();
        for t in templates {
            let Some(host) = dom.doc.parent(t) else { continue };
            if dom.doc.element(host).is_none() || dom.shadows.contains_key(&host) {
                continue;
            }
            let contents = dom.doc.element(t).and_then(|e| e.template_contents);
            let root = dom.doc.create(NodeData::Fragment);
            let kids: Vec<NodeId> = contents.map(|c| dom.doc.children(c).to_vec()).unwrap_or_default();
            dom.doc.set_children(root, kids);
            if let Some(c) = contents {
                dom.doc.set_children(c, Vec::new());
            }
            dom.doc.detach(t);
            dom.shadows.insert(host, root);
            dom.hosts.insert(root, host);
            dom.changed();
            scopes.push(root);
        }
    }
}

fn is_shadow_template(doc: &Document, n: NodeId) -> bool {
    doc.element(n).is_some_and(|e| &*e.name.local == "template" && (e.attr("shadowrootmode").is_some() || e.attr("shadowroot").is_some()))
}

/// Does the page use declarative shadow DOM (which needs the script
/// thread to render, scripts or not)?
pub fn has_declarative_shadow(doc: &Document) -> bool {
    doc.descendants(doc.root()).any(|n| is_shadow_template(doc, n))
}

/// A copy of the DOM as it renders: each shadow host's children are its
/// shadow root's, and `<slot>`s hold the light children assigned to them.
/// Which tree each node is in (and each host's own children) goes along in
/// `Document::shadow`, so selectors and stylesheets stay in their tree.
/// Node ids are the real ones, so clicks and hovers land on real nodes.
fn flatten(dom: &Dom, adopted: Vec<(Option<NodeId>, String)>) -> Document {
    let mut copy = dom.doc.clone();
    if dom.shadows.is_empty() && adopted.is_empty() {
        return copy;
    }
    let mut trees = page::ShadowTrees { adopted, ..Default::default() };
    let src = &dom.doc;
    let is_tag = |n: NodeId, t: &str| src.element(n).is_some_and(|e| &*e.name.local == t);
    for (&host, &root) in &dom.shadows {
        if src.get(host).is_none() {
            continue;
        }
        trees.light.insert(host, src.children(host).to_vec());
        if std::env::var_os("FEATHER_DEBUG_SHADOW").is_some() {
            let tag = |n: NodeId| src.element(n).map(|e| e.name.local.to_string()).unwrap_or_default();
            let kids: Vec<String> = src.children(root).iter().map(|&c| tag(c)).collect();
            eprintln!("shadow: <{}> light={} shadow={:?}", tag(host), src.children(host).len(), kids);
        }
        // Light children by the slot they ask for ("" = the default slot).
        let mut assigned: HashMap<String, Vec<NodeId>> = HashMap::new();
        for &c in src.children(host) {
            let name = src.element(c).and_then(|e| e.attr("slot")).unwrap_or("").to_owned();
            assigned.entry(name).or_default().push(c);
        }
        // This shadow tree's nodes (not those of shadow trees nested in it).
        let mut stack: Vec<NodeId> = src.children(root).to_vec();
        while let Some(n) = stack.pop() {
            trees.tree.insert(n, host);
            if is_tag(n, "slot") {
                let name = src.element(n).and_then(|e| e.attr("name")).unwrap_or("").to_owned();
                match assigned.remove(&name) {
                    Some(nodes) => copy.set_children(n, nodes),
                    // Nothing assigned: its fallback content shows, and is
                    // part of this tree.
                    None => stack.extend(src.children(n).iter().copied()),
                }
                continue;
            }
            stack.extend(src.children(n).iter().copied());
        }
        copy.set_children(host, src.children(root).to_vec());
    }
    copy.shadow = Some(std::sync::Arc::new(trees));
    copy
}


/// An image for drawImage: decoded once per URL (from the shared cache).
/// The picture `src` shows now, for `drawImage` (an animation's current
/// frame: pages that redraw every frame animate, as in browsers).
fn canvas_image(base: &Url, src: &str) -> Option<Arc<tiny_skia::Pixmap>> {
    thread_local! {
        static DECODED: RefCell<HashMap<String, Option<Rc<crate::canvas::Picture>>>> = RefCell::default();
    }
    if let Some(hit) = DECODED.with_borrow(|m| m.get(src).cloned()) {
        return hit.map(|p| p.now());
    }
    let url = base.join(src).ok()?;
    let fetched = net::fetch_bytes(&url);
    let picture = fetched.as_ref().ok().and_then(|b| crate::canvas::decode_picture(b)).map(Rc::new);
    if picture.is_none() && std::env::var_os("BROWSER_DEBUG").is_some() {
        eprintln!("canvas: can't draw {url}: {:?}", fetched.map(|b| b.iter().take(12).copied().collect::<Vec<u8>>()).map_err(|e| e.to_string()));
    }
    DECODED.with_borrow_mut(|m| {
        if m.len() > 256 {
            m.clear();
        }
        m.insert(src.to_owned(), picture.clone());
    });
    picture.map(|p| p.now())
}

/// Attribute names are case-insensitive on HTML elements only (`viewBox`
/// on an SVG element stays `viewBox`).
fn attr_name(e: &ElementData, raw: String) -> String {
    if e.is_html() { raw.to_ascii_lowercase() } else { raw }
}

/// The address a canvas's pixels are shown under (the tab fills it in
/// from `CanvasFrame`s; nothing is fetched).
pub fn canvas_url(node: NodeId) -> String {
    format!("feather-canvas:{}", node_to_number(node))
}


/// Canvases that left the document (an app re-rendering its <canvas> makes
/// a new one each time: Badge Directory's graph left hundreds of 4 MB
/// bitmaps behind) are let go of, but for the 16 drawn on most recently:
/// a detached canvas can be an offscreen buffer the page draws from.
fn forget_canvases(dom: &mut Dom, drawn: &HashSet<NodeId>) {
    const KEEP_DETACHED: usize = 16;
    let now = Instant::now();
    for &n in drawn {
        dom.canvas_used.insert(n, now);
    }
    let root = dom.doc.root();
    let attached = |n: NodeId| {
        let mut at = Some(n);
        while let Some(x) = at {
            if x == root {
                return true;
            }
            at = dom.doc.parent(x);
        }
        false
    };
    let mut detached: Vec<(NodeId, Instant)> = dom
        .canvases
        .keys()
        .filter(|&&n| !attached(n))
        .map(|&n| (n, dom.canvas_used.get(&n).copied().unwrap_or(now)))
        .collect();
    if detached.len() <= KEEP_DETACHED {
        return;
    }
    detached.sort_by_key(|&(_, used)| std::cmp::Reverse(used));
    for (n, _) in detached.into_iter().skip(KEEP_DETACHED) {
        dom.canvases.remove(&n);
        dom.canvas_used.remove(&n);
    }
}

/// Call a global function the prelude defines (`undefined` if it throws).
fn call_global(d: &Shared, name: &str, args: &[JsValue], cx: &mut Context) -> JsValue {
    let global = cx.global_object();
    let f = global.get(JsString::from(name), cx).ok().and_then(|f| f.as_callable());
    let Some(f) = f else { return JsValue::undefined() };
    match f.call(&JsValue::undefined(), args, cx) {
        Ok(v) => v,
        Err(e) => {
            if d.borrow().debug {
                eprintln!("js error in {name}: {e}");
            }
            JsValue::undefined()
        }
    }
}

fn node_set(ids: JsValue, cx: &mut Context) -> HashSet<NodeId> {
    let mut set = HashSet::new();
    if let Some(arr) = ids.as_object().and_then(|o| JsArray::from_object(o).ok()) {
        let len = arr.length(cx).unwrap_or(0);
        for i in 0..len {
            if let Ok(v) = arr.get(i, cx)
                && let Some(n) = v.as_number()
            {
                set.insert(node_from_number(n));
            }
        }
    }
    set
}

/// Tell the tab which elements scripts listen on and measure.
pub(crate) fn refresh_listening(d: &Shared, cx: &mut Context) {
    let listening = call_global(d, "__listeningIds", &[], cx);
    let listening = node_set(listening, cx);
    let mut measured = d.borrow().geometry.measured.clone();
    // What's clicked is what scripts measure next (a dropdown placed
    // under its trigger, a tooltip by its target): its box and position
    // are kept ready, not first asked for (and found missing, at 0,0)
    // in the click handler. Its parent too (menus position against it).
    {
        let dom = d.borrow();
        for &n in &listening {
            measured.insert(n);
            if let Some(p) = dom.doc.parent(n).filter(|&p| dom.doc.element(p).is_some()) {
                measured.insert(p);
            }
        }
    }
    let mouse = call_global(d, "__mouseIds", &[], cx);
    let mouse = node_set(mouse, cx);
    let dom = d.borrow();
    *dom.published.listening.write().unwrap_or_else(|e| e.into_inner()) = listening;
    *dom.published.measured.write().unwrap_or_else(|e| e.into_inner()) = measured;
    *dom.published.mouse.write().unwrap_or_else(|e| e.into_inner()) = mouse;
}

/// The DOM as it is now, as the page renders it: shadow trees flattened
/// into their hosts (for the first paint and the tab).
pub(crate) fn snapshot(d: &Shared, cx: &mut Context) -> Document {
    refresh_listening(d, cx);
    let adopted = call_global(d, "__adoptedSheets", &[], cx);
    let adopted = node_texts(adopted, cx);
    // Canvases drawn on get their bitmaps (and so their address in the
    // document); the pixels go to the tab at the next flush.
    if std::mem::take(&mut d.borrow_mut().canvases_dirty) {
        context2d::draw_pending(d);
    }
    let cssom = call_global(d, "__cssomStyles", &[], cx);
    let cssom: Vec<(NodeId, String)> = node_texts(cssom, cx).into_iter().filter_map(|(id, css)| Some((id?, css))).collect();
    let mut dom = d.borrow_mut();
    dom.dirty = false;
    dom.last_sent = Instant::now();
    dom.touched.clear();
    dom.touched_all = false;
    let mut doc = flatten(&dom, adopted);
    // Canvases show their pixels, sent alongside (`CanvasFrame`).
    for &node in dom.canvases.keys() {
        if let Some(e) = doc.element_mut(node) {
            set_attr(e, "src", canvas_url(node));
        }
    }
    // <style> elements scripts filled through the CSSOM (CSS-in-JS).
    for (style, css) in cssom {
        if doc.get(style).is_some() {
            let text = doc.create_text(css);
            doc.set_children(style, vec![text]);
        }
    }
    doc
}
