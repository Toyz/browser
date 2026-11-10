//! The Fetch standard's body mixin, for native classes with a body
//! (`Request`, `Response`): `text`, `json`, `arrayBuffer`, `bytes`, `blob`
//! and `formData`, each reading the body once, and `bodyUsed`.

use boa_engine::class::ClassBuilder;
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::{JsArray, JsPromise};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, NativeObject, js_string};

use crate::dom::js_value_string;

/// A native object with a body.
pub(crate) trait HasBody: NativeObject + Sized {
    /// The body's bytes, once (`None`: already read).
    fn take_body(&self, cx: &mut Context) -> JsResult<Option<Vec<u8>>>;
    fn body_used(&self) -> bool;
    /// Its `Headers` (for the blob's type).
    fn headers(&self) -> JsObject;
}

fn this_one<T: HasBody>(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|o| o.downcast_ref::<T>().is_some())
        .ok_or_else(|| JsNativeError::typ().with_message("not a body").into())
}

/// The body's bytes (a TypeError once read).
fn bytes_of<T: HasBody>(this: &JsValue, cx: &mut Context) -> JsResult<Vec<u8>> {
    let o = this_one::<T>(this)?;
    let taken = o.downcast_ref::<T>().expect("checked").take_body(cx)?;
    taken.ok_or_else(|| JsNativeError::typ().with_message("Body has already been consumed.").into())
}

/// A promise of what `make` makes of the body.
fn read<T: HasBody>(this: &JsValue, cx: &mut Context, make: fn(Vec<u8>, &JsValue, &mut Context) -> JsResult<JsValue>) -> JsResult<JsValue> {
    let result = bytes_of::<T>(this, cx).and_then(|bytes| make(bytes, this, cx));
    Ok(match result {
        Ok(v) => JsPromise::resolve(v, cx)?.into(),
        Err(e) => JsPromise::reject(e, cx)?.into(),
    })
}

pub(crate) fn text(bytes: &[u8]) -> JsValue {
    JsString::from(String::from_utf8_lossy(bytes).as_ref()).into()
}

/// `JSON.parse` of the bytes as text.
pub(crate) fn json(bytes: &[u8], cx: &mut Context) -> JsResult<JsValue> {
    let json = cx.intrinsics().objects().json();
    let parse = json.get(js_string!("parse"), cx)?;
    parse.as_callable().expect("JSON.parse").call(&json.clone().into(), &[text(bytes)], cx)
}

/// A `Blob` of the bytes, typed by `headers`' content type.
pub(crate) fn blob(bytes: Vec<u8>, headers: &JsObject, cx: &mut Context) -> JsResult<JsValue> {
    let get = headers.get(js_string!("get"), cx)?;
    let kind = get.as_callable().map(|g| g.call(&headers.clone().into(), &[js_string!("content-type").into()], cx)).transpose()?;
    let kind = kind.filter(|k| !k.is_null()).map(|k| js_value_string(&k, cx)).unwrap_or_default();
    Ok(crate::blob::new_blob(bytes.into(), kind, cx))
}

fn text_of<T: HasBody>(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    read::<T>(this, cx, |b, _, _| Ok(text(&b)))
}
fn json_of<T: HasBody>(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    read::<T>(this, cx, |b, _, cx| json(&b, cx))
}
fn array_buffer_of<T: HasBody>(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    read::<T>(this, cx, |b, _, cx| crate::blob::array_buffer(&b, cx))
}
fn bytes_promise<T: HasBody>(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    read::<T>(this, cx, |b, _, cx| crate::blob::uint8_array(&b, cx))
}
fn blob_of<T: HasBody>(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    read::<T>(this, cx, |b, this, cx| {
        let headers = this_one::<T>(this)?.downcast_ref::<T>().expect("checked").headers();
        blob(b, &headers, cx)
    })
}
/// `application/x-www-form-urlencoded` bodies as a `FormData`.
fn form_data_of<T: HasBody>(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    read::<T>(this, cx, |b, _, cx| {
        let form = cx.global_object().get(js_string!("FormData"), cx)?;
        let form = form.as_constructor().ok_or_else(|| JsNativeError::typ().with_message("no FormData"))?.construct(&[], None, cx)?;
        let append = form.get(js_string!("append"), cx)?;
        let append = append.as_callable().expect("FormData.append").clone();
        for (k, v) in url::form_urlencoded::parse(&b) {
            append.call(&form.clone().into(), &[JsString::from(k.as_ref()).into(), JsString::from(v.as_ref()).into()], cx)?;
        }
        Ok(form.into())
    })
}

/// Add the body methods and `bodyUsed` to `T`'s class.
pub(crate) fn add_to<T: HasBody>(class: &mut ClassBuilder<'_>) {
    type Method = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;
    let methods: [(&str, Method); 6] = [
        ("text", text_of::<T>),
        ("json", json_of::<T>),
        ("arrayBuffer", array_buffer_of::<T>),
        ("bytes", bytes_promise::<T>),
        ("blob", blob_of::<T>),
        ("formData", form_data_of::<T>),
    ];
    for (name, f) in methods {
        class.method(JsString::from(name), 0, NativeFunction::from_fn_ptr(f));
    }
    let used = NativeFunction::from_fn_ptr(|this, _, _| Ok(this_one::<T>(this)?.downcast_ref::<T>().expect("checked").body_used().into()));
    let realm = class.context().realm().clone();
    let used = FunctionObjectBuilder::new(&realm, used).name(js_string!("get bodyUsed")).build();
    class.accessor(js_string!("bodyUsed"), Some(used), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
}

/// `new Headers(init)`.
pub(crate) fn new_headers(init: &JsValue, cx: &mut Context) -> JsResult<JsObject> {
    let class = cx.global_object().get(js_string!("Headers"), cx)?;
    let class = class.as_constructor().ok_or_else(|| JsNativeError::typ().with_message("no Headers"))?;
    let args: &[JsValue] = if init.is_undefined() { &[] } else { std::slice::from_ref(init) };
    class.construct(args, None, cx)
}

/// A `Headers`' pairs (as stored).
pub(crate) fn header_pairs(headers: &JsObject, cx: &mut Context) -> JsResult<Vec<(String, String)>> {
    let list = headers.get(js_string!("_list"), cx)?;
    let list = list.as_callable().map(|f| f.call(&headers.clone().into(), &[], cx)).transpose()?;
    let mut out = Vec::new();
    if let Some(list) = list.and_then(|l| l.as_object()).and_then(|o| JsArray::from_object(o).ok()) {
        for i in 0..list.length(cx)? {
            let Some(pair) = list.get(i, cx)?.as_object() else { continue };
            out.push((js_value_string(&pair.get(0, cx)?, cx), js_value_string(&pair.get(1, cx)?, cx)));
        }
    }
    Ok(out)
}
