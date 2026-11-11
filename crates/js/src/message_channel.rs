//! `MessageChannel` and `MessagePort` for workers (pages have the
//! prelude's): two entangled ports; what one posts arrives at the other as a
//! `message` task, structured-cloned.

use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};

#[derive(Trace, Finalize, JsData, Default)]
struct Port {
    /// The port at the other end.
    other: GcRefCell<Option<JsObject>>,
    /// `message` listeners.
    listeners: GcRefCell<Vec<JsObject>>,
    closed: GcRefCell<bool>,
}

fn port_of(this: &JsValue) -> JsResult<JsObject> {
    this.as_object().filter(|o| o.downcast_ref::<Port>().is_some()).ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation").into())
}

/// Deliver `data` to `port`'s `onmessage` and listeners.
fn deliver(_: &JsValue, _: &[JsValue], (port, data): &(JsObject, JsValue), cx: &mut Context) -> JsResult<JsValue> {
    if port.downcast_ref::<Port>().is_some_and(|p| *p.closed.borrow()) {
        return Ok(JsValue::undefined());
    }
    let ports = boa_engine::object::builtins::JsArray::new(cx)?;
    let event = ObjectInitializer::new(cx)
        .property(js_string!("type"), js_string!("message"), Attribute::all())
        .property(js_string!("data"), data.clone(), Attribute::all())
        .property(js_string!("origin"), js_string!(), Attribute::all())
        .property(js_string!("lastEventId"), js_string!(), Attribute::all())
        .property(js_string!("source"), JsValue::null(), Attribute::all())
        .property(js_string!("ports"), ports, Attribute::all())
        .property(js_string!("target"), port.clone(), Attribute::all())
        .property(js_string!("currentTarget"), port.clone(), Attribute::all())
        .build();
    let mut handlers: Vec<JsObject> = port.downcast_ref::<Port>().map(|p| p.listeners.borrow().clone()).unwrap_or_default();
    if let Some(f) = port.get(js_string!("onmessage"), cx)?.as_object().filter(|f| f.is_callable()) {
        handlers.insert(0, f);
    }
    for f in handlers {
        // A listener's error doesn't stop the others.
        f.call(&port.clone().into(), &[event.clone().into()], cx).ok();
    }
    Ok(JsValue::undefined())
}

fn new_port(proto: &JsObject, cx: &mut Context) -> JsObject {
    let port = ObjectInitializer::with_native_data(Port::default(), cx).property(js_string!("onmessage"), JsValue::null(), Attribute::all()).build();
    port.set_prototype(Some(proto.clone()));
    port
}

fn method(cx: &mut Context, proto: &JsObject, name: &str, length: usize, f: NativeFunction) {
    let realm = cx.realm().clone();
    let f = FunctionObjectBuilder::new(&realm, f).name(JsString::from(name)).length(length).build();
    proto.set(JsString::from(name), f, false, cx).ok();
}

/// Install `MessageChannel` and `MessagePort` on a worker's global.
pub(crate) fn install(cx: &mut Context) {
    let proto = JsObject::with_object_proto(cx.intrinsics());
    method(
        cx,
        &proto,
        "postMessage",
        1,
        NativeFunction::from_fn_ptr(|this, a, cx| {
            let port = port_of(this)?;
            let other = port.downcast_ref::<Port>().and_then(|p| p.other.borrow().clone());
            let Some(other) = other else { return Ok(JsValue::undefined()) };
            let data = crate::structured_clone::clone(&a.first().cloned().unwrap_or_default(), cx)?;
            // As a task: after the poster's code has run.
            let task = NativeFunction::from_copy_closure_with_captures(deliver, (other, data));
            let task = FunctionObjectBuilder::new(cx.realm(), task).build();
            let set_timeout = cx.global_object().get(js_string!("setTimeout"), cx)?;
            if let Some(set_timeout) = set_timeout.as_callable() {
                set_timeout.call(&JsValue::undefined(), &[task.into(), JsValue::from(0)], cx)?;
            }
            Ok(JsValue::undefined())
        }),
    );
    method(cx, &proto, "start", 0, NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined())));
    method(
        cx,
        &proto,
        "close",
        0,
        NativeFunction::from_fn_ptr(|this, _, _| {
            let port = port_of(this)?;
            if let Some(p) = port.downcast_ref::<Port>() {
                *p.closed.borrow_mut() = true;
            }
            Ok(JsValue::undefined())
        }),
    );
    method(
        cx,
        &proto,
        "addEventListener",
        2,
        NativeFunction::from_fn_ptr(|this, a, cx| {
            let port = port_of(this)?;
            let kind = a.first().map(|v| v.to_string(cx)).transpose()?.map(|s| s.to_std_string_escaped()).unwrap_or_default();
            if kind == "message"
                && let Some(f) = a.get(1).and_then(JsValue::as_object).filter(|f| f.is_callable())
                && let Some(p) = port.downcast_ref::<Port>()
                && !p.listeners.borrow().contains(&f)
            {
                p.listeners.borrow_mut().push(f);
            }
            Ok(JsValue::undefined())
        }),
    );
    method(
        cx,
        &proto,
        "removeEventListener",
        2,
        NativeFunction::from_fn_ptr(|this, a, _| {
            let port = port_of(this)?;
            if let (Some(f), Some(p)) = (a.get(1).and_then(JsValue::as_object), port.downcast_ref::<Port>()) {
                p.listeners.borrow_mut().retain(|g| g != &f);
            }
            Ok(JsValue::undefined())
        }),
    );

    let realm = cx.realm().clone();
    let port_ctor = FunctionObjectBuilder::new(
        &realm,
        NativeFunction::from_fn_ptr(|_, _, _| Err(JsNativeError::typ().with_message("Illegal constructor").into())),
    )
    .name(js_string!("MessagePort"))
    .length(0)
    .constructor(true)
    .build();
    port_ctor.set(js_string!("prototype"), proto.clone(), false, cx).ok();
    proto.set(js_string!("constructor"), port_ctor.clone(), false, cx).ok();

    let channel_proto = JsObject::with_object_proto(cx.intrinsics());
    let channel_ctor = FunctionObjectBuilder::new(
        &realm,
        NativeFunction::from_copy_closure_with_captures(
            |this, _, (proto, channel_proto): &(JsObject, JsObject), cx| {
                if this.is_undefined() {
                    return Err(JsNativeError::typ().with_message("Constructor MessageChannel requires 'new'").into());
                }
                let (a, b) = (new_port(proto, cx), new_port(proto, cx));
                if let (Some(pa), Some(pb)) = (a.downcast_ref::<Port>(), b.downcast_ref::<Port>()) {
                    *pa.other.borrow_mut() = Some(b.clone());
                    *pb.other.borrow_mut() = Some(a.clone());
                }
                let channel = ObjectInitializer::new(cx)
                    .property(js_string!("port1"), a, Attribute::READONLY | Attribute::ENUMERABLE)
                    .property(js_string!("port2"), b, Attribute::READONLY | Attribute::ENUMERABLE)
                    .build();
                channel.set_prototype(Some(channel_proto.clone()));
                Ok(channel.into())
            },
            (proto.clone(), channel_proto.clone()),
        ),
    )
    .name(js_string!("MessageChannel"))
    .length(0)
    .constructor(true)
    .build();
    channel_ctor.set(js_string!("prototype"), channel_proto.clone(), false, cx).ok();
    channel_proto.set(js_string!("constructor"), channel_ctor.clone(), false, cx).ok();

    for (object, tag) in [(&proto, "MessagePort"), (&channel_proto, "MessageChannel")] {
        let desc = boa_engine::property::PropertyDescriptor::builder().value(JsString::from(tag)).writable(false).enumerable(false).configurable(true);
        object.define_property_or_throw(boa_engine::JsSymbol::to_string_tag(), desc, cx).ok();
    }
    let global = cx.global_object();
    global.set(js_string!("MessagePort"), port_ctor, false, cx).ok();
    global.set(js_string!("MessageChannel"), channel_ctor, false, cx).ok();
}
