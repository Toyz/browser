//! `window.console`, `window.navigator` and `window.location`, native.

use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::object::builtins::JsFunction;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use url::quirks;

use crate::dom::{Shared, js_value_string};

/// A native function capturing the page's DOM.
fn native(dom: &Shared, f: fn(&Shared, &[JsValue], &mut Context) -> JsResult<JsValue>) -> NativeFunction {
    let d = dom.clone();
    // SAFETY: the closure captures only the Rc'd DOM, which holds no
    // garbage-collected values.
    unsafe { NativeFunction::from_closure(move |_this, args, cx| f(&d, args, cx)) }
}

fn function(cx: &mut Context, name: &str, f: NativeFunction) -> JsFunction {
    FunctionObjectBuilder::new(cx.realm(), f).name(JsString::from(name)).build()
}

/// Install `console`, `navigator` and `location` on the natives object.
pub(crate) fn install(natives: &JsObject, dom: &Shared, cx: &mut Context) {
    let console = console(dom, cx);
    let navigator = navigator(cx);
    let location = location(dom, cx);
    for (name, value) in [("console", console), ("navigator", navigator), ("location", location)] {
        natives.set(JsString::from(name), value, false, cx).ok();
    }
}

/// What `console.log(a, b)` prints: strings as they are, errors with their
/// stack, elements as `<tag>`, everything else as JSON where it can be.
fn format(args: &[JsValue], cx: &mut Context) -> String {
    let mut parts = Vec::with_capacity(args.len());
    for a in args {
        let text = match a {
            v if v.is_string() => js_value_string(v, cx),
            v if v.is_object() => {
                let o = v.as_object().expect("an object");
                let stack = o.get(js_string!("stack"), cx).ok().filter(|s| s.is_string());
                let local = o.get(js_string!("localName"), cx).ok().filter(|s| s.is_string());
                if let Some(stack) = stack {
                    format!("{}\n{}", js_value_string(v, cx), js_value_string(&stack, cx))
                } else if let Some(local) = local {
                    format!("<{}>", js_value_string(&local, cx))
                } else {
                    let json = cx.global_object().get(js_string!("JSON"), cx).ok().and_then(|j| j.as_object());
                    let stringify = json.as_ref().and_then(|j| j.get(js_string!("stringify"), cx).ok()).and_then(|f| f.as_callable());
                    match (json, stringify) {
                        (Some(json), Some(f)) => match f.call(&json.into(), std::slice::from_ref(v), cx) {
                            Ok(s) if s.is_string() => js_value_string(&s, cx),
                            _ => js_value_string(v, cx),
                        },
                        _ => js_value_string(v, cx),
                    }
                }
            }
            v => js_value_string(v, cx),
        };
        parts.push(text);
    }
    parts.join(" ")
}

fn console(dom: &Shared, cx: &mut Context) -> JsValue {
    let mut init = ObjectInitializer::new(cx);
    // Printed when debugging (BROWSER_DEBUG), as `console.level: text`.
    macro_rules! level {
        ($name:literal) => {{
            let f = native(dom, |d, a, cx| {
                let text = format(a, cx);
                if crate::dom::debug_logs(d) {
                    eprintln!(concat!("console.", $name, ": {}"), text);
                }
                Ok(JsValue::undefined())
            });
            init.function(f, js_string!($name), 0);
        }};
    }
    level!("log");
    level!("info");
    level!("warn");
    level!("debug");
    level!("error");
    level!("trace");
    for name in ["group", "groupCollapsed", "groupEnd", "time", "timeEnd", "timeLog", "count", "countReset", "assert", "table", "dir", "dirxml", "clear"] {
        init.function(NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined())), JsString::from(name), 0);
    }
    init.build().into()
}

fn navigator(cx: &mut Context) -> JsValue {
    let ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Feather/0.1 Safari/537.36";
    let languages = boa_engine::object::builtins::JsArray::from_iter([js_string!("en-US").into(), js_string!("en").into()], cx);
    // `sendBeacon(url, data)`: a fire-and-forget POST, through `fetch`.
    let send_beacon = NativeFunction::from_fn_ptr(|_, a, cx| {
        let Some(fetch) = cx.global_object().get(js_string!("fetch"), cx)?.as_callable() else { return Ok(JsValue::from(false)) };
        let init = ObjectInitializer::new(cx)
            .property(js_string!("method"), js_string!("POST"), Attribute::all())
            .property(js_string!("body"), a.get(1).cloned().unwrap_or_default(), Attribute::all())
            .property(js_string!("mode"), js_string!("no-cors"), Attribute::all())
            .property(js_string!("credentials"), js_string!("include"), Attribute::all())
            .build();
        let url = a.first().cloned().unwrap_or_default();
        fetch.call(&JsValue::undefined(), &[url, init.into()], cx).ok();
        Ok(JsValue::from(true))
    });
    let resolved = |cx: &mut Context| -> JsFunction {
        let f = NativeFunction::from_fn_ptr(|_, _, cx| Ok(boa_engine::object::builtins::JsPromise::resolve(js_string!(""), cx)?.into()));
        function(cx, "", f)
    };
    let (write, read) = (resolved(cx), resolved(cx));
    let clipboard = ObjectInitializer::new(cx)
        .property(js_string!("writeText"), write, Attribute::all())
        .property(js_string!("readText"), read, Attribute::all())
        .build();
    let ro = Attribute::READONLY | Attribute::ENUMERABLE | Attribute::CONFIGURABLE;
    ObjectInitializer::new(cx)
        .property(js_string!("userAgent"), JsString::from(ua), ro)
        .property(js_string!("appVersion"), JsString::from(&ua[8..]), ro)
        .property(js_string!("language"), js_string!("en-US"), ro)
        .property(js_string!("languages"), languages, ro)
        .property(js_string!("platform"), js_string!("MacIntel"), ro)
        .property(js_string!("vendor"), js_string!(""), ro)
        .property(js_string!("onLine"), true, ro)
        .property(js_string!("cookieEnabled"), true, ro)
        .property(js_string!("hardwareConcurrency"), std::thread::available_parallelism().map_or(4, |n| n.get()) as i32, ro)
        .property(js_string!("maxTouchPoints"), 0, ro)
        .property(js_string!("webdriver"), false, ro)
        .property(js_string!("clipboard"), clipboard, ro)
        .function(send_beacon, js_string!("sendBeacon"), 2)
        .build()
        .into()
}

/// The parts of `location` with their `url::quirks` getter and setter.
const PARTS: &[(&str, fn(&url::Url) -> &str, fn(&mut url::Url, &str) -> Result<(), ()>)] = &[
    ("protocol", quirks::protocol, quirks::set_protocol),
    ("host", quirks::host, quirks::set_host),
    ("hostname", quirks::hostname, quirks::set_hostname),
    ("port", quirks::port, quirks::set_port),
    ("pathname", quirks::pathname, |u, v| {
        quirks::set_pathname(u, v);
        Ok(())
    }),
    ("search", quirks::search, |u, v| {
        quirks::set_search(u, v);
        Ok(())
    }),
    ("hash", quirks::hash, |u, v| {
        quirks::set_hash(u, v);
        Ok(())
    }),
];

fn location(dom: &Shared, cx: &mut Context) -> JsValue {
    let href_get = native(dom, |d, _, _| Ok(JsValue::from(JsString::from(d.borrow().doc.url.as_str()))));
    let href_set = native(dom, |d, a, cx| {
        let to = js_value_string(&a.first().cloned().unwrap_or_default(), cx);
        crate::dom::navigate_to(d, &to);
        Ok(JsValue::undefined())
    });
    let (href_get, href_set) = (function(cx, "href", href_get), function(cx, "href", href_set));
    let go = native(dom, |d, a, cx| {
        let to = js_value_string(&a.first().cloned().unwrap_or_default(), cx);
        crate::dom::navigate_to(d, &to);
        Ok(JsValue::undefined())
    });
    let reload = native(dom, |d, _, _| {
        let here = d.borrow().doc.url.to_string();
        crate::dom::navigate_to(d, &here);
        Ok(JsValue::undefined())
    });
    let to_string = native(dom, |d, _, _| Ok(JsValue::from(JsString::from(d.borrow().doc.url.as_str()))));
    let origin = native(dom, |d, _, _| Ok(JsValue::from(JsString::from(quirks::origin(&d.borrow().doc.url)))));
    let origin = function(cx, "origin", origin);
    let mut accessors = Vec::new();
    for (i, &(name, _, _)) in PARTS.iter().enumerate() {
        // (A part's index rides in the closure: one getter and setter each.)
        let d = dom.clone();
        // SAFETY: captures only the Rc'd DOM and an index.
        let get = unsafe {
            NativeFunction::from_closure(move |_, _, _| Ok(JsValue::from(JsString::from((PARTS[i].1)(&d.borrow().doc.url)))))
        };
        let d = dom.clone();
        // SAFETY: as above.
        let set = unsafe {
            NativeFunction::from_closure(move |_, a, cx| {
                let value = js_value_string(&a.first().cloned().unwrap_or_default(), cx);
                let mut url = d.borrow().doc.url.clone();
                if (PARTS[i].2)(&mut url, &value).is_err() {
                    return Ok(JsValue::undefined());
                }
                if PARTS[i].0 == "hash" {
                    // A new entry in this document, then `hashchange`.
                    if let Some(f) = cx.global_object().get(js_string!("__hashNavigate"), cx)?.as_callable() {
                        f.call(&JsValue::undefined(), &[JsValue::from(JsString::from(url.as_str()))], cx)?;
                    }
                } else {
                    crate::dom::navigate_to(&d, url.as_str());
                }
                Ok(JsValue::undefined())
            })
        };
        accessors.push((name, function(cx, name, get), function(cx, name, set)));
    }
    let mut init = ObjectInitializer::new(cx);
    init.accessor(js_string!("href"), Some(href_get), Some(href_set), Attribute::ENUMERABLE | Attribute::CONFIGURABLE)
        .accessor(js_string!("origin"), Some(origin), None, Attribute::ENUMERABLE | Attribute::CONFIGURABLE)
        .function(go.clone(), js_string!("assign"), 1)
        .function(go, js_string!("replace"), 1)
        .function(reload, js_string!("reload"), 0)
        .function(to_string, js_string!("toString"), 0);
    for (name, get, set) in accessors {
        init.accessor(JsString::from(name), Some(get), Some(set), Attribute::ENUMERABLE | Attribute::CONFIGURABLE);
    }
    init.build().into()
}
