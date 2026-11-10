//! `Request`, native: a URL (resolved against the page), a method, headers
//! (a `Headers`), the body as given (encoded when it's sent or read), and
//! the fetch options. `fetch` (the prelude's) reads them, `_body` included.

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};

use crate::dom::js_value_string;

#[derive(Trace, Finalize, JsData)]
struct Request {
    #[unsafe_ignore_trace]
    url: String,
    #[unsafe_ignore_trace]
    method: String,
    headers: JsObject,
    body: GcRefCell<JsValue>,
    #[unsafe_ignore_trace]
    body_used: std::cell::Cell<bool>,
    /// mode, credentials, cache, redirect, referrer.
    #[unsafe_ignore_trace]
    options: [String; 5],
    signal: JsValue,
}

const OPTIONS: [(&str, &str); 5] = [("mode", "cors"), ("credentials", "same-origin"), ("cache", "default"), ("redirect", "follow"), ("referrer", "about:client")];

fn this_request(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|o| o.downcast_ref::<Request>().is_some())
        .ok_or_else(|| JsNativeError::typ().with_message("not a Request").into())
}

fn with<R>(this: &JsValue, f: impl FnOnce(&Request) -> R) -> JsResult<R> {
    let o = this_request(this)?;
    let r = o.downcast_ref::<Request>().expect("a Request");
    Ok(f(&r))
}

/// `init[key]`, when `init` is an object and it's there.
fn option(init: Option<&JsObject>, key: &str, cx: &mut Context) -> JsResult<Option<JsValue>> {
    let Some(o) = init else { return Ok(None) };
    let v = o.get(JsString::from(key), cx)?;
    Ok((!v.is_undefined()).then_some(v))
}

impl crate::body::HasBody for Request {
    fn take_body(&self, cx: &mut Context) -> JsResult<Option<Vec<u8>>> {
        if self.body_used.replace(true) {
            return Ok(None);
        }
        let body = self.body.borrow().clone();
        if body.is_null_or_undefined() {
            return Ok(Some(Vec::new()));
        }
        Ok(Some(crate::blob::encode(&body, cx)?.0))
    }
    fn body_used(&self) -> bool {
        self.body_used.get()
    }
    fn headers(&self) -> JsObject {
        self.headers.clone()
    }
}

/// What the network is asked for a `Request`: its method, URL, headers
/// (the body's content type added), body and options. `None` if `o` isn't
/// a Request.
pub(crate) fn parts(o: &JsObject, cx: &mut Context) -> JsResult<Option<crate::cors::ScriptRequest>> {
    let Some((url, method, headers, body, options)) = o.downcast_ref::<Request>().map(|r| (r.url.clone(), r.method.clone(), r.headers.clone(), r.body.borrow().clone(), r.options.clone())) else {
        return Ok(None);
    };
    let Ok(url) = url::Url::parse(&url) else { return Ok(None) };
    let mut pairs = crate::body::header_pairs(&headers, cx)?;
    let body = if body.is_null_or_undefined() {
        None
    } else {
        let (bytes, kind) = crate::blob::encode(&body, cx)?;
        if let Some(kind) = kind.filter(|_| !pairs.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type"))) {
            pairs.push(("content-type".into(), kind));
        }
        Some(bytes)
    };
    let [mode, credentials, ..] = options;
    Ok(Some(crate::cors::ScriptRequest { method, url, headers: pairs, body, mode, credentials }))
}

fn getter(class: &mut ClassBuilder<'_>, name: &str, f: NativeFunction) {
    let realm = class.context().realm().clone();
    let get = FunctionObjectBuilder::new(&realm, f).name(JsString::from(format!("get {name}"))).build();
    class.accessor(JsString::from(name), Some(get), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
}

impl Class for Request {
    const NAME: &'static str = "Request";
    const LENGTH: usize = 1;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        getter(class, "url", NativeFunction::from_fn_ptr(|this, _, _| Ok(JsString::from(with(this, |r| r.url.clone())?).into())));
        getter(class, "method", NativeFunction::from_fn_ptr(|this, _, _| Ok(JsString::from(with(this, |r| r.method.clone())?).into())));
        getter(class, "headers", NativeFunction::from_fn_ptr(|this, _, _| Ok(with(this, |r| r.headers.clone())?.into())));
        getter(class, "signal", NativeFunction::from_fn_ptr(|this, _, _| with(this, |r| r.signal.clone())));
        // For `fetch`: the body as given.
        getter(class, "_body", NativeFunction::from_fn_ptr(|this, _, _| with(this, |r| r.body.borrow().clone())));
        for (i, (name, _)) in OPTIONS.iter().enumerate() {
            let f = NativeFunction::from_copy_closure(move |this, _, _| Ok(JsString::from(with(this, |r| r.options[i].clone())?).into()));
            getter(class, name, f);
        }
        crate::body::add_to::<Request>(class);
        class.method(js_string!("clone"), 0, NativeFunction::from_fn_ptr(|this, _, cx| {
            let o = this_request(this)?;
            if o.downcast_ref::<Request>().expect("a Request").body_used.get() {
                return Err(JsNativeError::typ().with_message("Request.clone: Body has already been consumed.").into());
            }
            let class = cx.global_object().get(js_string!("Request"), cx)?;
            let class = class.as_constructor().expect("Request");
            Ok(class.construct(&[o.into()], None, cx)?.into())
        }));
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let input = a.first().cloned().unwrap_or_default();
        let init = a.get(1).and_then(JsValue::as_object);
        let from = input.as_object().filter(|o| o.downcast_ref::<Request>().is_some());
        let base = from.as_ref().map(|o| {
            let r = o.downcast_ref::<Request>().expect("a Request");
            (r.url.clone(), r.method.clone(), r.headers.clone(), r.body.borrow().clone(), r.options.clone(), r.signal.clone())
        });
        let url = match &base {
            Some(b) => b.0.clone(),
            None => {
                let href = js_value_string(&input, cx);
                let resolved = crate::dom::resolve(cx, &href).map(|u| u.to_string());
                resolved.ok_or_else(|| JsNativeError::typ().with_message(format!("Request: invalid URL {href:?}")))?
            }
        };
        let method = match option(init.as_ref(), "method", cx)? {
            Some(m) => js_value_string(&m, cx).to_ascii_uppercase(),
            None => base.as_ref().map_or_else(|| "GET".to_owned(), |b| b.1.clone()),
        };
        let headers = match option(init.as_ref(), "headers", cx)? {
            Some(h) => crate::body::new_headers(&h, cx)?,
            None => crate::body::new_headers(&base.as_ref().map_or(JsValue::undefined(), |b| b.2.clone().into()), cx)?,
        };
        let body = match option(init.as_ref(), "body", cx)? {
            Some(b) => b,
            None => base.as_ref().map_or(JsValue::null(), |b| b.3.clone()),
        };
        if !body.is_null() && matches!(method.as_str(), "GET" | "HEAD") {
            return Err(JsNativeError::typ().with_message("Request with GET/HEAD method cannot have body.").into());
        }
        let mut options = base.as_ref().map_or_else(|| OPTIONS.map(|(_, d)| d.to_owned()), |b| b.4.clone());
        for (i, (name, _)) in OPTIONS.iter().enumerate() {
            if let Some(v) = option(init.as_ref(), name, cx)? {
                options[i] = js_value_string(&v, cx);
            }
        }
        let signal = match option(init.as_ref(), "signal", cx)? {
            Some(s) => s,
            None => base.as_ref().map_or(JsValue::null(), |b| b.5.clone()),
        };
        Ok(Request { url, method, headers, body: GcRefCell::new(body), body_used: std::cell::Cell::new(false), options, signal })
    }
}

/// Register `Request`.
pub(crate) fn install(cx: &mut Context) {
    if let Err(e) = cx.register_global_class::<Request>() {
        eprintln!("js: Request: {e}");
    }
}
