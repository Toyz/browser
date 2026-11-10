//! `Headers`, native: case-insensitive names, values for a repeated name
//! combined (`set-cookie` kept apart), iterated in sorted order as the
//! Fetch standard has it.

use std::cell::RefCell;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsResult, JsString, JsSymbol, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, Trace};

use crate::dom::js_value_string;

#[derive(Trace, Finalize, JsData, Default)]
struct Headers {
    /// (lower-case name, value), in insertion order; `set-cookie` may
    /// repeat, every other name appears once.
    #[unsafe_ignore_trace]
    list: RefCell<Vec<(String, String)>>,
}

/// A header value as stored: leading and trailing whitespace trimmed.
fn normalize(v: &str) -> String {
    v.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r')).to_owned()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic() && !b"\"(),/:;<=>?@[\\]{}".contains(&b))
}

impl Headers {
    fn append(&self, name: &str, value: &str) -> JsResult<()> {
        if !valid_name(name) {
            return Err(JsNativeError::typ().with_message(format!("Headers: invalid header name {name:?}")).into());
        }
        let (name, value) = (name.to_ascii_lowercase(), normalize(value));
        let mut list = self.list.borrow_mut();
        match list.iter_mut().find(|(n, _)| *n == name).filter(|_| name != "set-cookie") {
            Some((_, v)) => {
                v.push_str(", ");
                v.push_str(&value);
            }
            None => list.push((name, value)),
        }
        Ok(())
    }

    fn set(&self, name: &str, value: &str) -> JsResult<()> {
        if !valid_name(name) {
            return Err(JsNativeError::typ().with_message(format!("Headers: invalid header name {name:?}")).into());
        }
        let name = name.to_ascii_lowercase();
        let mut list = self.list.borrow_mut();
        list.retain(|(n, _)| *n != name);
        list.push((name, normalize(value)));
        Ok(())
    }

    fn get(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_lowercase();
        let list = self.list.borrow();
        let values: Vec<&str> = list.iter().filter(|(n, _)| *n == name).map(|(_, v)| v.as_str()).collect();
        (!values.is_empty()).then(|| values.join(", "))
    }

    /// The pairs iteration yields: sorted by name, `set-cookie` one per value.
    fn sorted(&self) -> Vec<(String, String)> {
        let mut list = self.list.borrow().clone();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        list
    }
}

/// Fill `headers` from an init: another Headers, a sequence of pairs, or a
/// record.
fn fill(headers: &Headers, init: &JsValue, cx: &mut Context) -> JsResult<()> {
    let Some(o) = init.as_object() else {
        if init.is_null_or_undefined() {
            return Ok(());
        }
        return Err(JsNativeError::typ().with_message("Headers: init must be an object").into());
    };
    if let Some(other) = o.downcast_ref::<Headers>() {
        let pairs = other.list.borrow().clone();
        drop(other);
        for (n, v) in pairs {
            headers.append(&n, &v)?;
        }
        return Ok(());
    }
    // A sequence (anything iterable): pairs.
    let iterator = o.get(JsSymbol::iterator(), cx)?;
    if iterator.is_callable() {
        let array = cx.intrinsics().constructors().array().constructor();
        let from = array.get(js_string!("from"), cx)?;
        let from = from.as_callable().ok_or_else(|| JsNativeError::typ().with_message("no Array.from"))?;
        let items = from.call(&array.into(), &[init.clone()], cx)?;
        let items = JsArray::from_object(items.as_object().expect("an array"))?;
        for i in 0..items.length(cx)? {
            let pair = items.get(i, cx)?;
            let pair = pair.as_object().and_then(|p| JsArray::from_object(p).ok()).ok_or_else(|| JsNativeError::typ().with_message("Headers: each pair must be [name, value]"))?;
            if pair.length(cx)? != 2 {
                return Err(JsNativeError::typ().with_message("Headers: each pair must be [name, value]").into());
            }
            let (n, v) = (js_value_string(&pair.get(0, cx)?, cx), js_value_string(&pair.get(1, cx)?, cx));
            headers.append(&n, &v)?;
        }
        return Ok(());
    }
    // A record: its own enumerable string keys.
    for key in o.own_property_keys(cx)? {
        let boa_engine::property::PropertyKey::String(name) = &key else { continue };
        let v = o.get(key.clone(), cx)?;
        headers.append(&name.to_std_string_escaped(), &js_value_string(&v, cx))?;
    }
    Ok(())
}

fn this_headers<R>(this: &JsValue, f: impl FnOnce(&Headers) -> R) -> JsResult<R> {
    let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a Headers"))?;
    let h = o.downcast_ref::<Headers>().ok_or_else(|| JsNativeError::typ().with_message("not a Headers"))?;
    Ok(f(&h))
}

fn arg(a: &[JsValue], i: usize, cx: &mut Context) -> String {
    js_value_string(&a.get(i).cloned().unwrap_or_default(), cx)
}

fn string(s: &str) -> JsValue {
    JsString::from(s).into()
}

/// An iterator over `items` (an array's).
fn iterate(items: Vec<JsValue>, cx: &mut Context) -> JsResult<JsValue> {
    JsArray::from_iter(items, cx).values(cx)
}

fn pairs(this: &JsValue, cx: &mut Context) -> JsResult<Vec<JsValue>> {
    let list = this_headers(this, Headers::sorted)?;
    Ok(list.into_iter().map(|(n, v)| JsArray::from_iter([string(&n), string(&v)], cx).into()).collect())
}

type Method = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

const METHODS: &[(&str, usize, Method)] = &[
    ("append", 2, |this, a, cx| {
        let (n, v) = (arg(a, 0, cx), arg(a, 1, cx));
        this_headers(this, |h| h.append(&n, &v))??;
        Ok(JsValue::undefined())
    }),
    ("set", 2, |this, a, cx| {
        let (n, v) = (arg(a, 0, cx), arg(a, 1, cx));
        this_headers(this, |h| h.set(&n, &v))??;
        Ok(JsValue::undefined())
    }),
    ("get", 1, |this, a, cx| {
        let n = arg(a, 0, cx);
        Ok(this_headers(this, |h| h.get(&n))?.map_or(JsValue::null(), |v| string(&v)))
    }),
    ("has", 1, |this, a, cx| {
        let n = arg(a, 0, cx).to_ascii_lowercase();
        Ok(this_headers(this, |h| h.list.borrow().iter().any(|(k, _)| *k == n))?.into())
    }),
    ("delete", 1, |this, a, cx| {
        let n = arg(a, 0, cx).to_ascii_lowercase();
        this_headers(this, |h| h.list.borrow_mut().retain(|(k, _)| *k != n))?;
        Ok(JsValue::undefined())
    }),
    ("getSetCookie", 0, |this, _, cx| {
        let cookies: Vec<JsValue> = this_headers(this, |h| h.list.borrow().iter().filter(|(k, _)| k == "set-cookie").map(|(_, v)| string(v)).collect())?;
        Ok(JsArray::from_iter(cookies, cx).into())
    }),
    ("forEach", 1, |this, a, cx| {
        let f = a.first().and_then(|f| f.as_callable()).ok_or_else(|| JsNativeError::typ().with_message("Headers.forEach needs a function"))?;
        let this_arg = a.get(1).cloned().unwrap_or_default();
        for (n, v) in this_headers(this, Headers::sorted)? {
            f.call(&this_arg, &[string(&v), string(&n), this.clone()], cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("keys", 0, |this, _, cx| {
        let keys = this_headers(this, Headers::sorted)?.into_iter().map(|(n, _)| string(&n)).collect();
        iterate(keys, cx)
    }),
    ("values", 0, |this, _, cx| {
        let values = this_headers(this, Headers::sorted)?.into_iter().map(|(_, v)| string(&v)).collect();
        iterate(values, cx)
    }),
    ("entries", 0, |this, _, cx| {
        let p = pairs(this, cx)?;
        iterate(p, cx)
    }),
    // For the fetch code: the pairs as stored (a list of [name, value]).
    ("_list", 0, |this, _, cx| {
        let list = this_headers(this, |h| h.list.borrow().clone())?;
        let list: Vec<JsValue> = list.into_iter().map(|(n, v)| JsArray::from_iter([string(&n), string(&v)], cx).into()).collect();
        Ok(JsArray::from_iter(list, cx).into())
    }),
];

impl Class for Headers {
    const NAME: &'static str = "Headers";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        for &(name, len, f) in METHODS {
            class.method(JsString::from(name), len, NativeFunction::from_fn_ptr(f));
        }
        let entries = NativeFunction::from_fn_ptr(|this, _, cx| {
            let p = pairs(this, cx)?;
            iterate(p, cx)
        });
        let realm = class.context().realm().clone();
        let entries = boa_engine::object::FunctionObjectBuilder::new(&realm, entries).name(js_string!("entries")).build();
        class.property(JsSymbol::iterator(), entries, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let headers = Headers::default();
        fill(&headers, &a.first().cloned().unwrap_or_default(), cx)?;
        Ok(headers)
    }
}

/// Register `Headers`.
pub(crate) fn install(cx: &mut Context) {
    if let Err(e) = cx.register_global_class::<Headers>() {
        eprintln!("js: headers: {e}");
    }
}
