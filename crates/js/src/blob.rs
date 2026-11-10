//! `Blob` and `File`, native, holding real bytes (a string-backed Blob
//! mangled anything binary), and the buffer helpers bodies use: bytes in
//! from ArrayBuffers, views and blobs, bytes out as ArrayBuffers.

use std::sync::Arc;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::{AlignedVec, JsArray, JsArrayBuffer, JsPromise, JsUint8Array};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, Trace};

use crate::dom::js_value_string;

/// A view's buffer, byte offset and length (`None`: not a view).
pub(crate) fn view_of(o: &JsObject, cx: &mut Context) -> JsResult<Option<(JsArrayBuffer, usize, usize)>> {
    let buffer = o.get(js_string!("buffer"), cx)?;
    let Some(buffer) = buffer.as_object().and_then(|b| JsArrayBuffer::from_object(b).ok()) else { return Ok(None) };
    let offset = o.get(js_string!("byteOffset"), cx)?.to_index(cx)? as usize;
    let len = o.get(js_string!("byteLength"), cx)?.to_index(cx)? as usize;
    Ok(Some((buffer, offset, len)))
}

/// The bytes of an ArrayBuffer, a view of one, or a blob (`None`: none of
/// those).
pub(crate) fn binary_of(v: &JsValue, cx: &mut Context) -> JsResult<Option<Vec<u8>>> {
    let Some(o) = v.as_object() else { return Ok(None) };
    if let Ok(buffer) = JsArrayBuffer::from_object(o.clone()) {
        return Ok(Some(buffer.data().map(|d| d.to_vec()).unwrap_or_default()));
    }
    if let Some((bytes, _)) = blob_of(&o) {
        return Ok(Some(bytes.to_vec()));
    }
    let Some((buffer, offset, len)) = view_of(&o, cx)? else { return Ok(None) };
    Ok(Some(buffer.data().and_then(|d| d.get(offset..offset + len).map(<[u8]>::to_vec)).unwrap_or_default()))
}

/// The bytes of an ArrayBuffer or a view of one (a TypeError otherwise).
pub(crate) fn bytes_of(v: &JsValue, cx: &mut Context) -> JsResult<Vec<u8>> {
    binary_of(v, cx)?.ok_or_else(|| JsNativeError::typ().with_message("expected an ArrayBuffer or a view of one").into())
}

/// A new ArrayBuffer holding `bytes`.
pub(crate) fn array_buffer(bytes: &[u8], cx: &mut Context) -> JsResult<JsValue> {
    Ok(JsArrayBuffer::from_byte_block(AlignedVec::from_slice(0, bytes), cx)?.into())
}

/// A new Uint8Array holding `bytes`.
pub(crate) fn uint8_array(bytes: &[u8], cx: &mut Context) -> JsResult<JsValue> {
    let buffer = JsArrayBuffer::from_byte_block(AlignedVec::from_slice(0, bytes), cx)?;
    Ok(JsUint8Array::from_array_buffer(buffer, cx)?.into())
}

/// A blob's bytes and type (`File`s are blobs too).
/// `URL.createObjectURL` blobs, by URL (any thread may read them: a
/// worker loads its script from one).
static OBJECT_URLS: std::sync::Mutex<Option<std::collections::HashMap<String, (Arc<[u8]>, String)>>> = std::sync::Mutex::new(None);

/// A `blob:` URL for blob `o` (under `origin`), `None` if it isn't one.
pub(crate) fn object_url(o: &JsObject, origin: &str) -> Option<String> {
    use ring::rand::SecureRandom;
    let blob = blob_of(o)?;
    let mut b = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut b).ok();
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let url = format!("blob:{origin}/{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]);
    let mut urls = OBJECT_URLS.lock().unwrap_or_else(|e| e.into_inner());
    urls.get_or_insert_with(Default::default).insert(url.clone(), blob);
    Some(url)
}

/// What a `blob:` URL is (its bytes and type), while it isn't revoked.
pub(crate) fn object_url_data(url: &str) -> Option<(Arc<[u8]>, String)> {
    OBJECT_URLS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(url).cloned()
}

pub(crate) fn revoke_object_url(url: &str) {
    if let Some(urls) = OBJECT_URLS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        urls.remove(url);
    }
}

/// Whether `o` is a `File`.
pub(crate) fn is_file(o: &JsObject) -> bool {
    o.downcast_ref::<File>().is_some()
}

pub(crate) fn blob_of(o: &JsObject) -> Option<(Arc<[u8]>, String)> {
    if let Some(b) = o.downcast_ref::<Blob>() {
        return Some((b.bytes.clone(), b.kind.clone()));
    }
    o.downcast_ref::<File>().map(|f| (f.blob.bytes.clone(), f.blob.kind.clone()))
}

#[derive(Trace, Finalize, JsData)]
pub(crate) struct Blob {
    #[unsafe_ignore_trace]
    bytes: Arc<[u8]>,
    /// Its `type`.
    #[unsafe_ignore_trace]
    kind: String,
}

#[derive(Trace, Finalize, JsData)]
struct File {
    blob: Blob,
    #[unsafe_ignore_trace]
    name: String,
    #[unsafe_ignore_trace]
    modified: f64,
}

/// A blob from its constructor's parts and options.
fn blob_from(parts: &JsValue, opts: &JsValue, cx: &mut Context) -> JsResult<Blob> {
    let mut bytes = Vec::new();
    if let Some(list) = parts.as_object().and_then(|o| JsArray::from_object(o).ok()) {
        for i in 0..list.length(cx)? {
            let part = list.get(i, cx)?;
            match binary_of(&part, cx)? {
                Some(b) => bytes.extend(b),
                None => bytes.extend(js_value_string(&part, cx).into_bytes()),
            }
        }
    } else if !parts.is_null_or_undefined() {
        return Err(JsNativeError::typ().with_message("Blob parts must be a sequence").into());
    }
    let kind = match opts.as_object() {
        Some(o) => {
            let t = o.get(js_string!("type"), cx)?;
            if t.is_undefined() { String::new() } else { js_value_string(&t, cx) }
        }
        None => String::new(),
    };
    // A type with anything but printable ASCII is no type.
    let kind = if kind.bytes().all(|b| (0x20..0x7f).contains(&b)) { kind.to_ascii_lowercase() } else { String::new() };
    Ok(Blob { bytes: bytes.into(), kind })
}

fn this_blob(this: &JsValue) -> JsResult<(Arc<[u8]>, String)> {
    this.as_object().as_ref().and_then(blob_of).ok_or_else(|| JsNativeError::typ().with_message("not a Blob").into())
}

pub(crate) fn new_blob(bytes: Arc<[u8]>, kind: String, cx: &mut Context) -> JsValue {
    let proto = cx.get_global_class::<Blob>().map(|c| c.prototype());
    JsObject::from_proto_and_data(proto, Blob { bytes, kind }).into()
}

fn promised(v: JsResult<JsValue>, cx: &mut Context) -> JsResult<JsValue> {
    match v {
        Ok(v) => JsPromise::resolve(v, cx),
        Err(e) => JsPromise::reject(e, cx),
    }
    .map(JsValue::from)
}

type Method = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

const METHODS: &[(&str, usize, Method)] = &[
    ("text", 0, |this, _, cx| {
        let text = this_blob(this).map(|(b, _)| JsValue::from(JsString::from(String::from_utf8_lossy(&b).as_ref())));
        promised(text, cx)
    }),
    ("arrayBuffer", 0, |this, _, cx| {
        let buffer = this_blob(this).and_then(|(b, _)| array_buffer(&b, cx));
        promised(buffer, cx)
    }),
    ("bytes", 0, |this, _, cx| {
        let bytes = this_blob(this).and_then(|(b, _)| uint8_array(&b, cx));
        promised(bytes, cx)
    }),
    ("slice", 0, |this, a, cx| {
        let (bytes, _) = this_blob(this)?;
        let len = bytes.len() as f64;
        let at = |v: Option<&JsValue>, default: f64, cx: &mut Context| -> JsResult<usize> {
            let n = match v.filter(|v| !v.is_undefined()) {
                Some(v) => v.to_integer_or_infinity(cx)?.clamp_finite(-len as i64, len as i64) as f64,
                None => default,
            };
            Ok((if n < 0.0 { (len + n).max(0.0) } else { n.min(len) }) as usize)
        };
        let start = at(a.first(), 0.0, cx)?;
        let end = at(a.get(1), len, cx)?.max(start);
        let kind = match a.get(2).filter(|v| !v.is_undefined()) {
            Some(t) => js_value_string(t, cx).to_ascii_lowercase(),
            None => String::new(),
        };
        Ok(new_blob(bytes[start..end].into(), kind, cx))
    }),
    // A stream of its bytes, in one chunk (the page's ReadableStream).
    ("stream", 0, |this, _, cx| {
        let (bytes, _) = this_blob(this)?;
        let class = cx.global_object().get(js_string!("ReadableStream"), cx)?;
        let class = class.as_constructor().ok_or_else(|| JsNativeError::typ().with_message("no ReadableStream"))?;
        let stream = class.construct(&[], None, cx)?;
        let chunk = uint8_array(&bytes, cx)?;
        for (method, args) in [("_push", vec![chunk]), ("_close", Vec::new())] {
            if let Some(f) = stream.get(JsString::from(method), cx)?.as_callable() {
                f.call(&stream.clone().into(), &args, cx)?;
            }
        }
        Ok(stream.into())
    }),
];

fn getter(class: &mut ClassBuilder<'_>, name: &str, f: Method) {
    let realm = class.context().realm().clone();
    let get = FunctionObjectBuilder::new(&realm, NativeFunction::from_fn_ptr(f)).name(JsString::from(name)).build();
    class.accessor(JsString::from(name), Some(get), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
}

impl Class for Blob {
    const NAME: &'static str = "Blob";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        for &(name, len, f) in METHODS {
            class.method(JsString::from(name), len, NativeFunction::from_fn_ptr(f));
        }
        getter(class, "size", |this, _, _| Ok(JsValue::from(this_blob(this)?.0.len() as f64)));
        getter(class, "type", |this, _, _| Ok(JsString::from(this_blob(this)?.1).into()));
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        blob_from(&a.first().cloned().unwrap_or_default(), &a.get(1).cloned().unwrap_or_default(), cx)
    }
}

impl Class for File {
    const NAME: &'static str = "File";
    const LENGTH: usize = 2;

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        getter(class, "name", |this, _, _| {
            let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a File"))?;
            let f = o.downcast_ref::<File>().ok_or_else(|| JsNativeError::typ().with_message("not a File"))?;
            Ok(JsString::from(f.name.as_str()).into())
        });
        getter(class, "lastModified", |this, _, _| {
            let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a File"))?;
            let f = o.downcast_ref::<File>().ok_or_else(|| JsNativeError::typ().with_message("not a File"))?;
            Ok(JsValue::from(f.modified))
        });
        getter(class, "webkitRelativePath", |_, _, _| Ok(js_string!("").into()));
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let opts = a.get(2).cloned().unwrap_or_default();
        let blob = blob_from(&a.first().cloned().unwrap_or_default(), &opts, cx)?;
        let name = js_value_string(&a.get(1).cloned().unwrap_or_default(), cx);
        let modified = match opts.as_object() {
            Some(o) => o.get(js_string!("lastModified"), cx)?.as_number(),
            None => None,
        };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64);
        Ok(File { blob, name, modified: modified.unwrap_or(now) })
    }
}

/// Is `v` an instance of the global class `name`?
fn is_a(v: &JsValue, name: &str, cx: &mut Context) -> JsResult<bool> {
    let class = cx.global_object().get(JsString::from(name), cx)?;
    if !class.is_callable() {
        return Ok(false);
    }
    v.instance_of(&class, cx)
}

/// A random multipart boundary.
fn boundary() -> String {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 12];
    ring::rand::SystemRandom::new().fill(&mut b).ok();
    format!("----FeatherFormBoundary{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>())
}

/// `encodeBody(body)`: a request or response body (string,
/// URLSearchParams, FormData, Blob, ArrayBuffer or view) as the network
/// sends it, `[string | ArrayBuffer, content type | null]`.
fn encode_body(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let body = a.first().cloned().unwrap_or_default();
    let text = |s: String, kind: &str| (JsValue::from(JsString::from(s)), Some(kind.to_owned()));
    let (payload, kind): (JsValue, Option<String>) = if body.is_string() {
        (body.clone(), Some("text/plain;charset=UTF-8".to_owned()))
    } else if let Some((bytes, kind)) = body.as_object().as_ref().and_then(blob_of) {
        (array_buffer(&bytes, cx)?, Some(kind).filter(|k| !k.is_empty()))
    } else if let Some(bytes) = binary_of(&body, cx)? {
        (array_buffer(&bytes, cx)?, None)
    } else if is_a(&body, "URLSearchParams", cx)? {
        text(js_value_string(&body, cx), "application/x-www-form-urlencoded;charset=UTF-8")
    } else if let Some(entries) = body.as_object().as_ref().and_then(crate::form_data::entries_of) {
        use crate::form_data::Value;
        let boundary = boundary();
        let mut out = Vec::new();
        for (name, value) in entries {
            out.extend(format!("--{boundary}\r\n").bytes());
            match &value {
                Value::File(o) => {
                    let (bytes, kind) = blob_of(o).unwrap_or_default();
                    let file = o.downcast_ref::<File>().map_or_else(|| "blob".to_owned(), |f| f.name.clone());
                    let kind = if kind.is_empty() { "application/octet-stream".to_owned() } else { kind };
                    out.extend(format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\nContent-Type: {kind}\r\n\r\n").bytes());
                    out.extend_from_slice(&bytes);
                }
                Value::Text(text) => {
                    out.extend(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").bytes());
                    out.extend(text.bytes());
                }
            }
            out.extend(b"\r\n");
        }
        out.extend(format!("--{boundary}--\r\n").bytes());
        (array_buffer(&out, cx)?, Some(format!("multipart/form-data; boundary={boundary}")))
    } else {
        text(js_value_string(&body, cx), "text/plain;charset=UTF-8")
    };
    let kind = kind.map_or(JsValue::null(), |k| JsString::from(k).into());
    Ok(JsArray::from_iter([payload, kind], cx).into())
}

/// A body as bytes and its content type (as `encodeBody` makes it).
pub(crate) fn encode(body: &JsValue, cx: &mut Context) -> JsResult<(Vec<u8>, Option<String>)> {
    let pair = encode_body(&JsValue::undefined(), std::slice::from_ref(body), cx)?;
    let pair = pair.as_object().and_then(|o| JsArray::from_object(o).ok()).expect("a pair");
    let (payload, kind) = (pair.get(0, cx)?, pair.get(1, cx)?);
    let bytes = match payload.as_string() {
        Some(s) => s.to_std_string_escaped().into_bytes(),
        None => binary_of(&payload, cx)?.unwrap_or_default(),
    };
    let kind = (!kind.is_null()).then(|| js_value_string(&kind, cx));
    Ok((bytes, kind))
}

/// Register `Blob` and `File` (a File is a Blob: its prototype's), and the
/// `encodeBody` native.
pub(crate) fn install(natives: &JsObject, cx: &mut Context) {
    let f = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(encode_body)).name(js_string!("encodeBody")).build();
    natives.set(js_string!("encodeBody"), f, false, cx).ok();
    for r in [cx.register_global_class::<Blob>(), cx.register_global_class::<File>()] {
        if let Err(e) = r {
            eprintln!("js: blob: {e}");
        }
    }
    if let (Some(blob), Some(file)) = (cx.get_global_class::<Blob>(), cx.get_global_class::<File>()) {
        file.prototype().set_prototype(Some(blob.prototype()));
        file.constructor().set_prototype(Some(blob.constructor()));
    }
}
