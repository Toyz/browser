//! A worker's network: `fetch` and `XMLHttpRequest` (async, and the
//! synchronous kind workers may still use). Requests run on threads of
//! their own and answer through the worker's queue (`ToWorker::Fetched`).

use std::cell::RefCell;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::JsPromise;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};

use crate::cors::{ScriptRequest, ScriptResponse};
use crate::dom::js_value_string;
use crate::worker::{Pending, ToWorker, scope};

/// Send `req` on a thread; its result comes back as request `id`.
fn start(id: u32, req: ScriptRequest, cx: &Context) {
    let s = scope(cx);
    let (inbox, base, debug) = (s.inbox.clone(), s.base.clone(), s.debug);
    std::thread::Builder::new()
        .name("feather-worker-fetch".into())
        .spawn(move || {
            let what = format!("{} {}", req.method, req.url);
            let result = crate::cors::fetch(&base, req);
            if debug {
                match &result {
                    Ok(r) => eprintln!("js: worker fetch {what} → {} ({} bytes)", r.status, r.body.len()),
                    Err(e) => eprintln!("js: worker fetch {what} failed: {e}"),
                }
            }
            inbox.send(ToWorker::Fetched { id, result }).ok();
        })
        .ok();
}

fn next_id(cx: &Context) -> u32 {
    let s = scope(cx);
    let id = s.next_request.get();
    s.next_request.set(id + 1);
    id
}

/// `fetch(input, init)`.
fn fetch(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let (promise, resolvers) = JsPromise::new_pending(cx);
    let request = (|| -> JsResult<ScriptRequest> {
        let class = cx.global_object().get(js_string!("Request"), cx)?;
        let class = class.as_constructor().ok_or_else(|| JsNativeError::typ().with_message("no Request"))?;
        let args = [a.first().cloned().unwrap_or_default(), a.get(1).cloned().unwrap_or_default()];
        let req = class.construct(&args, None, cx)?;
        let signal = req.get(js_string!("signal"), cx)?;
        if let Some(signal) = signal.as_object()
            && signal.get(js_string!("aborted"), cx)?.to_boolean()
        {
            return Err(boa_engine::JsError::from_opaque(signal.get(js_string!("reason"), cx)?));
        }
        crate::request::parts(&req, cx)?.ok_or_else(|| JsNativeError::typ().with_message("Failed to fetch").into())
    })();
    match request {
        Ok(req) => {
            let id = next_id(cx);
            scope(cx).pending.borrow_mut().insert(id, Pending::Fetch { resolve: resolvers.resolve, reject: resolvers.reject });
            start(id, req, cx);
        }
        Err(e) => {
            let e = e.into_opaque(cx)?;
            resolvers.reject.call(&JsValue::undefined(), &[e], cx)?;
        }
    }
    Ok(promise.into())
}

/// Request `id` finished.
pub(crate) fn fetched(id: u32, result: Result<ScriptResponse, String>, cx: &mut Context) -> JsResult<()> {
    let Some(pending) = scope(cx).pending.borrow_mut().remove(&id) else { return Ok(()) };
    match pending {
        Pending::Fetch { resolve, reject } => match result {
            Ok(r) => {
                let response = crate::response::from_network(r, cx)?;
                resolve.call(&JsValue::undefined(), &[response], cx)?;
            }
            Err(_) => {
                let e = JsNativeError::typ().with_message("Failed to fetch").into_opaque(cx);
                reject.call(&JsValue::undefined(), &[e.into()], cx)?;
            }
        },
        Pending::Xhr(xhr) => complete(&xhr, result, cx)?,
    }
    Ok(())
}

// ---- XMLHttpRequest --------------------------------------------------------

#[derive(Trace, Finalize, JsData)]
struct Xhr {
    #[unsafe_ignore_trace]
    state: RefCell<XhrState>,
    /// (type, function).
    listeners: GcRefCell<Vec<(String, JsObject)>>,
}

#[derive(Default)]
struct XhrState {
    ready: u8,
    method: String,
    url: Option<url::Url>,
    sync: bool,
    headers: Vec<(String, String)>,
    response_type: String,
    with_credentials: bool,
    /// The request in flight (its id), if any.
    sent: Option<u32>,
    response: Option<ScriptResponse>,
    failed: bool,
}

fn this_xhr(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|o| o.downcast_ref::<Xhr>().is_some())
        .ok_or_else(|| JsNativeError::typ().with_message("not an XMLHttpRequest").into())
}

fn with<R>(this: &JsValue, f: impl FnOnce(&mut XhrState) -> R) -> JsResult<R> {
    let o = this_xhr(this)?;
    let x = o.downcast_ref::<Xhr>().expect("checked");
    let mut state = x.state.borrow_mut();
    Ok(f(&mut state))
}

/// Fire `kind` at `xhr`: listeners, then `on<kind>`.
fn fire(xhr: &JsObject, kind: &str, cx: &mut Context) -> JsResult<()> {
    let ev = JsObject::with_object_proto(cx.intrinsics());
    ev.set(js_string!("type"), JsString::from(kind), false, cx)?;
    ev.set(js_string!("target"), xhr.clone(), false, cx)?;
    ev.set(js_string!("currentTarget"), xhr.clone(), false, cx)?;
    let (loaded, total) = {
        let x = xhr.downcast_ref::<Xhr>().expect("an XMLHttpRequest");
        let n = x.state.borrow().response.as_ref().map_or(0, |r| r.body.len());
        (n, n)
    };
    ev.set(js_string!("loaded"), loaded as f64, false, cx)?;
    ev.set(js_string!("total"), total as f64, false, cx)?;
    ev.set(js_string!("lengthComputable"), total > 0, false, cx)?;
    let listeners: Vec<JsObject> = {
        let x = xhr.downcast_ref::<Xhr>().expect("an XMLHttpRequest");
        x.listeners.borrow().iter().filter(|(t, _)| t == kind).map(|(_, f)| f.clone()).collect()
    };
    for f in listeners {
        f.call(&xhr.clone().into(), &[ev.clone().into()], cx)?;
    }
    let handler = xhr.get(JsString::from(format!("on{kind}")), cx)?;
    if let Some(handler) = handler.as_callable() {
        handler.call(&xhr.clone().into(), &[ev.into()], cx)?;
    }
    Ok(())
}

fn set_ready(xhr: &JsObject, ready: u8, cx: &mut Context) -> JsResult<()> {
    xhr.downcast_ref::<Xhr>().expect("an XMLHttpRequest").state.borrow_mut().ready = ready;
    fire(xhr, "readystatechange", cx)
}

/// The request finished: the response (or the failure) and its events.
fn complete(xhr: &JsObject, result: Result<ScriptResponse, String>, cx: &mut Context) -> JsResult<()> {
    let ok = {
        let x = xhr.downcast_ref::<Xhr>().expect("an XMLHttpRequest");
        let mut s = x.state.borrow_mut();
        s.sent = None;
        match result {
            Ok(r) => {
                s.response = Some(r);
                true
            }
            Err(_) => {
                s.failed = true;
                false
            }
        }
    };
    if ok {
        set_ready(xhr, 2, cx)?;
        set_ready(xhr, 3, cx)?;
        fire(xhr, "progress", cx)?;
    }
    set_ready(xhr, 4, cx)?;
    fire(xhr, if ok { "load" } else { "error" }, cx)?;
    fire(xhr, "loadend", cx)
}

fn body_bytes(this: &JsValue) -> JsResult<Vec<u8>> {
    with(this, |s| s.response.as_ref().map(|r| r.body.clone()).unwrap_or_default())
}

type Method = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

fn getter(class: &mut ClassBuilder<'_>, name: &str, get: Method, set: Option<Method>) {
    let realm = class.context().realm().clone();
    let get = FunctionObjectBuilder::new(&realm, NativeFunction::from_fn_ptr(get)).name(JsString::from(format!("get {name}"))).build();
    let set = set.map(|s| FunctionObjectBuilder::new(&realm, NativeFunction::from_fn_ptr(s)).name(JsString::from(format!("set {name}"))).build());
    class.accessor(JsString::from(name), Some(get), set, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
}

impl Class for Xhr {
    const NAME: &'static str = "XMLHttpRequest";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        for (i, name) in ["UNSENT", "OPENED", "HEADERS_RECEIVED", "LOADING", "DONE"].into_iter().enumerate() {
            class.static_property(JsString::from(name), i as u32, Attribute::empty());
            class.property(JsString::from(name), i as u32, Attribute::empty());
        }
        for name in ["onreadystatechange", "onload", "onerror", "onloadend", "onloadstart", "onprogress", "onabort", "ontimeout"] {
            class.property(JsString::from(name), JsValue::null(), Attribute::all());
        }
        class.property(js_string!("timeout"), 0, Attribute::all());
        getter(class, "readyState", |this, _, _| Ok(with(this, |s| s.ready)?.into()), None);
        getter(class, "status", |this, _, _| Ok(with(this, |s| s.response.as_ref().map_or(0, |r| r.status))?.into()), None);
        getter(class, "statusText", |this, _, _| Ok(JsString::from(with(this, |s| s.response.as_ref().map(|r| r.status_text.clone()).unwrap_or_default())?).into()), None);
        getter(class, "responseURL", |this, _, _| Ok(JsString::from(with(this, |s| s.response.as_ref().map(|r| r.url.to_string()).unwrap_or_default())?).into()), None);
        getter(class, "responseText", |this, _, _| Ok(crate::body::text(&body_bytes(this)?)), None);
        getter(
            class,
            "responseType",
            |this, _, _| Ok(JsString::from(with(this, |s| s.response_type.clone())?).into()),
            Some(|this, a, cx| {
                let t = js_value_string(&a.first().cloned().unwrap_or_default(), cx);
                with(this, |s| s.response_type = t)?;
                Ok(JsValue::undefined())
            }),
        );
        getter(
            class,
            "withCredentials",
            |this, _, _| Ok(with(this, |s| s.with_credentials)?.into()),
            Some(|this, a, _| {
                let v = a.first().is_some_and(JsValue::to_boolean);
                with(this, |s| s.with_credentials = v)?;
                Ok(JsValue::undefined())
            }),
        );
        getter(class, "response", |this, _, cx| {
            let (kind, done) = with(this, |s| (s.response_type.clone(), s.ready == 4 && s.response.is_some()))?;
            let bytes = body_bytes(this)?;
            match kind.as_str() {
                "" | "text" => Ok(crate::body::text(&bytes)),
                _ if !done => Ok(JsValue::null()),
                "json" => Ok(crate::body::json(&bytes, cx).unwrap_or(JsValue::null())),
                "arraybuffer" => crate::blob::array_buffer(&bytes, cx),
                "blob" => {
                    let kind = with(this, |s| s.response.as_ref().and_then(|r| r.headers.iter().find(|(k, _)| k == "content-type").map(|(_, v)| v.clone())))?;
                    Ok(crate::blob::new_blob(bytes.into(), kind.unwrap_or_default(), cx))
                }
                _ => Ok(JsValue::null()),
            }
        }, None);
        // A stand-in `upload` (nothing reports upload progress).
        getter(class, "upload", |_, _, cx| {
            let upload = JsObject::with_object_proto(cx.intrinsics());
            for name in ["addEventListener", "removeEventListener"] {
                let f = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined()))).name(JsString::from(name)).build();
                upload.set(JsString::from(name), f, false, cx)?;
            }
            Ok(upload.into())
        }, None);

        let methods: [(&str, usize, Method); 9] = [
            ("open", 2, |this, a, cx| {
                let method = js_value_string(&a.first().cloned().unwrap_or_default(), cx).to_ascii_uppercase();
                let href = js_value_string(&a.get(1).cloned().unwrap_or_default(), cx);
                let url = crate::dom::resolve(cx, &href).ok_or_else(|| crate::dom_exception::error("SyntaxError", &format!("XMLHttpRequest: invalid URL {href:?}"), cx))?;
                let sync = a.get(2).is_some_and(|v| !v.is_undefined() && !v.to_boolean());
                with(this, |s| {
                    *s = XhrState { method, url: Some(url), sync, response_type: std::mem::take(&mut s.response_type), with_credentials: s.with_credentials, ..Default::default() };
                })?;
                set_ready(&this_xhr(this)?, 1, cx)?;
                Ok(JsValue::undefined())
            }),
            ("setRequestHeader", 2, |this, a, cx| {
                let (k, v) = (js_value_string(&a.first().cloned().unwrap_or_default(), cx), js_value_string(&a.get(1).cloned().unwrap_or_default(), cx));
                let opened = with(this, |s| {
                    if s.ready != 1 {
                        return false;
                    }
                    match s.headers.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(&k)) {
                        Some((_, old)) => {
                            old.push_str(", ");
                            old.push_str(&v);
                        }
                        None => s.headers.push((k, v)),
                    }
                    true
                })?;
                if !opened {
                    return Err(crate::dom_exception::error("InvalidStateError", "XMLHttpRequest: not opened", cx));
                }
                Ok(JsValue::undefined())
            }),
            ("send", 0, |this, a, cx| {
                let xhr = this_xhr(this)?;
                let body = a.first().cloned().unwrap_or_default();
                let (method, url, mut headers, sync, credentials) = with(this, |s| {
                    (s.method.clone(), s.url.clone(), s.headers.clone(), s.sync, if s.with_credentials { "include" } else { "same-origin" })
                })?;
                let Some(url) = url else { return Err(crate::dom_exception::error("InvalidStateError", "XMLHttpRequest: not opened", cx)) };
                let body = if body.is_null_or_undefined() || matches!(method.as_str(), "GET" | "HEAD") {
                    None
                } else {
                    let (bytes, kind) = crate::blob::encode(&body, cx)?;
                    if let Some(kind) = kind.filter(|_| !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type"))) {
                        headers.push(("content-type".into(), kind));
                    }
                    Some(bytes)
                };
                let req = ScriptRequest { method, url, headers, body, mode: "cors".into(), credentials: credentials.into() };
                if sync {
                    let base = scope(cx).base.clone();
                    let result = crate::cors::fetch(&base, req);
                    complete(&xhr, result, cx)?;
                    return Ok(JsValue::undefined());
                }
                let id = next_id(cx);
                with(this, |s| s.sent = Some(id))?;
                scope(cx).pending.borrow_mut().insert(id, Pending::Xhr(xhr.clone()));
                fire(&xhr, "loadstart", cx)?;
                start(id, req, cx);
                Ok(JsValue::undefined())
            }),
            ("abort", 0, |this, _, cx| {
                let xhr = this_xhr(this)?;
                let sent = with(this, |s| s.sent.take())?;
                if let Some(id) = sent {
                    scope(cx).pending.borrow_mut().remove(&id);
                    with(this, |s| s.ready = 4)?;
                    fire(&xhr, "readystatechange", cx)?;
                    fire(&xhr, "abort", cx)?;
                    fire(&xhr, "loadend", cx)?;
                }
                with(this, |s| s.ready = 0)?;
                Ok(JsValue::undefined())
            }),
            ("getResponseHeader", 1, |this, a, cx| {
                let name = js_value_string(&a.first().cloned().unwrap_or_default(), cx).to_ascii_lowercase();
                let values: Vec<String> = with(this, |s| s.response.as_ref().map(|r| r.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(&name)).map(|(_, v)| v.clone()).collect()).unwrap_or_default())?;
                Ok(if values.is_empty() { JsValue::null() } else { JsString::from(values.join(", ")).into() })
            }),
            ("getAllResponseHeaders", 0, |this, _, _| {
                let all = with(this, |s| {
                    let mut pairs: Vec<(String, String)> = s.response.as_ref().map(|r| r.headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.clone())).collect()).unwrap_or_default();
                    pairs.sort();
                    pairs.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect::<String>()
                })?;
                Ok(JsString::from(all).into())
            }),
            ("overrideMimeType", 1, |_, _, _| Ok(JsValue::undefined())),
            ("addEventListener", 2, |this, a, cx| {
                let o = this_xhr(this)?;
                let kind = js_value_string(&a.first().cloned().unwrap_or_default(), cx);
                if let Some(f) = a.get(1).and_then(JsValue::as_object).filter(|f| f.is_callable()) {
                    let x = o.downcast_ref::<Xhr>().expect("checked");
                    let mut list = x.listeners.borrow_mut();
                    if !list.iter().any(|(t, g)| *t == kind && g == &f) {
                        list.push((kind, f));
                    }
                }
                Ok(JsValue::undefined())
            }),
            ("removeEventListener", 2, |this, a, cx| {
                let o = this_xhr(this)?;
                let kind = js_value_string(&a.first().cloned().unwrap_or_default(), cx);
                if let Some(f) = a.get(1).and_then(JsValue::as_object) {
                    o.downcast_ref::<Xhr>().expect("checked").listeners.borrow_mut().retain(|(t, g)| !(*t == kind && g == &f));
                }
                Ok(JsValue::undefined())
            }),
        ];
        for (name, len, f) in methods {
            class.method(JsString::from(name), len, NativeFunction::from_fn_ptr(f));
        }
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Ok(Xhr { state: RefCell::new(XhrState::default()), listeners: GcRefCell::new(Vec::new()) })
    }
}

/// Install `fetch` and `XMLHttpRequest` in a worker.
pub(crate) fn install(cx: &mut Context) {
    let f = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(fetch)).name(js_string!("fetch")).length(1).build();
    let global = cx.global_object();
    global.set(js_string!("fetch"), f, false, cx).ok();
    if let Err(e) = cx.register_global_class::<Xhr>() {
        eprintln!("js: XMLHttpRequest: {e}");
    }
}
