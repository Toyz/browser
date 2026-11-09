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
    match e.attr("type").map(|t| t.trim().to_ascii_lowercase()) {
        None => true,
        Some(t) => t.is_empty() || matches!(t.as_str(), "text/javascript" | "application/javascript" | "text/ecmascript" | "application/ecmascript" | "application/x-javascript"),
    }
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
    /// Form field values from the page (by node).
    values: HashMap<NodeId, String>,
    debug: bool,
    dark: bool,
    started: Instant,
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
    last_sent: Instant,
    flush_at: Option<Instant>,
}

impl Runtime {
    pub fn new(
        doc: Document,
        opts: Options,
        rx: mpsc::Receiver<ToJs>,
        updates: async_channel::Sender<FromJs>,
        listening: Arc<RwLock<HashSet<NodeId>>>,
    ) -> Runtime {
        let dom = Rc::new(RefCell::new(Dom {
            doc,
            dirty: false,
            timers: Vec::new(),
            pending_scripts: Vec::new(),
            ran: HashSet::new(),
            navigate: None,
            values: HashMap::new(),
            debug: opts.debug,
            dark: opts.dark,
            started: Instant::now(),
        }));
        let mut cx = Context::default();
        // Runaway loops end with an exception instead of a stuck thread.
        cx.runtime_limits_mut().set_loop_iteration_limit(200_000_000);
        cx.runtime_limits_mut().set_recursion_limit(2048);
        install_natives(&mut cx, &dom);
        if let Err(e) = cx.eval(Source::from_bytes(PRELUDE)) {
            eprintln!("js prelude failed: {e}");
        }
        let mut rt = Runtime { cx, dom, rx, updates, listening, opts, last_sent: Instant::now(), flush_at: None };
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

    /// The script's source: inline text, or fetched.
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
        if let Some((code, what)) = self.script_source(node) {
            let t = Instant::now();
            self.exec(Some(node), &code, &what);
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
                    Some((n, external && e.attr("defer").is_some(), external && e.attr("async").is_some()))
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

    /// The DOM as it is now (for the first paint).
    pub fn snapshot_doc(&mut self) -> Document {
        self.refresh_listening();
        let mut dom = self.dom.borrow_mut();
        dom.dirty = false;
        self.last_sent = Instant::now();
        dom.doc.clone()
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
        if let Some(url) = self.dom.borrow_mut().navigate.take() {
            self.updates.send_blocking(FromJs::Navigate(url)).ok();
        }
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
                Ok(ToJs::Click { node, reply }) => {
                    let prevented = self.call_global("__dispatchClick", &[JsValue::from(node_to_number(node))]);
                    if let Some(reply) = reply {
                        reply.send(prevented.to_boolean()).ok();
                    }
                }
                Ok(ToJs::Input { node, value }) => {
                    self.dom.borrow_mut().values.insert(node, value);
                    self.call_global("__dispatchInput", &[JsValue::from(node_to_number(node))]);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
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
    add("createElement", 1, Box::new(|d, a, cx| {
        let tag = arg_string(a, 0, cx)?.to_ascii_lowercase();
        let mut dom = d.borrow_mut();
        let id = dom.doc.create_element(page::html_name(&tag), Vec::new());
        Ok(JsValue::from(node_to_number(id)))
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
        let parsed = html::parse_fragment(&html, if tag.is_empty() { "div" } else { &tag }, dom.doc.url.clone());
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
    cx.register_global_property(js_string!("__native"), obj, PropAttr::all()).ok();
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
