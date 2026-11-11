//! `HTMLFormElement.prototype.elements`, native: the form's controls as an
//! `HTMLFormControlsCollection` (indexed, `item`, `namedItem`, and named
//! access: `form.elements.solution`), in tree order, with controls
//! elsewhere that name the form in their `form` attribute.

use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsNativeError, JsResult, JsString, JsValue, NativeFunction, js_string};
use page::NodeId;

use crate::dom::{node_from_number, node_to_number};

const CONTROLS: &[&str] = &["input", "select", "textarea", "button", "fieldset", "output", "object"];

/// The form's controls, in tree order.
fn controls(form: NodeId, cx: &Context) -> Vec<NodeId> {
    let d = crate::dom::page(cx);
    let dom = d.borrow();
    let doc = &dom.doc;
    let form_id = doc.element(form).and_then(|e| e.attr("id")).map(str::to_owned);
    let is_control = |n: NodeId| doc.element(n).is_some_and(|e| e.is_html() && CONTROLS.contains(&&**e.local_name()));
    // Its own, unless they name another form; and those naming it.
    let mut out: Vec<NodeId> = doc
        .descendants(form)
        .filter(|&n| n != form && is_control(n))
        .filter(|&n| doc.element(n).and_then(|e| e.attr("form")).is_none_or(|f| Some(f) == form_id.as_deref()))
        .collect();
    if let Some(id) = form_id {
        for n in doc.descendants(doc.root()) {
            if is_control(n) && !out.contains(&n) && doc.element(n).and_then(|e| e.attr("form")) == Some(id.as_str()) {
                out.push(n);
            }
        }
        // Back into tree order.
        let order: Vec<NodeId> = doc.descendants(doc.root()).collect();
        out.sort_by_key(|n| order.iter().position(|m| m == n));
    }
    out
}

fn get_elements(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a form"))?;
    let id = o.get(js_string!("__featherNode"), cx)?.as_number().ok_or_else(|| JsNativeError::typ().with_message("not a form"))?;
    let nodes = controls(node_from_number(id), cx);
    let d = crate::dom::page(cx);
    let names: Vec<(Option<String>, Option<String>)> = {
        let dom = d.borrow();
        nodes
            .iter()
            .map(|&n| {
                let e = dom.doc.element(n);
                (e.and_then(|e| e.attr("id")).map(str::to_owned), e.and_then(|e| e.attr("name")).map(str::to_owned))
            })
            .collect()
    };
    let wrapped: Vec<JsValue> = nodes.iter().map(|&n| crate::dom::call_global(&d, "__wrapNode", &[JsValue::from(node_to_number(n))], cx)).collect();
    let list = JsArray::from_iter(wrapped.clone(), cx);
    // Named access: the first control with that id or name.
    for (value, (id, name)) in wrapped.iter().zip(&names) {
        for key in [id, name].into_iter().flatten() {
            let key = JsString::from(key.as_str());
            if !list.has_own_property(key.clone(), cx)? {
                list.define_property_or_throw(key, PropertyDescriptor::builder().value(value.clone()).writable(false).enumerable(false).configurable(true), cx)?;
            }
        }
    }
    let item = NativeFunction::from_fn_ptr(|this, a, cx| {
        let list = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a collection"))?;
        let i = a.first().cloned().unwrap_or_default().to_u32(cx)?;
        let v = list.get(i, cx)?;
        Ok(if v.is_undefined() { JsValue::null() } else { v })
    });
    let named = NativeFunction::from_fn_ptr(|this, a, cx| {
        let list = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a collection"))?;
        let key = a.first().cloned().unwrap_or_default().to_string(cx)?;
        Ok(if list.has_own_property(key.clone(), cx)? { list.get(key, cx)? } else { JsValue::null() })
    });
    for (name, f) in [("item", item), ("namedItem", named)] {
        let f = FunctionObjectBuilder::new(cx.realm(), f).name(JsString::from(name)).length(1).build();
        list.define_property_or_throw(JsString::from(name), PropertyDescriptor::builder().value(f).writable(true).enumerable(false).configurable(true), cx)?;
    }
    Ok(list.into())
}

/// After the prelude: `elements` on `HTMLFormElement.prototype`.
pub(crate) fn link(cx: &mut Context) {
    let Some(class) = cx.global_object().get(js_string!("HTMLFormElement"), cx).ok().and_then(|c| c.as_object()) else { return };
    let Some(proto) = class.get(js_string!("prototype"), cx).ok().and_then(|p| p.as_object()) else { return };
    let get = FunctionObjectBuilder::new(cx.realm(), NativeFunction::from_fn_ptr(get_elements)).name(js_string!("get elements")).build();
    proto
        .define_property_or_throw(js_string!("elements"), PropertyDescriptor::builder().get(get).configurable(true).enumerable(true), cx)
        .ok();
}
