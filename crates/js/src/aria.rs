//! ARIA reflection (`element.ariaCurrent = "true"`, `element.role`): the
//! properties stand for their attributes. Without them a page setting one
//! set a plain property, and nothing it styles by the attribute showed (MDN
//! marks the current section of its contents this way).

use boa_engine::object::FunctionObjectBuilder;
use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};

/// The ARIA attributes elements reflect (ARIAMixin), as properties.
const PROPERTIES: &[&str] = &[
    "role",
    "ariaAtomic",
    "ariaAutoComplete",
    "ariaBrailleLabel",
    "ariaBrailleRoleDescription",
    "ariaBusy",
    "ariaChecked",
    "ariaColCount",
    "ariaColIndex",
    "ariaColIndexText",
    "ariaColSpan",
    "ariaCurrent",
    "ariaDescription",
    "ariaDisabled",
    "ariaExpanded",
    "ariaHasPopup",
    "ariaHidden",
    "ariaInvalid",
    "ariaKeyShortcuts",
    "ariaLabel",
    "ariaLevel",
    "ariaLive",
    "ariaModal",
    "ariaMultiLine",
    "ariaMultiSelectable",
    "ariaOrientation",
    "ariaPlaceholder",
    "ariaPosInSet",
    "ariaPressed",
    "ariaReadOnly",
    "ariaRelevant",
    "ariaRequired",
    "ariaRoleDescription",
    "ariaRowCount",
    "ariaRowIndex",
    "ariaRowIndexText",
    "ariaRowSpan",
    "ariaSelected",
    "ariaSetSize",
    "ariaSort",
    "ariaValueMax",
    "ariaValueMin",
    "ariaValueNow",
    "ariaValueText",
];

/// `ariaValueNow` → `aria-valuenow`.
fn attribute(property: &str) -> String {
    match property.strip_prefix("aria") {
        Some(rest) => format!("aria-{}", rest.to_ascii_lowercase()),
        None => property.to_owned(),
    }
}

/// Call `this.<method>(args)` (the element's own attribute methods, so
/// mutation records and custom elements' callbacks happen as usual).
fn call(this: &JsValue, method: &str, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let Some(o) = this.as_object() else { return Ok(JsValue::null()) };
    match o.get(JsString::from(method), cx)?.as_callable() {
        Some(f) => f.call(this, args, cx),
        None => Ok(JsValue::null()),
    }
}

/// Define the properties on `proto` (`Element.prototype`).
fn install_on(proto: &JsObject, cx: &mut Context) -> JsResult<()> {
    for &property in PROPERTIES {
        let name = JsString::from(attribute(property));
        let get_name = name.clone();
        let get = NativeFunction::from_copy_closure_with_captures(|this, _, name: &JsString, cx| call(this, "getAttribute", &[name.clone().into()], cx), get_name);
        let set = NativeFunction::from_copy_closure_with_captures(
            |this, a, name: &JsString, cx| {
                let v = a.first().cloned().unwrap_or_default();
                if v.is_null() {
                    call(this, "removeAttribute", &[name.clone().into()], cx)?;
                } else {
                    let v = v.to_string(cx)?;
                    call(this, "setAttribute", &[name.clone().into(), v.into()], cx)?;
                }
                Ok(JsValue::undefined())
            },
            name,
        );
        let get = FunctionObjectBuilder::new(cx.realm(), get).name(JsString::from(property)).build();
        let set = FunctionObjectBuilder::new(cx.realm(), set).name(JsString::from(property)).build();
        proto.define_property_or_throw(
            JsString::from(property),
            PropertyDescriptor::builder().get(get).set(set).configurable(true).enumerable(true),
            cx,
        )?;
    }
    Ok(())
}

/// The `aria` native: `N.aria(Element.prototype)`.
pub(crate) fn install(natives: &JsObject, cx: &mut Context) {
    let f = NativeFunction::from_fn_ptr(|_, a, cx| {
        if let Some(proto) = a.first().and_then(|p| p.as_object()) {
            install_on(&proto, cx)?;
        }
        Ok(JsValue::undefined())
    });
    let f = FunctionObjectBuilder::new(cx.realm(), f).name(js_string!("aria")).build();
    natives.set(js_string!("aria"), f, false, cx).ok();
}
