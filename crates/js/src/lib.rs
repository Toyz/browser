//! JavaScript, via [Boa](https://github.com/boa-dev/boa).
//!
//! Each page with scripts gets a script thread (Boa's context can't leave
//! the thread it was made on). The thread owns the page's DOM — the
//! canonical [`Document`] — runs its scripts, then keeps running timers and
//! event handlers. Whenever a task changes the DOM it sends the tab a
//! snapshot, which the tab restyles and lays out like any page. Node ids
//! stay the same between snapshots, so hover state, form fields and scroll
//! positions survive.
//!
//! The DOM API is mostly JavaScript ([`prelude.js`]) over a small set of
//! native functions taking node ids (`dom.rs`): wrappers, listeners and
//! timers live in the JS heap, where Boa's GC can see them.

pub mod cors;
mod canvas;
mod dom;
mod images;
mod modules;
pub mod storage;
mod sse;
pub mod websocket;

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use page::{Document, NodeId};
use url::Url;

pub use dom::{node_from_number, to_html};

/// Fetches a script's source (blocking).
pub type FetchText = Arc<dyn Fn(&Url) -> Option<String> + Send + Sync>;

/// What the page needs from the outside world.
#[derive(Clone)]
pub struct Options {
    /// Fetch a script (synchronously; it runs on the script thread).
    pub fetch_text: FetchText,
    /// `prefers-color-scheme: dark`.
    pub dark: bool,
    /// How long the first paint waits for the page's scripts.
    pub budget: Duration,
    pub debug: bool,
    /// The tab (for `sessionStorage`).
    pub session: u64,
}

/// To the script thread.
pub enum ToJs {
    /// The user clicked `node`; answers whether a handler cancelled the
    /// default action.
    Click { node: NodeId, reply: Option<mpsc::Sender<bool>> },
    /// A form field's value changed (and fire `input`).
    Input { node: NodeId, value: String },
    /// A checkbox or radio button was toggled (fire `input` and `change`).
    Checked { node: NodeId, checked: bool },
    /// A form is being submitted: answers whether a handler cancelled it.
    Submit { form: NodeId, reply: mpsc::Sender<bool> },
    /// Something happened on one of the page's WebSockets.
    Socket { id: u32, event: websocket::SocketEvent },
    /// A script's request got its response head (CORS allowing).
    FetchHead { id: u32, head: cors::ScriptResponse },
    /// More of the body, for a script reading it as a stream.
    FetchChunk { id: u32, bytes: Vec<u8> },
    /// A request a script made finished (from its network thread).
    FetchDone { id: u32, result: Result<cors::ScriptResponse, String> },
    /// An `EventSource` event: `open`, `error`, or a message (type, data,
    /// last event id).
    Sse { id: u32, kind: &'static str, event: String, data: String, last_id: String },
    /// A key: `keydown` / `keyup`, with the DOM `key` and `code`, at the
    /// focused field (`None`: the page itself).
    Key { kind: &'static str, key: String, code: String, target: Option<NodeId>, modifiers: [bool; 4] },
    /// A mouse event over `node`: `mousedown`, `mouseup`, `mousemove`,
    /// `dblclick` or `wheel`, at viewport coordinates.
    Mouse { kind: &'static str, node: NodeId, x: f32, y: f32, button: i16, buttons: u16, delta: (f32, f32) },
    /// Where the elements scripts measure are (`[x, y, width, height]`,
    /// relative to the viewport), and the viewport's size. `gone`: measured
    /// elements that stopped being painted (hidden, or scrolled out of the
    /// rows on screen); their last rect stays for `getBoundingClientRect`,
    /// but they no longer intersect anything.
    Layout { rects: Vec<(NodeId, [f32; 4])>, gone: Vec<NodeId>, viewport: (f32, f32) },
    /// Back/forward between history entries of this document (made by
    /// `pushState`): `popstate` with the entry `delta` steps away.
    Traverse { delta: i32 },
    /// The tab focused a field (or nothing): scripts hear `blur`/`focusout`
    /// on the old one and `focus`/`focusin` on the new one.
    Focus { node: Option<NodeId> },
    /// An image a script loaded finished: its size, or `None` if it failed.
    Image { id: u32, size: Option<(u32, u32)> },
    Shutdown,
}

/// From the script thread.
pub enum FromJs {
    /// The DOM after a task that changed it.
    Document(Arc<Document>),
    /// `location.href = ...` and friends.
    Navigate(Url),
    /// `form.submit()`: submit it as the browser does (no `submit` event).
    Submit(NodeId),
    /// `history.pushState` (a new history entry for this document) or
    /// `replaceState` (`replace`): the address changed, nothing loads.
    UrlChanged { url: Url, replace: bool },
    /// `history.back()` / `forward()` / `go(n)`.
    Traverse(i32),
    /// Scripts moved focus (`el.focus()`, `el.blur()`): `None` is nothing.
    Focus(Option<NodeId>),
}

/// The tab's handle on a page's script thread.
pub struct ScriptHost {
    tx: mpsc::Sender<ToJs>,
    pub updates: async_channel::Receiver<FromJs>,
    /// Elements with `click` listeners (or `onclick`), as of the last
    /// snapshot: clicks under them wait for scripts before acting.
    pub listening: Arc<std::sync::RwLock<std::collections::HashSet<NodeId>>>,
    /// Elements whose geometry scripts read (getBoundingClientRect,
    /// ResizeObserver): the tab reports where they are.
    pub measured: Arc<std::sync::RwLock<std::collections::HashSet<NodeId>>>,
    /// Elements with mouse listeners (down/up/move/wheel/dblclick, or
    /// pointer events); the document's root stands for window/document.
    pub mouse_listening: Arc<std::sync::RwLock<std::collections::HashSet<NodeId>>>,
}

impl ScriptHost {
    pub fn send(&self, msg: ToJs) {
        self.tx.send(msg).ok();
    }

    /// Whether a click on `path` (the node and its ancestors) is heard.
    /// Whether mouse events on `path` (the node and its ancestors) are heard.
    pub fn hears_mouse(&self, path: &[NodeId]) -> bool {
        let set = self.mouse_listening.read().unwrap_or_else(|e| e.into_inner());
        !set.is_empty() && path.iter().any(|n| set.contains(n))
    }

    pub fn hears_click(&self, path: &[NodeId]) -> bool {
        let set = self.listening.read().unwrap_or_else(|e| e.into_inner());
        path.iter().any(|n| set.contains(n))
    }

    /// Ask scripts whether a form may be submitted (`false`: a `submit`
    /// handler cancelled it; waits at most `timeout`).
    pub fn may_submit(&self, form: NodeId, timeout: Duration) -> bool {
        let (reply, rx) = mpsc::channel();
        self.send(ToJs::Submit { form, reply });
        !rx.recv_timeout(timeout).unwrap_or(false)
    }

    /// Ask scripts about a click; `true` if a handler called
    /// `preventDefault()` (waits at most `timeout`).
    pub fn click(&self, node: NodeId, timeout: Duration) -> bool {
        let (reply, rx) = mpsc::channel();
        self.send(ToJs::Click { node, reply: Some(reply) });
        rx.recv_timeout(timeout).unwrap_or(false)
    }
}

impl Drop for ScriptHost {
    fn drop(&mut self) {
        self.tx.send(ToJs::Shutdown).ok();
    }
}

/// Does the document have scripts we'd run?
pub fn has_scripts(doc: &Document) -> bool {
    doc.descendants(doc.root()).any(|n| doc.element(n).is_some_and(|e| &**e.local_name() == "script" && dom::runnable(e)))
        || dom::has_declarative_shadow(doc)
}

/// Run `doc`'s scripts. Returns the DOM to paint first (the scripts' work,
/// or the untouched document if they're still busy after the budget) and
/// the handle for everything after.
pub fn start(doc: Document, opts: Options) -> (Document, Option<ScriptHost>) {
    if !has_scripts(&doc) {
        return (doc, None);
    }
    let (tx, rx) = mpsc::channel::<ToJs>();
    let fetch_tx = tx.clone();
    let (updates_tx, updates) = async_channel::unbounded();
    let (first_tx, first_rx) = mpsc::channel::<Document>();
    let listening = Arc::new(std::sync::RwLock::new(Default::default()));
    let measured: Arc<std::sync::RwLock<std::collections::HashSet<NodeId>>> = Arc::new(std::sync::RwLock::new(Default::default()));
    let shared_measured = measured.clone();
    let mouse_listening: Arc<std::sync::RwLock<std::collections::HashSet<NodeId>>> = Arc::new(std::sync::RwLock::new(Default::default()));
    let shared_mouse = mouse_listening.clone();
    let fallback = doc.clone();
    let budget = opts.budget;
    let shared_listening = listening.clone();
    let spawned = std::thread::Builder::new()
        .name("feather-js".into())
        .stack_size(256 << 20)
        .spawn(move || {
            let mut rt = dom::Runtime::new(doc, opts, rx, fetch_tx, updates_tx, shared_listening, shared_measured, shared_mouse);
            rt.run_initial_scripts();
            // Data the page fetched while loading makes the first paint
            // too, if it's quick.
            rt.drain_for(budget / 2);
            first_tx.send(rt.snapshot_doc()).ok();
            rt.event_loop();
        });
    if spawned.is_err() {
        return (fallback, None);
    }
    let first = first_rx.recv_timeout(budget).unwrap_or(fallback);
    (first, Some(ScriptHost { tx, updates, listening, measured, mouse_listening }))
}

/// When the next timer is due, for `recv_timeout`.
fn until(deadline: Option<Instant>) -> Duration {
    match deadline {
        Some(d) => d.saturating_duration_since(Instant::now()),
        None => Duration::from_secs(3600),
    }
}
