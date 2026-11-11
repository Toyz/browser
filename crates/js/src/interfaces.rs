//! Web IDL class strings: each interface's prototype carries its name as
//! `Symbol.toStringTag`, so `Object.prototype.toString.call(document)` is
//! `"[object HTMLDocument]"` (and `"[object Window]"`, `"[object Navigator]"`).

use std::collections::HashSet;
use std::sync::OnceLock;

use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::{Attribute, PropertyDescriptor, PropertyKey};
use boa_engine::{Context, JsNativeError, JsObject, JsString, JsSymbol, JsValue, NativeFunction, js_string};

/// The language's own globals (they have their own tags, or none).
fn language_globals() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut cx = Context::default();
        let global = cx.global_object();
        global
            .own_property_keys(&mut cx)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|k| match k {
                PropertyKey::String(s) => Some(s.to_std_string_escaped()),
                _ => None,
            })
            .collect()
    })
}

/// Interfaces of the window's own objects (made natively, without them):
/// a constructor pages can't call and a prototype each object inherits.
const SINGLETONS: &[(&str, &str)] = &[
    ("navigator", "Navigator"),
    ("location", "Location"),
    ("history", "History"),
    ("screen", "Screen"),
    ("performance", "Performance"),
    ("crypto", "Crypto"),
];

/// The prototype of interface `name`, made (constructor and all) if new.
pub(crate) fn interface(cx: &mut Context, name: &str) -> JsObject {
    let global = cx.global_object();
    if let Some(existing) = global.get(JsString::from(name), cx).ok().and_then(|v| v.as_object())
        && let Some(proto) = existing.get(js_string!("prototype"), cx).ok().and_then(|v| v.as_object())
    {
        return proto;
    }
    let realm = cx.realm().clone();
    let ctor = FunctionObjectBuilder::new(
        &realm,
        NativeFunction::from_fn_ptr(|_, _, _| Err(JsNativeError::typ().with_message("Illegal constructor").into())),
    )
    .name(JsString::from(name))
    .length(0)
    .constructor(true)
    .build();
    let proto = JsObject::with_object_proto(cx.intrinsics());
    let hidden = Attribute::WRITABLE | Attribute::CONFIGURABLE;
    proto.define_property_or_throw(js_string!("constructor"), PropertyDescriptor::builder().value(ctor.clone()).writable(true).enumerable(false).configurable(true), cx).ok();
    ctor.define_property_or_throw(js_string!("prototype"), PropertyDescriptor::builder().value(proto.clone()).writable(false).enumerable(false).configurable(false), cx).ok();
    global.define_property_or_throw(JsString::from(name), PropertyDescriptor::builder().value(ctor).writable(hidden.writable()).enumerable(false).configurable(true), cx).ok();
    proto
}

/// `navigator.plugins` and `navigator.mimeTypes`: empty (no PDF viewer).
fn plugins(cx: &mut Context) {
    let Some(navigator) = cx.global_object().get(js_string!("navigator"), cx).ok().and_then(|v| v.as_object()) else { return };
    if navigator.has_property(js_string!("plugins"), cx).unwrap_or(true) {
        return;
    }
    for (prop, iface) in [("plugins", "PluginArray"), ("mimeTypes", "MimeTypeArray")] {
        let proto = interface(cx, iface);
        let none = NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::null()));
        let list = ObjectInitializer::new(cx)
            .property(js_string!("length"), 0, Attribute::READONLY | Attribute::NON_ENUMERABLE)
            .function(none.clone(), js_string!("item"), 1)
            .function(none, js_string!("namedItem"), 1)
            .build();
        if prop == "plugins" {
            let refresh = NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined()));
            let realm = cx.realm().clone();
            let refresh = FunctionObjectBuilder::new(&realm, refresh).name(js_string!("refresh")).length(0).build();
            list.set(js_string!("refresh"), refresh, false, cx).ok();
        }
        list.set_prototype(Some(proto));
        navigator.define_property_or_throw(JsString::from(prop), PropertyDescriptor::builder().value(list).writable(false).enumerable(true).configurable(true), cx).ok();
    }
    navigator
        .define_property_or_throw(js_string!("pdfViewerEnabled"), PropertyDescriptor::builder().value(false).writable(false).enumerable(true).configurable(true), cx)
        .ok();
}

/// Tag every interface on the global object (once its constructors exist).
pub(crate) fn tag(cx: &mut Context) {
    for (object, iface) in SINGLETONS {
        let Some(object) = cx.global_object().get(JsString::from(*object), cx).ok().and_then(|v| v.as_object()) else { continue };
        let proto = interface(cx, iface);
        // (Whatever it inherited before comes after the interface.)
        if let Some(old) = object.prototype()
            && old != proto
            && proto.prototype().is_some_and(|p| p != old)
            && !old.is_callable()
        {
            proto.set_prototype(Some(old));
        }
        object.set_prototype(Some(proto));
    }
    plugins(cx);
    let global = cx.global_object();
    let tag_key = PropertyKey::Symbol(JsSymbol::to_string_tag());
    let skip = language_globals();
    for key in global.own_property_keys(cx).unwrap_or_default() {
        let PropertyKey::String(name) = &key else { continue };
        let name_str = name.to_std_string_escaped();
        if skip.contains(&name_str) || !name_str.starts_with(|c: char| c.is_ascii_uppercase()) {
            continue;
        }
        let Some(ctor) = global.get(key.clone(), cx).ok().and_then(|v| v.as_object()).filter(|o| o.is_callable()) else { continue };
        let Some(proto) = ctor.get(js_string!("prototype"), cx).ok().and_then(|v| v.as_object()) else { continue };
        // Its own prototype (not a legacy factory's, like `Image`'s).
        let own = proto.get(js_string!("constructor"), cx).ok().and_then(|v| v.as_object()).is_some_and(|c| c == ctor);
        if !own || proto.has_own_property(tag_key.clone(), cx).unwrap_or(true) {
            continue;
        }
        let desc = PropertyDescriptor::builder().value(JsValue::from(name.clone())).writable(false).enumerable(false).configurable(true);
        proto.define_property_or_throw(tag_key.clone(), desc, cx).ok();
    }
}
