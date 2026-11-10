//! The structured clone algorithm (minus transfer), native: a value made
//! into [`Cloned`], plain data that can go to another thread (a worker's
//! messages), and made again in a context. `structuredClone` is both
//! steps. Cycles and shared references come out as they went in;
//! functions and symbols can't be cloned.

use std::collections::HashMap;
use std::sync::Arc;

use boa_engine::object::builtins::{JsArray, JsArrayBuffer, JsDataView, JsDate, JsMap, JsRegExp, JsSet, JsTypedArray};
use boa_engine::{Context, JsError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};

use crate::dom_exception;

/// A value as the structured clone algorithm serializes it.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    BigInt(String),
    /// An object: `Cloned::objects[i]`.
    Object(usize),
}

/// An object the clone holds (others refer to it by index).
#[derive(Clone, Debug, PartialEq)]
pub enum Object {
    Plain(Vec<(String, Value)>),
    Array { length: u64, items: Vec<(u64, Value)> },
    Date(f64),
    RegExp { source: String, flags: String },
    Map(Vec<(Value, Value)>),
    Set(Vec<Value>),
    Buffer(Vec<u8>),
    /// A typed array (by constructor name) or `DataView` over a buffer.
    View { class: String, buffer: usize, offset: u64, length: u64 },
    Blob { bytes: Arc<[u8]>, kind: String },
    Error { name: String, message: String },
}

/// A cloned value, ready to be made again anywhere.
#[derive(Clone, Debug, PartialEq)]
pub struct Cloned {
    pub root: Value,
    pub objects: Vec<Object>,
}

fn uncloneable(what: &str, cx: &mut Context) -> JsError {
    dom_exception::error("DataCloneError", &format!("{what} could not be cloned"), cx)
}

/// Serialize `v`.
pub(crate) fn serialize(v: &JsValue, cx: &mut Context) -> JsResult<Cloned> {
    let mut s = Serializer::default();
    let root = s.value(v, cx)?;
    Ok(Cloned { root, objects: s.objects.into_iter().map(|o| o.expect("filled")).collect() })
}

#[derive(Default)]
struct Serializer {
    seen: HashMap<JsObject, usize>,
    objects: Vec<Option<Object>>,
}

impl Serializer {
    fn value(&mut self, v: &JsValue, cx: &mut Context) -> JsResult<Value> {
        let Some(o) = v.as_object() else {
            return Ok(if v.is_undefined() {
                Value::Undefined
            } else if v.is_null() {
                Value::Null
            } else if let Some(b) = v.as_boolean() {
                Value::Bool(b)
            } else if let Some(n) = v.as_number() {
                Value::Number(n)
            } else if let Some(s) = v.as_string() {
                Value::String(s.to_std_string_escaped())
            } else if let Some(b) = v.as_bigint() {
                Value::BigInt(b.to_string())
            } else {
                return Err(uncloneable("a symbol", cx));
            });
        };
        if let Some(&i) = self.seen.get(&o) {
            return Ok(Value::Object(i));
        }
        if o.is_callable() {
            return Err(uncloneable("a function", cx));
        }
        // Its index first: what it holds may refer back to it.
        let i = self.objects.len();
        self.objects.push(None);
        self.seen.insert(o.clone(), i);
        let object = self.object(&o, cx)?;
        self.objects[i] = Some(object);
        Ok(Value::Object(i))
    }

    fn object(&mut self, o: &JsObject, cx: &mut Context) -> JsResult<Object> {
        if let Some((bytes, kind)) = crate::blob::blob_of(o) {
            return Ok(Object::Blob { bytes, kind });
        }
        if let Ok(date) = JsDate::from_object(o.clone()) {
            return Ok(Object::Date(date.get_time(cx)?.to_number(cx)?));
        }
        if let Ok(re) = JsRegExp::from_object(o.clone()) {
            return Ok(Object::RegExp { source: re.source(cx)?, flags: re.flags(cx)? });
        }
        if let Ok(buffer) = JsArrayBuffer::from_object(o.clone()) {
            return Ok(Object::Buffer(buffer.to_vec().unwrap_or_default()));
        }
        if let Ok(view) = JsTypedArray::from_object(o.clone()) {
            let class = o.get(js_string!("constructor"), cx)?;
            let class = class.as_object().map(|c| c.get(js_string!("name"), cx)).transpose()?.map(|n| n.to_string(cx)).transpose()?;
            let class = class.map(|c| c.to_std_string_escaped()).ok_or_else(|| uncloneable("a typed array", cx))?;
            let Value::Object(buffer) = self.value(&view.buffer(cx)?, cx)? else { return Err(uncloneable("a typed array", cx)) };
            return Ok(Object::View { class, buffer, offset: view.byte_offset(cx)? as u64, length: view.length(cx)? as u64 });
        }
        if let Ok(view) = JsDataView::from_object(o.clone()) {
            let Value::Object(buffer) = self.value(&view.buffer(cx)?, cx)? else { return Err(uncloneable("a DataView", cx)) };
            return Ok(Object::View { class: "DataView".into(), buffer, offset: view.byte_offset(cx)?, length: view.byte_length(cx)? });
        }
        if let Ok(map) = JsMap::from_object(o.clone()) {
            let mut entries = Vec::new();
            map.for_each_native(|k, v| {
                entries.push((k, v));
                Ok(())
            })?;
            let mut out = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                out.push((self.value(&k, cx)?, self.value(&v, cx)?));
            }
            return Ok(Object::Map(out));
        }
        if let Ok(set) = JsSet::from_object(o.clone()) {
            let mut items = Vec::new();
            set.for_each_native(|v| {
                items.push(v);
                Ok(())
            })?;
            let mut out = Vec::with_capacity(items.len());
            for v in items {
                out.push(self.value(&v, cx)?);
            }
            return Ok(Object::Set(out));
        }
        if o.is_array() {
            let length = o.get(js_string!("length"), cx)?.to_length(cx)?;
            let mut items = Vec::new();
            for i in 0..length {
                if o.has_own_property(i, cx)? {
                    items.push((i, self.value(&o.get(i, cx)?, cx)?));
                }
            }
            return Ok(Object::Array { length, items });
        }
        if o.is::<boa_engine::builtins::error::Error>() {
            let text = |key: JsString, cx: &mut Context| -> JsResult<String> {
                let v = o.get(key, cx)?;
                Ok(if v.is_undefined() { String::new() } else { v.to_string(cx)?.to_std_string_escaped() })
            };
            return Ok(Object::Error { name: text(js_string!("name"), cx)?, message: text(js_string!("message"), cx)? });
        }
        // Anything else: its own enumerable string-keyed properties.
        let object = cx.intrinsics().constructors().object().constructor();
        let keys = object.get(js_string!("keys"), cx)?;
        let keys = keys.as_callable().expect("Object.keys").call(&object.into(), &[o.clone().into()], cx)?;
        let keys = JsArray::from_object(keys.as_object().expect("an array"))?;
        let mut out = Vec::new();
        for i in 0..keys.length(cx)? {
            let key = keys.get(i, cx)?.to_string(cx)?;
            let v = o.get(key.clone(), cx)?;
            out.push((key.to_std_string_escaped(), self.value(&v, cx)?));
        }
        Ok(Object::Plain(out))
    }
}

/// Make `c` in `cx`.
pub(crate) fn deserialize(c: &Cloned, cx: &mut Context) -> JsResult<JsValue> {
    let mut made: Vec<Option<JsValue>> = vec![None; c.objects.len()];
    value(c, &c.root, &mut made, cx)
}

fn value(c: &Cloned, v: &Value, made: &mut Vec<Option<JsValue>>, cx: &mut Context) -> JsResult<JsValue> {
    Ok(match v {
        Value::Undefined => JsValue::undefined(),
        Value::Null => JsValue::null(),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => (*n).into(),
        Value::String(s) => JsString::from(s.as_str()).into(),
        Value::BigInt(b) => {
            let big = cx.global_object().get(js_string!("BigInt"), cx)?;
            big.as_callable().expect("BigInt").call(&JsValue::undefined(), &[JsString::from(b.as_str()).into()], cx)?
        }
        Value::Object(i) => object(c, *i, made, cx)?,
    })
}

fn construct(name: &str, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let class = cx.global_object().get(JsString::from(name), cx)?;
    let class = class.as_constructor().ok_or_else(|| uncloneable(name, cx))?;
    Ok(class.construct(args, None, cx)?.into())
}

fn object(c: &Cloned, i: usize, made: &mut Vec<Option<JsValue>>, cx: &mut Context) -> JsResult<JsValue> {
    if let Some(v) = &made[i] {
        return Ok(v.clone());
    }
    // Containers exist before what's in them (which may be them).
    let out: JsValue = match &c.objects[i] {
        Object::Plain(_) => JsObject::with_object_proto(cx.intrinsics()).into(),
        Object::Array { .. } => JsArray::new(cx)?.into(),
        Object::Map(_) => JsMap::new(cx).into(),
        Object::Set(_) => JsSet::new(cx).into(),
        Object::Date(t) => cx.intrinsics().constructors().date().constructor().construct(&[(*t).into()], None, cx)?.into(),
        Object::RegExp { source, flags } => JsRegExp::new(JsString::from(source.as_str()), JsString::from(flags.as_str()), cx)?.into(),
        Object::Buffer(bytes) => crate::blob::array_buffer(bytes, cx)?,
        Object::View { class, buffer, offset, length } => {
            let buffer = object(c, *buffer, made, cx)?;
            construct(class, &[buffer, (*offset as f64).into(), (*length as f64).into()], cx)?
        }
        Object::Blob { bytes, kind } => crate::blob::new_blob(bytes.clone(), kind.clone(), cx),
        Object::Error { name, message } => {
            let class = match name.as_str() {
                "EvalError" | "RangeError" | "ReferenceError" | "SyntaxError" | "TypeError" | "URIError" => name.as_str(),
                _ => "Error",
            };
            construct(class, &[JsString::from(message.as_str()).into()], cx)?
        }
    };
    made[i] = Some(out.clone());
    let o = out.as_object().expect("an object");
    match &c.objects[i] {
        Object::Plain(entries) => {
            for (k, v) in entries {
                let v = value(c, v, made, cx)?;
                o.set(JsString::from(k.as_str()), v, true, cx)?;
            }
        }
        Object::Array { length, items } => {
            for (index, v) in items {
                let v = value(c, v, made, cx)?;
                o.set(*index, v, true, cx)?;
            }
            o.set(js_string!("length"), *length as f64, true, cx)?;
        }
        Object::Map(entries) => {
            let map = JsMap::from_object(o.clone())?;
            for (k, v) in entries {
                let (k, v) = (value(c, k, made, cx)?, value(c, v, made, cx)?);
                map.set(k, v, cx)?;
            }
        }
        Object::Set(items) => {
            let set = JsSet::from_object(o.clone()).map_err(|_| uncloneable("a set", cx))?;
            for v in items {
                let v = value(c, v, made, cx)?;
                set.add(v, cx)?;
            }
        }
        _ => {}
    }
    Ok(out)
}

/// A deep copy of `v`.
pub(crate) fn clone(v: &JsValue, cx: &mut Context) -> JsResult<JsValue> {
    let c = serialize(v, cx)?;
    deserialize(&c, cx)
}

/// Register `structuredClone`.
pub(crate) fn install(cx: &mut Context) {
    let f = NativeFunction::from_fn_ptr(|_, a, cx| clone(&a.first().cloned().unwrap_or_default(), cx));
    if let Err(e) = cx.register_global_builtin_callable(js_string!("structuredClone"), 1, f) {
        eprintln!("js: structuredClone: {e}");
    }
}
