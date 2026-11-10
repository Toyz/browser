//! `structuredClone`, native: deep copies by the structured clone algorithm
//! (minus transfer). Cycles and shared references come out as they went in;
//! functions and symbols can't be cloned.

use std::collections::HashMap;

use boa_engine::object::builtins::{JsArray, JsArrayBuffer, JsDataView, JsDate, JsMap, JsRegExp, JsSet, JsTypedArray};
use boa_engine::{Context, JsError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};

use crate::dom_exception;

/// A deep copy of `v`.
pub(crate) fn clone(v: &JsValue, cx: &mut Context) -> JsResult<JsValue> {
    Cloner::default().value(v, cx)
}

fn uncloneable(what: &str, cx: &mut Context) -> JsError {
    dom_exception::error("DataCloneError", &format!("{what} could not be cloned"), cx)
}

#[derive(Default)]
struct Cloner {
    /// Objects copied so far, and their copies.
    seen: HashMap<JsObject, JsValue>,
}

impl Cloner {
    fn value(&mut self, v: &JsValue, cx: &mut Context) -> JsResult<JsValue> {
        let Some(o) = v.as_object() else {
            if v.is_symbol() {
                return Err(uncloneable("a symbol", cx));
            }
            return Ok(v.clone());
        };
        if let Some(copy) = self.seen.get(&o) {
            return Ok(copy.clone());
        }
        if o.is_callable() {
            return Err(uncloneable("a function", cx));
        }
        // Blobs are immutable: the same one does.
        if crate::blob::blob_of(&o).is_some() {
            return Ok(v.clone());
        }
        if let Ok(date) = JsDate::from_object(o.clone()) {
            let time = date.get_time(cx)?;
            let copy = cx.intrinsics().constructors().date().constructor().construct(&[time], None, cx)?;
            return Ok(self.done(o, copy.into()));
        }
        if let Ok(re) = JsRegExp::from_object(o.clone()) {
            let (source, flags) = (re.source(cx)?, re.flags(cx)?);
            let copy = JsRegExp::new(JsString::from(source), JsString::from(flags), cx)?;
            return Ok(self.done(o, copy.into()));
        }
        if let Ok(buffer) = JsArrayBuffer::from_object(o.clone()) {
            let bytes = buffer.to_vec().unwrap_or_default();
            let copy = crate::blob::array_buffer(&bytes, cx)?;
            return Ok(self.done(o, copy));
        }
        if let Ok(view) = JsTypedArray::from_object(o.clone()) {
            let buffer = self.value(&view.buffer(cx)?, cx)?;
            let (offset, length) = (view.byte_offset(cx)?, view.length(cx)?);
            let class = o.get(js_string!("constructor"), cx)?;
            let class = class.as_constructor().ok_or_else(|| uncloneable("a typed array", cx))?;
            let copy = class.construct(&[buffer, offset.into(), length.into()], None, cx)?;
            return Ok(self.done(o, copy.into()));
        }
        if let Ok(view) = JsDataView::from_object(o.clone()) {
            let buffer = self.value(&view.buffer(cx)?, cx)?;
            let (offset, length) = (view.byte_offset(cx)?, view.byte_length(cx)?);
            let class = cx.intrinsics().constructors().data_view().constructor();
            let copy = class.construct(&[buffer, offset.into(), length.into()], None, cx)?;
            return Ok(self.done(o, copy.into()));
        }
        if let Ok(map) = JsMap::from_object(o.clone()) {
            let copy = JsMap::new(cx);
            self.seen.insert(o, copy.clone().into());
            let mut entries = Vec::new();
            map.for_each_native(|k, v| {
                entries.push((k, v));
                Ok(())
            })?;
            for (k, v) in entries {
                let (k, v) = (self.value(&k, cx)?, self.value(&v, cx)?);
                copy.set(k, v, cx)?;
            }
            return Ok(copy.into());
        }
        if let Ok(set) = JsSet::from_object(o.clone()) {
            let copy = JsSet::new(cx);
            self.seen.insert(o, copy.clone().into());
            let mut items = Vec::new();
            set.for_each_native(|v| {
                items.push(v);
                Ok(())
            })?;
            for v in items {
                let v = self.value(&v, cx)?;
                copy.add(v, cx)?;
            }
            return Ok(copy.into());
        }
        if o.is_array() {
            let copy = JsArray::new(cx)?;
            self.seen.insert(o.clone(), copy.clone().into());
            let length = o.get(js_string!("length"), cx)?.to_length(cx)?;
            for i in 0..length {
                if o.has_own_property(i, cx)? {
                    let v = self.value(&o.get(i, cx)?, cx)?;
                    copy.set(i, v, true, cx)?;
                }
            }
            copy.set(js_string!("length"), length, true, cx)?;
            return Ok(copy.into());
        }
        // Anything else: its own enumerable string-keyed properties.
        let copy = JsObject::with_object_proto(cx.intrinsics());
        self.seen.insert(o.clone(), copy.clone().into());
        let object = cx.intrinsics().constructors().object().constructor();
        let keys = object.get(js_string!("keys"), cx)?;
        let keys = keys.as_callable().expect("Object.keys").call(&object.into(), &[o.clone().into()], cx)?;
        let keys = JsArray::from_object(keys.as_object().expect("an array"))?;
        for i in 0..keys.length(cx)? {
            let key = keys.get(i, cx)?.to_string(cx)?;
            let v = self.value(&o.get(key.clone(), cx)?, cx)?;
            copy.set(key, v, true, cx)?;
        }
        Ok(copy.into())
    }

    fn done(&mut self, original: JsObject, copy: JsValue) -> JsValue {
        self.seen.insert(original, copy.clone());
        copy
    }
}

/// Register `structuredClone`.
pub(crate) fn install(cx: &mut Context) {
    let f = NativeFunction::from_fn_ptr(|_, a, cx| clone(&a.first().cloned().unwrap_or_default(), cx));
    if let Err(e) = cx.register_global_builtin_callable(js_string!("structuredClone"), 1, f) {
        eprintln!("js: structuredClone: {e}");
    }
}
