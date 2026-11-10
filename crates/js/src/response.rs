//! `Response`, native, with the whole body (a worker's: the page's is
//! still the prelude's, which streams). Made by scripts (`new Response`,
//! `Response.json`/`error`/`redirect`) or from what the network answered.

use std::cell::RefCell;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, Trace};

use crate::dom::js_value_string;

#[derive(Trace, Finalize, JsData)]
pub(crate) struct Response {
    #[unsafe_ignore_trace]
    status: u16,
    #[unsafe_ignore_trace]
    status_text: String,
    /// `basic`, `cors`, `opaque`, `error`, `default`.
    #[unsafe_ignore_trace]
    kind: String,
    #[unsafe_ignore_trace]
    url: String,
    #[unsafe_ignore_trace]
    redirected: bool,
    headers: JsObject,
    /// `None` once read.
    #[unsafe_ignore_trace]
    body: RefCell<Option<Vec<u8>>>,
}

impl crate::body::HasBody for Response {
    fn take_body(&self, _: &mut Context) -> JsResult<Option<Vec<u8>>> {
        Ok(self.body.borrow_mut().take())
    }
    fn body_used(&self) -> bool {
        self.body.borrow().is_none()
    }
    fn headers(&self) -> JsObject {
        self.headers.clone()
    }
}

fn with<R>(this: &JsValue, f: impl FnOnce(&Response) -> R) -> JsResult<R> {
    let o = this.as_object().filter(|o| o.downcast_ref::<Response>().is_some()).ok_or_else(|| JsNativeError::typ().with_message("not a Response"))?;
    let r = o.downcast_ref::<Response>().expect("checked");
    Ok(f(&r))
}

fn getter(class: &mut ClassBuilder<'_>, name: &str, f: NativeFunction) {
    let realm = class.context().realm().clone();
    let get = FunctionObjectBuilder::new(&realm, f).name(JsString::from(format!("get {name}"))).build();
    class.accessor(JsString::from(name), Some(get), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
}

/// `headers.set(name, value)` unless it's there.
fn default_header(headers: &JsObject, name: &str, value: &str, cx: &mut Context) -> JsResult<()> {
    let has = headers.get(js_string!("has"), cx)?;
    let present = has.as_callable().map(|f| f.call(&headers.clone().into(), &[JsString::from(name).into()], cx)).transpose()?.is_some_and(|v| v.to_boolean());
    if !present {
        let set = headers.get(js_string!("set"), cx)?;
        if let Some(set) = set.as_callable() {
            set.call(&headers.clone().into(), &[JsString::from(name).into(), JsString::from(value).into()], cx)?;
        }
    }
    Ok(())
}

/// A script-made response: `body` (encoded), `init`'s status and headers.
fn make(body: &JsValue, init: Option<&JsObject>, cx: &mut Context) -> JsResult<Response> {
    let get = |key: &str, cx: &mut Context| -> JsResult<JsValue> { Ok(init.map(|o| o.get(JsString::from(key), cx)).transpose()?.unwrap_or_default()) };
    let status = get("status", cx)?;
    let status = if status.is_undefined() { 200.0 } else { status.to_number(cx)? };
    if !(200.0..=599.0).contains(&status) {
        return Err(JsNativeError::range().with_message(format!("Response: status {status} is outside 200-599")).into());
    }
    let status_text = get("statusText", cx)?;
    let status_text = if status_text.is_undefined() { String::new() } else { js_value_string(&status_text, cx) };
    let headers = crate::body::new_headers(&get("headers", cx)?, cx)?;
    let bytes = if body.is_null_or_undefined() {
        Vec::new()
    } else {
        let (bytes, kind) = crate::blob::encode(body, cx)?;
        if let Some(kind) = kind {
            default_header(&headers, "content-type", &kind, cx)?;
        }
        bytes
    };
    Ok(Response { status: status as u16, status_text, kind: "default".into(), url: String::new(), redirected: false, headers, body: RefCell::new(Some(bytes)) })
}

/// A response the network gave.
pub(crate) fn from_network(r: crate::cors::ScriptResponse, cx: &mut Context) -> JsResult<JsValue> {
    let pairs: Vec<JsValue> = r.headers.iter().map(|(k, v)| JsArray::from_iter([JsString::from(k.as_str()).into(), JsString::from(v.as_str()).into()], cx).into()).collect();
    let headers = crate::body::new_headers(&JsArray::from_iter(pairs, cx).into(), cx)?;
    let data = Response { status: r.status, status_text: r.status_text, kind: r.kind.into(), url: r.url.to_string(), redirected: r.redirected, headers, body: RefCell::new(Some(r.body)) };
    Ok(Response::from_data(data, cx)?.into())
}

type Method = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

impl Class for Response {
    const NAME: &'static str = "Response";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let getters: [(&str, Method); 7] = [
            ("status", |this, _, _| Ok(with(this, |r| r.status)?.into())),
            ("statusText", |this, _, _| Ok(JsString::from(with(this, |r| r.status_text.clone())?).into())),
            ("ok", |this, _, _| Ok(with(this, |r| (200..300).contains(&r.status))?.into())),
            ("headers", |this, _, _| Ok(with(this, |r| r.headers.clone())?.into())),
            ("url", |this, _, _| Ok(JsString::from(with(this, |r| r.url.clone())?).into())),
            ("type", |this, _, _| Ok(JsString::from(with(this, |r| r.kind.clone())?).into())),
            ("redirected", |this, _, _| Ok(with(this, |r| r.redirected)?.into())),
        ];
        for (name, f) in getters {
            getter(class, name, NativeFunction::from_fn_ptr(f));
        }
        // No streams here: the body is read whole.
        getter(class, "body", NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::null())));
        crate::body::add_to::<Response>(class);
        class.method(js_string!("clone"), 0, NativeFunction::from_fn_ptr(|this, _, cx| {
            let (status, status_text, kind, url, redirected, headers, body) =
                with(this, |r| (r.status, r.status_text.clone(), r.kind.clone(), r.url.clone(), r.redirected, r.headers.clone(), r.body.borrow().clone()))?;
            let body = body.ok_or_else(|| JsNativeError::typ().with_message("Response.clone: Body has already been consumed."))?;
            let headers = crate::body::new_headers(&headers.into(), cx)?;
            Ok(Response::from_data(Response { status, status_text, kind, url, redirected, headers, body: RefCell::new(Some(body)) }, cx)?.into())
        }));
        let statics: [(&str, usize, Method); 3] = [
            ("json", 1, |_, a, cx| {
                let json = cx.intrinsics().objects().json();
                let stringify = json.get(js_string!("stringify"), cx)?;
                let text = stringify.as_callable().expect("JSON.stringify").call(&json.clone().into(), &[a.first().cloned().unwrap_or_default()], cx)?;
                let r = make(&text, a.get(1).and_then(JsValue::as_object).as_ref(), cx)?;
                let headers = r.headers.clone();
                let set = headers.get(js_string!("set"), cx)?;
                if let Some(set) = set.as_callable() {
                    set.call(&headers.clone().into(), &[js_string!("content-type").into(), js_string!("application/json").into()], cx)?;
                }
                Ok(Response::from_data(r, cx)?.into())
            }),
            ("error", 0, |_, _, cx| {
                let headers = crate::body::new_headers(&JsValue::undefined(), cx)?;
                let r = Response { status: 0, status_text: String::new(), kind: "error".into(), url: String::new(), redirected: false, headers, body: RefCell::new(Some(Vec::new())) };
                Ok(Response::from_data(r, cx)?.into())
            }),
            ("redirect", 1, |_, a, cx| {
                let url = js_value_string(&a.first().cloned().unwrap_or_default(), cx);
                let url = crate::dom::resolve(cx, &url).ok_or_else(|| JsNativeError::typ().with_message("Response.redirect: invalid URL"))?;
                let status = match a.get(1) {
                    Some(s) if !s.is_undefined() => s.to_number(cx)? as u16,
                    _ => 302,
                };
                if ![301, 302, 303, 307, 308].contains(&status) {
                    return Err(JsNativeError::range().with_message(format!("Response.redirect: {status} isn't a redirect status")).into());
                }
                let headers = crate::body::new_headers(&JsValue::undefined(), cx)?;
                default_header(&headers, "location", url.as_str(), cx)?;
                let r = Response { status, status_text: String::new(), kind: "default".into(), url: String::new(), redirected: false, headers, body: RefCell::new(Some(Vec::new())) };
                Ok(Response::from_data(r, cx)?.into())
            }),
        ];
        for (name, len, f) in statics {
            class.static_method(JsString::from(name), len, NativeFunction::from_fn_ptr(f));
        }
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        make(&a.first().cloned().unwrap_or_default(), a.get(1).and_then(JsValue::as_object).as_ref(), cx)
    }
}

/// Register `Response`.
pub(crate) fn install(cx: &mut Context) {
    if let Err(e) = cx.register_global_class::<Response>() {
        eprintln!("js: Response: {e}");
    }
}
