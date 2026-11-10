//! `TextEncoder`, `TextDecoder` (every encoding the Encoding Standard
//! names, through `encoding_rs`, streaming included) and the base64 of
//! `atob`/`btoa`, natively.

use std::cell::RefCell;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::builtins::{JsArrayBuffer, JsDataView, JsTypedArray, JsUint8Array};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsArgs, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, Trace};

fn type_error(message: &str) -> boa_engine::JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}

fn range_error(message: &str) -> boa_engine::JsError {
    JsNativeError::range().with_message(message.to_owned()).into()
}

/// The bytes of a `BufferSource` (an ArrayBuffer, a typed array or a
/// DataView: the part of the buffer it views).
pub(crate) fn buffer_bytes(value: &JsValue, cx: &mut Context) -> JsResult<Vec<u8>> {
    let Some(object) = value.as_object() else {
        return if value.is_undefined() { Ok(Vec::new()) } else { Err(type_error("expected an ArrayBuffer or a view of one")) };
    };
    let view = |buffer: JsValue, offset: usize, len: usize| -> JsResult<Vec<u8>> {
        let buffer = JsArrayBuffer::from_object(buffer.as_object().ok_or_else(|| type_error("detached buffer"))?.clone())?;
        let data = buffer.data().ok_or_else(|| type_error("detached buffer"))?;
        Ok(data.get(offset..offset + len).ok_or_else(|| range_error("view out of range"))?.to_vec())
    };
    if let Ok(buffer) = JsArrayBuffer::from_object(object.clone()) {
        return Ok(buffer.data().map(|d| d.to_vec()).unwrap_or_default());
    }
    if let Ok(array) = JsTypedArray::from_object(object.clone()) {
        let (offset, len) = (array.byte_offset(cx)?, array.byte_length(cx)?);
        return view(array.buffer(cx)?, offset, len);
    }
    if let Ok(dv) = JsDataView::from_object(object.clone()) {
        let (offset, len) = (dv.byte_offset(cx)? as usize, dv.byte_length(cx)? as usize);
        return view(dv.buffer(cx)?, offset, len);
    }
    Err(type_error("expected an ArrayBuffer or a view of one"))
}

#[derive(Debug, Trace, Finalize, JsData)]
pub struct WebTextEncoder;

impl Class for WebTextEncoder {
    const NAME: &'static str = "TextEncoder";

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Ok(WebTextEncoder)
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let realm = class.context().realm().clone();
        let encoding = NativeFunction::from_fn_ptr(|_, _, _| Ok(js_string!("utf-8").into())).to_js_function(&realm);
        class.accessor(js_string!("encoding"), Some(encoding), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        class.method(
            js_string!("encode"),
            0,
            NativeFunction::from_fn_ptr(|_, args, cx| {
                let input = args.get(0).filter(|v| !v.is_undefined()).map(|v| v.to_string(cx)).transpose()?;
                // Lone surrogates become U+FFFD (USVString conversion).
                let text = input.map(|s| String::from_utf16_lossy(&s.iter().collect::<Vec<u16>>())).unwrap_or_default();
                Ok(JsUint8Array::from_iter(text.into_bytes(), cx)?.into())
            }),
        );
        class.method(
            js_string!("encodeInto"),
            2,
            NativeFunction::from_fn_ptr(|_, args, cx| {
                let text = String::from_utf16_lossy(&args.get_or_undefined(0).to_string(cx)?.iter().collect::<Vec<u16>>());
                let target = args.get_or_undefined(1).as_object().ok_or_else(|| type_error("encodeInto needs a Uint8Array"))?.clone();
                let target = JsUint8Array::from_object(target)?;
                let capacity = target.length(cx)?;
                // Whole characters only.
                let (mut read, mut written) = (0usize, 0usize);
                for ch in text.chars() {
                    let len = ch.len_utf8();
                    if written + len > capacity {
                        break;
                    }
                    let mut buf = [0u8; 4];
                    for (i, b) in ch.encode_utf8(&mut buf).bytes().enumerate() {
                        target.set(written + i, b, true, cx)?;
                    }
                    written += len;
                    read += ch.len_utf16();
                }
                let result = JsObject::with_object_proto(cx.intrinsics());
                result.set(js_string!("read"), read as u32, true, cx)?;
                result.set(js_string!("written"), written as u32, true, cx)?;
                Ok(result.into())
            }),
        );
        Ok(())
    }
}

#[derive(Trace, Finalize, JsData)]
pub struct WebTextDecoder {
    #[unsafe_ignore_trace]
    encoding: &'static encoding_rs::Encoding,
    fatal: bool,
    ignore_bom: bool,
    /// Between `{ stream: true }` calls: what's left of a split character.
    #[unsafe_ignore_trace]
    decoder: RefCell<Option<encoding_rs::Decoder>>,
}

impl std::fmt::Debug for WebTextDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TextDecoder({})", self.encoding.name())
    }
}

impl WebTextDecoder {
    fn new_decoder(&self) -> encoding_rs::Decoder {
        if self.ignore_bom { self.encoding.new_decoder_without_bom_handling() } else { self.encoding.new_decoder_with_bom_removal() }
    }
}

impl Class for WebTextDecoder {
    const NAME: &'static str = "TextDecoder";

    fn data_constructor(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let label = match args.get(0).filter(|v| !v.is_undefined()) {
            Some(v) => v.to_string(cx)?.to_std_string_escaped(),
            None => "utf-8".into(),
        };
        // The Encoding Standard's labels; "replacement" isn't one to decode with.
        let encoding = encoding_rs::Encoding::for_label(label.trim().as_bytes())
            .filter(|e| *e != encoding_rs::REPLACEMENT)
            .ok_or_else(|| range_error(&format!("The encoding label provided ('{label}') is invalid.")))?;
        let options = args.get_or_undefined(1);
        let flag = |name: &str, cx: &mut Context| -> JsResult<bool> {
            match options.as_object() {
                Some(o) => Ok(o.get(JsString::from(name), cx)?.to_boolean()),
                None => Ok(false),
            }
        };
        Ok(WebTextDecoder { encoding, fatal: flag("fatal", cx)?, ignore_bom: flag("ignoreBOM", cx)?, decoder: RefCell::new(None) })
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let realm = class.context().realm().clone();
        let getter = |f: fn(&WebTextDecoder) -> JsValue| {
            NativeFunction::from_copy_closure(move |this, _, _| {
                let o = this.as_object().ok_or_else(|| type_error("Illegal invocation"))?;
                let d = o.downcast_ref::<WebTextDecoder>().ok_or_else(|| type_error("Illegal invocation"))?;
                Ok(f(&d))
            })
            .to_js_function(&realm)
        };
        let attr = Attribute::CONFIGURABLE | Attribute::ENUMERABLE;
        class.accessor(js_string!("encoding"), Some(getter(|d| JsString::from(d.encoding.name().to_ascii_lowercase()).into())), None, attr);
        class.accessor(js_string!("fatal"), Some(getter(|d| d.fatal.into())), None, attr);
        class.accessor(js_string!("ignoreBOM"), Some(getter(|d| d.ignore_bom.into())), None, attr);
        class.method(
            js_string!("decode"),
            0,
            NativeFunction::from_fn_ptr(|this, args, cx| {
                let o = this.as_object().ok_or_else(|| type_error("Illegal invocation"))?;
                let bytes = buffer_bytes(args.get_or_undefined(0), cx)?;
                let stream = match args.get_or_undefined(1).as_object() {
                    Some(opts) => opts.get(js_string!("stream"), cx)?.to_boolean(),
                    None => false,
                };
                let d = o.downcast_ref::<WebTextDecoder>().ok_or_else(|| type_error("Illegal invocation"))?;
                let mut slot = d.decoder.borrow_mut();
                let mut decoder = slot.take().unwrap_or_else(|| d.new_decoder());
                let mut out = String::with_capacity(decoder.max_utf8_buffer_length(bytes.len()).unwrap_or(bytes.len() * 3));
                let last = !stream;
                let malformed = if d.fatal {
                    let (result, _) = decoder.decode_to_string_without_replacement(&bytes, &mut out, last);
                    matches!(result, encoding_rs::DecoderResult::Malformed(..))
                } else {
                    let (_, _, had_errors) = decoder.decode_to_string(&bytes, &mut out, last);
                    let _ = had_errors;
                    false
                };
                if malformed {
                    return Err(type_error("The encoded data was not valid."));
                }
                // A stream keeps its decoder (and a split character) for next time.
                if stream {
                    *slot = Some(decoder);
                }
                Ok(JsString::from(out.as_str()).into())
            }),
        );
        Ok(())
    }
}

/// `btoa`: a string of code points up to U+00FF, as base64 (`None`: a
/// character out of that range).
fn btoa(input: &[u16]) -> Option<String> {
    let bytes: Option<Vec<u8>> = input.iter().map(|&c| u8::try_from(c).ok()).collect();
    Some(base64_encode(&bytes?))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Base64 (forgiving, as `atob`) as bytes.
pub(crate) fn base64_decode(input: &str) -> Option<Vec<u8>> {
    atob(input).map(|s| s.chars().map(|c| c as u8).collect())
}

/// `atob`: the HTML Standard's forgiving-base64 decode (ASCII whitespace
/// ignored, padding optional), as a latin1 string; `None` if it isn't base64.
fn atob(input: &str) -> Option<String> {
    let mut data: Vec<u8> = input.bytes().filter(|b| !matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')).collect();
    if data.len() % 4 == 0 && data.ends_with(b"==") {
        data.truncate(data.len() - 2);
    } else if data.len() % 4 == 0 && data.ends_with(b"=") {
        data.truncate(data.len() - 1);
    }
    if data.len() % 4 == 1 {
        return None;
    }
    let value = |b: u8| B64.iter().position(|&c| c == b).map(|v| v as u32);
    let mut out = String::with_capacity(data.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &b in &data {
        buffer = buffer << 6 | value(b)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(char::from(((buffer >> bits) & 0xff) as u8));
        }
    }
    Some(out)
}

/// Install `TextEncoder`, `TextDecoder` and the natives behind `atob`/`btoa`.
pub(crate) fn register(cx: &mut Context, native: &JsObject) {
    if let Err(e) = cx.register_global_class::<WebTextEncoder>() {
        eprintln!("js: TextEncoder class: {e}");
    }
    if let Err(e) = cx.register_global_class::<WebTextDecoder>() {
        eprintln!("js: TextDecoder class: {e}");
    }
    let add = |cx: &mut Context, name: &str, f: NativeFunction| {
        let f = f.to_js_function(cx.realm());
        native.set(JsString::from(name), f, false, cx).ok();
    };
    // (Also on the natives, for the prelude.)
    add(cx, "btoa", NativeFunction::from_fn_ptr(btoa_js));
    add(cx, "atob", NativeFunction::from_fn_ptr(atob_js));
    install_globals(cx);
}

fn btoa_js(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let s = args.get_or_undefined(0).to_string(cx)?;
    match btoa(&s.iter().collect::<Vec<u16>>()) {
        Some(b) => Ok(JsString::from(b.as_str()).into()),
        None => Err(crate::dom_exception::error("InvalidCharacterError", "The string to be encoded contains characters outside of the Latin1 range.", cx)),
    }
}

fn atob_js(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let s = args.get_or_undefined(0).to_string(cx)?.to_std_string_escaped();
    match atob(&s) {
        Some(b) => Ok(JsString::from(b.as_str()).into()),
        None => Err(crate::dom_exception::error("InvalidCharacterError", "The string to be decoded is not correctly encoded.", cx)),
    }
}

/// `atob` and `btoa` on the global object.
fn install_globals(cx: &mut Context) {
    for (name, f) in [("atob", atob_js as boa_engine::native_function::NativeFunctionPointer), ("btoa", btoa_js)] {
        if let Err(e) = cx.register_global_builtin_callable(JsString::from(name), 1, NativeFunction::from_fn_ptr(f)) {
            eprintln!("js: {name}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_both_ways() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(atob("aGVsbG8=").as_deref(), Some("hello"));
        assert_eq!(atob(" aGVs bG8 ").as_deref(), Some("hello"));
        assert_eq!(atob("aGVsbG8").as_deref(), Some("hello"));
        assert_eq!(atob("a"), None);
        assert_eq!(atob("a$=="), None);
        assert_eq!(btoa(&[0x100]), None);
        assert_eq!(btoa(&"\u{ff}".encode_utf16().collect::<Vec<_>>()).as_deref(), Some("/w=="));
    }
}
