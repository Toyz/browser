//! `FormData`, native: (name, value) entries in order, a value a string or
//! a `File` (a Blob appended becomes one, named `blob` unless given a
//! name), filled from a `<form>`'s controls when constructed with one.

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsSymbol, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};

use crate::dom::js_value_string;

#[derive(Trace, Finalize, Clone)]
pub(crate) enum Value {
    Text(String),
    File(JsObject),
}

impl Value {
    fn to_js(&self) -> JsValue {
        match self {
            Value::Text(s) => JsString::from(s.as_str()).into(),
            Value::File(o) => o.clone().into(),
        }
    }
}

#[derive(Trace, Finalize, JsData, Default)]
pub(crate) struct FormData {
    entries: GcRefCell<Vec<(String, Value)>>,
}

/// The entries of a `FormData` (for request bodies), `None` for anything else.
pub(crate) fn entries_of(o: &JsObject) -> Option<Vec<(String, Value)>> {
    o.downcast_ref::<FormData>().map(|f| f.entries.borrow().clone())
}

/// A blob or file as a value: a `File` (a blob wrapped in one, named
/// `filename` or `blob`; a file renamed when given a name).
fn value_of(v: &JsValue, filename: Option<&JsValue>, cx: &mut Context) -> JsResult<Value> {
    let Some(o) = v.as_object().filter(|o| crate::blob::blob_of(o).is_some()) else {
        return Ok(Value::Text(js_value_string(v, cx)));
    };
    let is_file = crate::blob::is_file(&o);
    let filename = filename.filter(|f| !f.is_undefined());
    if is_file && filename.is_none() {
        return Ok(Value::File(o));
    }
    let name = match filename {
        Some(f) => js_value_string(f, cx),
        None => "blob".to_owned(),
    };
    let file = cx.global_object().get(js_string!("File"), cx)?;
    let file = file.as_constructor().ok_or_else(|| JsNativeError::typ().with_message("no File"))?;
    let kind = o.get(js_string!("type"), cx)?;
    let options = JsObject::with_object_proto(cx.intrinsics());
    options.set(js_string!("type"), kind, false, cx)?;
    let parts = JsArray::from_iter([o.into()], cx);
    let made = file.construct(&[parts.into(), JsString::from(name).into(), options.into()], None, cx)?;
    Ok(Value::File(made))
}

fn this_form<R>(this: &JsValue, f: impl FnOnce(&FormData) -> R) -> JsResult<R> {
    let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a FormData"))?;
    let d = o.downcast_ref::<FormData>().ok_or_else(|| JsNativeError::typ().with_message("not a FormData"))?;
    Ok(f(&d))
}

fn name_arg(a: &[JsValue], cx: &mut Context) -> String {
    js_value_string(&a.first().cloned().unwrap_or_default(), cx)
}

/// An iterator over `items`.
fn iterate(items: Vec<JsValue>, cx: &mut Context) -> JsResult<JsValue> {
    JsArray::from_iter(items, cx).values(cx)
}

fn pairs(this: &JsValue, cx: &mut Context) -> JsResult<Vec<JsValue>> {
    let list = this_form(this, |f| f.entries.borrow().clone())?;
    Ok(list.into_iter().map(|(n, v)| JsArray::from_iter([JsString::from(n).into(), v.to_js()], cx).into()).collect())
}

type Method = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

const METHODS: &[(&str, usize, Method)] = &[
    ("append", 2, |this, a, cx| {
        let name = name_arg(a, cx);
        let value = value_of(&a.get(1).cloned().unwrap_or_default(), a.get(2), cx)?;
        this_form(this, |f| f.entries.borrow_mut().push((name, value)))?;
        Ok(JsValue::undefined())
    }),
    ("set", 2, |this, a, cx| {
        let name = name_arg(a, cx);
        let value = value_of(&a.get(1).cloned().unwrap_or_default(), a.get(2), cx)?;
        this_form(this, |f| {
            let mut list = f.entries.borrow_mut();
            // The first keeps its place; the rest go.
            match list.iter().position(|(n, _)| *n == name) {
                Some(i) => {
                    list[i].1 = value;
                    let mut seen = false;
                    list.retain(|(n, _)| *n != name || !std::mem::replace(&mut seen, true));
                }
                None => list.push((name, value)),
            }
        })?;
        Ok(JsValue::undefined())
    }),
    ("get", 1, |this, a, cx| {
        let name = name_arg(a, cx);
        Ok(this_form(this, |f| f.entries.borrow().iter().find(|(n, _)| *n == name).map(|(_, v)| v.to_js()))?.unwrap_or(JsValue::null()))
    }),
    ("getAll", 1, |this, a, cx| {
        let name = name_arg(a, cx);
        let all: Vec<JsValue> = this_form(this, |f| f.entries.borrow().iter().filter(|(n, _)| *n == name).map(|(_, v)| v.to_js()).collect())?;
        Ok(JsArray::from_iter(all, cx).into())
    }),
    ("has", 1, |this, a, cx| {
        let name = name_arg(a, cx);
        Ok(this_form(this, |f| f.entries.borrow().iter().any(|(n, _)| *n == name))?.into())
    }),
    ("delete", 1, |this, a, cx| {
        let name = name_arg(a, cx);
        this_form(this, |f| f.entries.borrow_mut().retain(|(n, _)| *n != name))?;
        Ok(JsValue::undefined())
    }),
    ("forEach", 1, |this, a, cx| {
        let f = a.first().and_then(|f| f.as_callable()).ok_or_else(|| JsNativeError::typ().with_message("FormData.forEach needs a function"))?;
        let this_arg = a.get(1).cloned().unwrap_or_default();
        for (n, v) in this_form(this, |f| f.entries.borrow().clone())? {
            f.call(&this_arg, &[v.to_js(), JsString::from(n).into(), this.clone()], cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("keys", 0, |this, _, cx| {
        let keys = this_form(this, |f| f.entries.borrow().iter().map(|(n, _)| JsString::from(n.as_str()).into()).collect())?;
        iterate(keys, cx)
    }),
    ("values", 0, |this, _, cx| {
        let values = this_form(this, |f| f.entries.borrow().iter().map(|(_, v)| v.to_js()).collect())?;
        iterate(values, cx)
    }),
    ("entries", 0, |this, _, cx| {
        let p = pairs(this, cx)?;
        iterate(p, cx)
    }),
];

/// What a form submits: its named, enabled controls' values (checked
/// boxes only; no buttons or files).
fn fill_from_form(data: &FormData, form: &JsObject, cx: &mut Context) -> JsResult<()> {
    let query = form.get(js_string!("querySelectorAll"), cx)?;
    let Some(query) = query.as_callable() else { return Ok(()) };
    let controls = query.call(&form.clone().into(), &[js_string!("input, select, textarea").into()], cx)?;
    let Some(controls) = controls.as_object() else { return Ok(()) };
    let length = controls.get(js_string!("length"), cx)?.to_length(cx)?;
    let mut list = Vec::new();
    for i in 0..length {
        let Some(el) = controls.get(i, cx)?.as_object() else { continue };
        let attr = |el: &JsObject, name: &str, cx: &mut Context| -> JsResult<Option<String>> {
            let get = el.get(js_string!("getAttribute"), cx)?;
            let v = get.as_callable().map(|g| g.call(&el.clone().into(), &[JsString::from(name).into()], cx)).transpose()?;
            Ok(v.filter(|v| !v.is_null_or_undefined()).map(|v| js_value_string(&v, cx)))
        };
        let Some(name) = attr(&el, "name", cx)?.filter(|n| !n.is_empty()) else { continue };
        if attr(&el, "disabled", cx)?.is_some() {
            continue;
        }
        let kind = attr(&el, "type", cx)?.unwrap_or_default().to_ascii_lowercase();
        if matches!(kind.as_str(), "submit" | "button" | "reset" | "file" | "image") {
            continue;
        }
        let checkable = matches!(kind.as_str(), "checkbox" | "radio");
        if checkable && !el.get(js_string!("checked"), cx)?.to_boolean() {
            continue;
        }
        let value = if kind == "checkbox" && attr(&el, "value", cx)?.is_none() {
            "on".to_owned()
        } else {
            js_value_string(&el.get(js_string!("value"), cx)?, cx)
        };
        list.push((name, Value::Text(value)));
    }
    data.entries.borrow_mut().extend(list);
    Ok(())
}

impl Class for FormData {
    const NAME: &'static str = "FormData";

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
        let data = FormData::default();
        if let Some(form) = a.first().and_then(JsValue::as_object)
            && !form.get(js_string!("__id"), cx)?.is_undefined()
        {
            fill_from_form(&data, &form, cx)?;
        }
        Ok(data)
    }
}

/// Register `FormData`.
pub(crate) fn install(cx: &mut Context) {
    if let Err(e) = cx.register_global_class::<FormData>() {
        eprintln!("js: FormData: {e}");
    }
}
