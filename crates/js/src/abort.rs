//! `AbortController` and `AbortSignal`, native. A signal is an
//! `EventTarget` (the prelude's: linked in once it has run, see [`link`]);
//! aborting it sets its reason and fires `abort` (and `onabort`).

use std::cell::Cell;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};

#[derive(Trace, Finalize, JsData)]
struct AbortSignal {
    #[unsafe_ignore_trace]
    aborted: Cell<bool>,
    reason: GcRefCell<JsValue>,
}

#[derive(Trace, Finalize, JsData)]
struct AbortController {
    signal: JsObject,
}

fn signal_of(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|o| o.downcast_ref::<AbortSignal>().is_some())
        .ok_or_else(|| JsNativeError::typ().with_message("not an AbortSignal").into())
}

fn new_signal(cx: &mut Context) -> JsResult<JsObject> {
    AbortSignal::from_data(AbortSignal { aborted: Cell::new(false), reason: GcRefCell::new(JsValue::undefined()) }, cx)
}

/// Abort `signal` with `reason` (an `AbortError` when undefined): once.
fn abort(signal: &JsObject, reason: &JsValue, cx: &mut Context) -> JsResult<()> {
    {
        let s = signal.downcast_ref::<AbortSignal>().expect("an AbortSignal");
        if s.aborted.replace(true) {
            return Ok(());
        }
        let reason = if reason.is_undefined() {
            crate::dom_exception::error("AbortError", "signal is aborted without reason", cx).into_opaque(cx)?
        } else {
            reason.clone()
        };
        *s.reason.borrow_mut() = reason;
    }
    let event = cx.global_object().get(js_string!("Event"), cx)?;
    let Some(event) = event.as_constructor() else { return Ok(()) };
    let event: JsValue = event.construct(&[js_string!("abort").into()], None, cx)?.into();
    let dispatch = signal.get(js_string!("dispatchEvent"), cx)?;
    if let Some(dispatch) = dispatch.as_callable() {
        dispatch.call(&signal.clone().into(), std::slice::from_ref(&event), cx)?;
    }
    let handler = signal.get(js_string!("onabort"), cx)?;
    if let Some(handler) = handler.as_callable() {
        handler.call(&signal.clone().into(), &[event], cx)?;
    }
    Ok(())
}

fn getter(class: &mut ClassBuilder<'_>, name: &str, f: NativeFunction) {
    let realm = class.context().realm().clone();
    let get = FunctionObjectBuilder::new(&realm, f).name(JsString::from(format!("get {name}"))).build();
    class.accessor(JsString::from(name), Some(get), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
}

impl Class for AbortSignal {
    const NAME: &'static str = "AbortSignal";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        getter(class, "aborted", NativeFunction::from_fn_ptr(|this, _, _| {
            let o = signal_of(this)?;
            Ok(o.downcast_ref::<AbortSignal>().expect("an AbortSignal").aborted.get().into())
        }));
        getter(class, "reason", NativeFunction::from_fn_ptr(|this, _, _| {
            let o = signal_of(this)?;
            Ok(o.downcast_ref::<AbortSignal>().expect("an AbortSignal").reason.borrow().clone())
        }));
        class.property(js_string!("onabort"), JsValue::null(), Attribute::all());
        class.method(js_string!("throwIfAborted"), 0, NativeFunction::from_fn_ptr(|this, _, _| {
            let o = signal_of(this)?;
            let s = o.downcast_ref::<AbortSignal>().expect("an AbortSignal");
            if s.aborted.get() {
                return Err(boa_engine::JsError::from_opaque(s.reason.borrow().clone()));
            }
            Ok(JsValue::undefined())
        }));
        class.static_method(js_string!("abort"), 0, NativeFunction::from_fn_ptr(|_, a, cx| {
            let s = new_signal(cx)?;
            abort(&s, &a.first().cloned().unwrap_or_default(), cx)?;
            Ok(s.into())
        }));
        class.static_method(js_string!("timeout"), 1, NativeFunction::from_fn_ptr(|_, a, cx| {
            let ms = a.first().cloned().unwrap_or_default().to_number(cx)?;
            let s = new_signal(cx)?;
            let fire = NativeFunction::from_copy_closure_with_captures(
                |_, _, s: &JsObject, cx| {
                    let e = crate::dom_exception::error("TimeoutError", "The operation timed out.", cx).into_opaque(cx)?;
                    abort(s, &e, cx)?;
                    Ok(JsValue::undefined())
                },
                s.clone(),
            );
            let fire = FunctionObjectBuilder::new(cx.realm(), fire).build();
            let set_timeout = cx.global_object().get(js_string!("setTimeout"), cx)?;
            if let Some(set_timeout) = set_timeout.as_callable() {
                set_timeout.call(&JsValue::undefined(), &[fire.into(), ms.into()], cx)?;
            }
            Ok(s.into())
        }));
        // Aborted when any of them is (with its reason).
        class.static_method(js_string!("any"), 1, NativeFunction::from_fn_ptr(|_, a, cx| {
            let s = new_signal(cx)?;
            let list = a.first().and_then(JsValue::as_object).ok_or_else(|| JsNativeError::typ().with_message("AbortSignal.any needs signals"))?;
            let list = JsArray::from_object(list).map_err(|_| JsNativeError::typ().with_message("AbortSignal.any needs an array"))?;
            for i in 0..list.length(cx)? {
                let source = signal_of(&list.get(i, cx)?)?;
                let (done, reason) = {
                    let d = source.downcast_ref::<AbortSignal>().expect("an AbortSignal");
                    (d.aborted.get(), d.reason.borrow().clone())
                };
                if done {
                    abort(&s, &reason, cx)?;
                    break;
                }
                let follow = NativeFunction::from_copy_closure_with_captures(
                    |_, _, (s, source): &(JsObject, JsObject), cx| {
                        let reason = source.downcast_ref::<AbortSignal>().expect("an AbortSignal").reason.borrow().clone();
                        abort(s, &reason, cx)?;
                        Ok(JsValue::undefined())
                    },
                    (s.clone(), source.clone()),
                );
                let follow = FunctionObjectBuilder::new(cx.realm(), follow).build();
                let add = source.get(js_string!("addEventListener"), cx)?;
                if let Some(add) = add.as_callable() {
                    add.call(&source.clone().into(), &[js_string!("abort").into(), follow.into()], cx)?;
                }
            }
            Ok(s.into())
        }));
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<Self> {
        Ok(AbortSignal { aborted: Cell::new(false), reason: GcRefCell::new(JsValue::undefined()) })
    }
}

impl Class for AbortController {
    const NAME: &'static str = "AbortController";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        getter(class, "signal", NativeFunction::from_fn_ptr(|this, _, _| {
            let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not an AbortController"))?;
            let c = o.downcast_ref::<AbortController>().ok_or_else(|| JsNativeError::typ().with_message("not an AbortController"))?;
            Ok(c.signal.clone().into())
        }));
        class.method(js_string!("abort"), 0, NativeFunction::from_fn_ptr(|this, a, cx| {
            let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not an AbortController"))?;
            let signal = o.downcast_ref::<AbortController>().ok_or_else(|| JsNativeError::typ().with_message("not an AbortController"))?.signal.clone();
            abort(&signal, &a.first().cloned().unwrap_or_default(), cx)?;
            Ok(JsValue::undefined())
        }));
        Ok(())
    }

    fn data_constructor(_: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        Ok(AbortController { signal: new_signal(cx)? })
    }
}

/// Register `AbortController` and `AbortSignal`.
pub(crate) fn install(cx: &mut Context) {
    for r in [cx.register_global_class::<AbortSignal>(), cx.register_global_class::<AbortController>()] {
        if let Err(e) = r {
            eprintln!("js: abort: {e}");
        }
    }
}

/// Make `AbortSignal` an `EventTarget` (once the prelude has made that).
pub(crate) fn link(cx: &mut Context) {
    crate::dom::extend_event_target::<AbortSignal>(cx);
}
