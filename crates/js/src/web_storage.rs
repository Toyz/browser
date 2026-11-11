//! `localStorage` and `sessionStorage`, native: proxies (so
//! `localStorage.key = value`, `in`, `delete` and `Object.keys` work) over
//! the areas `storage.rs` keeps (on disk per origin, or per tab).

use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::{JsArray, JsProxyBuilder};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, Trace};

use crate::dom::{Shared, js_value_string};
use crate::storage::{Kind, with};

/// One storage area of the page: which, and the page (for its origin).
#[derive(Clone, Trace, Finalize, JsData)]
struct Area {
    #[unsafe_ignore_trace]
    dom: Shared,
    #[unsafe_ignore_trace]
    session: bool,
}

impl Area {
    /// The area's kind and origin; `None` for opaque origins (`data:`),
    /// which get nothing that lasts.
    fn place(&self) -> Option<(Kind, String)> {
        let dom = self.dom.borrow();
        let kind = if self.session { Kind::Session(crate::dom::session_of(&dom)) } else { Kind::Local };
        let url = crate::dom::url_of(&dom);
        let origin = match url.origin() {
            o if o.is_tuple() => o.ascii_serialization(),
            _ if url.scheme() == "file" => "file://".to_owned(),
            _ => return None,
        };
        Some((kind, origin))
    }

    fn get(&self, key: &str) -> Option<String> {
        let (kind, origin) = self.place()?;
        with(kind, &origin, |area| (area.get(key).cloned(), false))
    }

    fn set(&self, key: String, value: String) {
        if let Some((kind, origin)) = self.place() {
            with(kind, &origin, |area| ((), area.insert(key, value.clone()).as_ref() != Some(&value)));
        }
    }

    fn remove(&self, key: &str) {
        if let Some((kind, origin)) = self.place() {
            with(kind, &origin, |area| ((), area.remove(key).is_some()));
        }
    }

    fn clear(&self) {
        if let Some((kind, origin)) = self.place() {
            with(kind, &origin, |area| {
                let had = !area.is_empty();
                area.clear();
                ((), had)
            });
        }
    }

    fn keys(&self) -> Vec<String> {
        self.place().map(|(kind, origin)| with(kind, &origin, |area| (area.keys().cloned().collect(), false))).unwrap_or_default()
    }
}

/// The area a trap's target (argument 0), or a method's captures, is.
fn area_of(v: &JsValue) -> Option<Area> {
    v.as_object().and_then(|o| o.downcast_ref::<Area>().map(|a| (*a).clone()))
}

fn arg(a: &[JsValue], i: usize, cx: &mut Context) -> String {
    js_value_string(&a.get(i).cloned().unwrap_or_default(), cx)
}

/// Install `localStorage` and `sessionStorage` on the natives object.
pub(crate) fn install(natives: &JsObject, dom: &Shared, cx: &mut Context) {
    for (name, session) in [("localStorage", false), ("sessionStorage", true)] {
        if let Ok(storage) = storage(Area { dom: dom.clone(), session }, cx) {
            natives.set(JsString::from(name), storage, false, cx).ok();
        }
    }
}

fn storage(area: Area, cx: &mut Context) -> JsResult<JsValue> {
    let method = |body: fn(&JsValue, &[JsValue], &Area, &mut Context) -> JsResult<JsValue>| NativeFunction::from_copy_closure_with_captures(body, area.clone());
    let get_item = method(|_, a, s, cx| Ok(s.get(&arg(a, 0, cx)).map_or(JsValue::null(), |v| JsValue::from(JsString::from(v)))));
    let set_item = method(|_, a, s, cx| {
        let (k, v) = (arg(a, 0, cx), arg(a, 1, cx));
        s.set(k, v);
        Ok(JsValue::undefined())
    });
    let remove_item = method(|_, a, s, cx| {
        s.remove(&arg(a, 0, cx));
        Ok(JsValue::undefined())
    });
    let clear = method(|_, _, s, _| {
        s.clear();
        Ok(JsValue::undefined())
    });
    let key = method(|_, a, s, cx| {
        let i = a.first().map(|v| v.to_number(cx)).transpose()?.unwrap_or(0.0);
        Ok(s.keys().get(i as usize).filter(|_| i >= 0.0).map_or(JsValue::null(), |k| JsValue::from(JsString::from(k.as_str()))))
    });
    let length = method(|_, _, s, _| Ok(JsValue::from(s.keys().len() as u32)));
    let length = boa_engine::object::FunctionObjectBuilder::new(cx.realm(), length).name(js_string!("length")).build();
    let target = ObjectInitializer::with_native_data(area, cx)
        .function(get_item, js_string!("getItem"), 1)
        .function(set_item, js_string!("setItem"), 2)
        .function(remove_item, js_string!("removeItem"), 1)
        .function(clear, js_string!("clear"), 0)
        .function(key, js_string!("key"), 1)
        .accessor(js_string!("length"), Some(length), None, Attribute::CONFIGURABLE)
        .build();
    // (A proxy's prototype is its target's: `localStorage instanceof Storage`.)
    target.set_prototype(Some(crate::interfaces::interface(cx, "Storage")));
    let proxy = JsProxyBuilder::new(target)
        .get(trap_get)
        .set(trap_set)
        .has(trap_has)
        .delete_property(trap_delete)
        .own_keys(trap_own_keys)
        .get_own_property_descriptor(trap_descriptor)
        .build(cx)?;
    Ok(proxy.into())
}

/// The key a trap was asked about, as a string (`None` for symbols).
fn string_key(a: &[JsValue]) -> Option<String> {
    a.get(1).filter(|k| k.is_string()).and_then(|k| k.as_string()).map(|k| k.to_std_string_escaped())
}

/// `storage.x`: its methods and `length`, else the stored value.
fn trap_get(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let target = a.first().and_then(|t| t.as_object()).expect("a target");
    let key = a.get(1).cloned().unwrap_or_default();
    let own = target.has_property(key.to_property_key(cx)?, cx)?;
    match string_key(a).filter(|_| !own) {
        Some(k) => Ok(area_of(&a[0]).and_then(|s| s.get(&k)).map_or(JsValue::undefined(), |v| JsValue::from(JsString::from(v)))),
        None => target.get(key.to_property_key(cx)?, cx),
    }
}

/// `storage.x = value`: stored.
fn trap_set(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    if let (Some(s), Some(k)) = (area_of(&a[0]), string_key(a)) {
        let value = arg(a, 2, cx);
        s.set(k, value);
    }
    Ok(JsValue::from(true))
}

fn trap_has(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let target = a.first().and_then(|t| t.as_object()).expect("a target");
    let key = a.get(1).cloned().unwrap_or_default();
    if target.has_property(key.to_property_key(cx)?, cx)? {
        return Ok(JsValue::from(true));
    }
    Ok(JsValue::from(string_key(a).zip(area_of(&a[0])).is_some_and(|(k, s)| s.get(&k).is_some())))
}

fn trap_delete(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    if let (Some(s), Some(k)) = (area_of(&a[0]), string_key(a)) {
        s.remove(&k);
    }
    Ok(JsValue::from(true))
}

/// `Object.keys(storage)`: the stored keys.
fn trap_own_keys(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let keys = area_of(&a[0]).map(|s| s.keys()).unwrap_or_default();
    Ok(JsArray::from_iter(keys.into_iter().map(|k| JsValue::from(JsString::from(k))), cx).into())
}

fn trap_descriptor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let Some(value) = string_key(a).zip(area_of(&a[0])).and_then(|(k, s)| s.get(&k)) else { return Ok(JsValue::undefined()) };
    Ok(ObjectInitializer::new(cx)
        .property(js_string!("value"), JsString::from(value), Attribute::all())
        .property(js_string!("enumerable"), true, Attribute::all())
        .property(js_string!("configurable"), true, Attribute::all())
        .property(js_string!("writable"), true, Attribute::all())
        .build()
        .into())
}
