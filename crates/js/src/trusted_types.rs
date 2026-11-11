//! Trusted Types (`window.trustedTypes`): policies that turn strings into
//! `TrustedHTML`, `TrustedScript` and `TrustedScriptURL` values, by the
//! page's own rules. Nothing is enforced (no `require-trusted-types-for`):
//! the values read as their text wherever a string goes, and `eval` runs a
//! `TrustedScript` (the engine's `HostCodeLike`). Pages that find the API
//! take their Trusted Types path, as they do in Chrome.

use boa_engine::builtins::eval::HostCodeLike;
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};

const KINDS: [&str; 3] = ["TrustedHTML", "TrustedScript", "TrustedScriptURL"];
const CREATE: [&str; 3] = ["createHTML", "createScript", "createScriptURL"];

#[derive(Trace, Finalize, JsData)]
struct Policy {
    #[unsafe_ignore_trace]
    name: JsString,
    /// The `create*` callbacks, as given when the policy was made.
    rules: JsObject,
}

#[derive(Trace, Finalize, JsData, Default)]
struct Factory {
    default: GcRefCell<Option<JsObject>>,
}

fn type_error(message: String) -> boa_engine::JsError {
    JsNativeError::typ().with_message(message).into()
}

fn func(cx: &mut Context, name: &str, length: usize, f: NativeFunction) -> JsObject {
    let realm = cx.realm().clone();
    FunctionObjectBuilder::new(&realm, f).name(JsString::from(name)).length(length).build().into()
}

fn method(cx: &mut Context, target: &JsObject, name: &str, length: usize, f: NativeFunction) {
    let f = func(cx, name, length, f);
    let desc = PropertyDescriptor::builder().value(f).writable(true).enumerable(true).configurable(true);
    target.define_property_or_throw(JsString::from(name), desc, cx).ok();
}

fn getter(cx: &mut Context, target: &JsObject, name: &str, f: NativeFunction) {
    let f = func(cx, &format!("get {name}"), 0, f);
    let desc = PropertyDescriptor::builder().get(f).enumerable(true).configurable(true);
    target.define_property_or_throw(JsString::from(name), desc, cx).ok();
}

/// A new trusted value of `kind` holding `text`.
fn make(kind: u8, text: JsString, cx: &mut Context) -> JsObject {
    let proto = crate::interfaces::interface(cx, KINDS[kind as usize]);
    let value = ObjectInitializer::with_native_data(HostCodeLike { kind, text }, cx).build();
    value.set_prototype(Some(proto));
    value
}

/// The text of `value` if it's a trusted value of `kind`.
fn text_of(value: &JsValue, kind: u8) -> Option<JsString> {
    value.as_object()?.downcast_ref::<HostCodeLike>().filter(|c| c.kind == kind).map(|c| c.text.clone())
}

/// `policy.createHTML(input, ...args)` and the others.
fn create(this: &JsValue, args: &[JsValue], kind: u8, cx: &mut Context) -> JsResult<JsValue> {
    let (name, rules) = {
        let policy = this.as_object().and_then(|o| o.downcast_ref::<Policy>().map(|p| (p.name.clone(), p.rules.clone())));
        policy.ok_or_else(|| type_error("Illegal invocation".into()))?
    };
    let member = CREATE[kind as usize];
    let callback = rules.get(JsString::from(member), cx)?;
    let Some(callback) = callback.as_callable() else {
        return Err(type_error(format!(
            "Failed to execute '{member}' on 'TrustedTypePolicy': Policy {}'s TrustedTypePolicyOptions did not specify a '{member}' member.",
            name.to_std_string_escaped()
        )));
    };
    let input = args.first().cloned().unwrap_or_default().to_string(cx)?;
    let mut call_args = vec![JsValue::from(input)];
    call_args.extend(args.iter().skip(1).cloned());
    let result = callback.call(&JsValue::undefined(), &call_args, cx)?;
    let text = if result.is_null_or_undefined() { js_string!() } else { result.to_string(cx)? };
    Ok(make(kind, text, cx).into())
}

/// Which trusted type a sink takes (`getAttributeType`/`getPropertyType`).
fn sink_type(tag: &str, name: &str, attribute: bool) -> Option<&'static str> {
    let (tag, name) = (tag.to_ascii_lowercase(), if attribute { name.to_ascii_lowercase() } else { name.to_owned() });
    match (tag.as_str(), name.as_str()) {
        (_, n) if attribute && n.starts_with("on") => Some("TrustedScript"),
        ("iframe", "srcdoc") => Some("TrustedHTML"),
        ("script", "src") => Some("TrustedScriptURL"),
        (_, "innerHTML" | "outerHTML") if !attribute => Some("TrustedHTML"),
        ("script", "text" | "textContent" | "innerText") if !attribute => Some("TrustedScript"),
        _ => None,
    }
}

/// Install `trustedTypes` and the interfaces on the window.
pub(crate) fn install(cx: &mut Context) {
    // The values: their text wherever a string goes.
    for (kind, name) in KINDS.iter().enumerate() {
        let proto = crate::interfaces::interface(cx, name);
        let to_string = NativeFunction::from_copy_closure(move |this, _, _| {
            text_of(this, kind as u8).map(JsValue::from).ok_or_else(|| type_error("Illegal invocation".into()))
        });
        method(cx, &proto, "toString", 0, to_string.clone());
        method(cx, &proto, "toJSON", 0, to_string);
    }

    // Policies.
    let policy_proto = crate::interfaces::interface(cx, "TrustedTypePolicy");
    getter(
        cx,
        &policy_proto,
        "name",
        NativeFunction::from_fn_ptr(|this, _, _| {
            let name = this.as_object().and_then(|o| o.downcast_ref::<Policy>().map(|p| p.name.clone()));
            name.map(JsValue::from).ok_or_else(|| type_error("Illegal invocation".into()))
        }),
    );
    for (kind, member) in CREATE.iter().enumerate() {
        method(cx, &policy_proto, member, 1, NativeFunction::from_copy_closure(move |this, a, cx| create(this, a, kind as u8, cx)));
    }

    // The factory.
    let factory_proto = crate::interfaces::interface(cx, "TrustedTypePolicyFactory");
    let factory = ObjectInitializer::with_native_data(Factory::default(), cx).build();
    factory.set_prototype(Some(factory_proto.clone()));
    method(
        cx,
        &factory_proto,
        "createPolicy",
        2,
        NativeFunction::from_copy_closure_with_captures(
            |this, a, policy_proto, cx| {
                let factory = this.as_object().filter(|o| o.downcast_ref::<Factory>().is_some()).ok_or_else(|| type_error("Illegal invocation".into()))?;
                let name = a.first().cloned().unwrap_or_default().to_string(cx)?;
                let is_default = name.to_std_string_escaped() == "default";
                if is_default && factory.downcast_ref::<Factory>().is_some_and(|f| f.default.borrow().is_some()) {
                    return Err(type_error("Failed to execute 'createPolicy' on 'TrustedTypePolicyFactory': Policy with name \"default\" already exists.".into()));
                }
                // The options' callbacks, read now (a dictionary).
                let rules = JsObject::with_null_proto();
                if let Some(options) = a.get(1).and_then(JsValue::as_object) {
                    for member in CREATE {
                        let f = options.get(JsString::from(member), cx)?;
                        if !f.is_undefined() {
                            rules.set(JsString::from(member), f, false, cx)?;
                        }
                    }
                }
                let policy = ObjectInitializer::with_native_data(Policy { name, rules }, cx).build();
                policy.set_prototype(Some(policy_proto.clone()));
                if is_default && let Some(f) = factory.downcast_ref::<Factory>() {
                    *f.default.borrow_mut() = Some(policy.clone());
                }
                Ok(policy.into())
            },
            policy_proto,
        ),
    );
    for (kind, name) in ["isHTML", "isScript", "isScriptURL"].iter().enumerate() {
        method(cx, &factory_proto, name, 1, NativeFunction::from_copy_closure(move |_, a, _| Ok(JsValue::from(text_of(&a.first().cloned().unwrap_or_default(), kind as u8).is_some()))));
    }
    getter(cx, &factory_proto, "emptyHTML", NativeFunction::from_fn_ptr(|_, _, cx| Ok(make(HostCodeLike::HTML, js_string!(), cx).into())));
    getter(cx, &factory_proto, "emptyScript", NativeFunction::from_fn_ptr(|_, _, cx| Ok(make(HostCodeLike::SCRIPT, js_string!(), cx).into())));
    getter(
        cx,
        &factory_proto,
        "defaultPolicy",
        NativeFunction::from_fn_ptr(|this, _, _| {
            let policy = this.as_object().and_then(|o| o.downcast_ref::<Factory>().and_then(|f| f.default.borrow().clone()));
            Ok(policy.map(JsValue::from).unwrap_or(JsValue::null()))
        }),
    );
    for (name, attribute) in [("getAttributeType", true), ("getPropertyType", false)] {
        method(
            cx,
            &factory_proto,
            name,
            2,
            NativeFunction::from_copy_closure(move |_, a, cx| {
                let tag = a.first().cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
                let member = a.get(1).cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
                Ok(sink_type(&tag, &member, attribute).map(|t| JsValue::from(JsString::from(t))).unwrap_or(JsValue::null()))
            }),
        );
    }
    let desc = PropertyDescriptor::builder().value(factory).writable(true).enumerable(true).configurable(true);
    cx.global_object().define_property_or_throw(js_string!("trustedTypes"), desc, cx).ok();
}
