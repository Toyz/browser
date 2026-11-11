//! `URL` and `URLSearchParams`, as native classes over the `url` crate (the
//! WHATWG URL Standard: parsing, the setters' rules, `origin`), with a URL's
//! `searchParams` kept in step with its query both ways.

use std::cell::RefCell;

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsArgs, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use boa_gc::{Finalize, GcRefCell, Trace};
use url::{Url, quirks};

#[derive(Debug, Trace, Finalize, JsData)]
pub struct WebUrl {
    #[unsafe_ignore_trace]
    url: RefCell<Url>,
    /// Its `searchParams` object, once asked for (always the same one).
    params: GcRefCell<Option<JsObject>>,
}

#[derive(Debug, Trace, Finalize, JsData)]
pub struct WebSearchParams {
    #[unsafe_ignore_trace]
    list: RefCell<Vec<(String, String)>>,
    /// The URL whose query this is.
    owner: GcRefCell<Option<JsObject>>,
}

fn type_error(message: &str) -> boa_engine::JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}

fn arg(args: &[JsValue], i: usize, cx: &mut Context) -> JsResult<String> {
    Ok(args.get_or_undefined(i).to_string(cx)?.to_std_string_escaped())
}

fn js(s: &str) -> JsValue {
    JsValue::from(JsString::from(s))
}

fn this_object<T: boa_engine::object::NativeObject>(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|o| o.downcast_ref::<T>().is_some())
        .ok_or_else(|| type_error("Illegal invocation"))
}

fn parse(input: &str, base: Option<&str>) -> Option<Url> {
    match base {
        Some(base) => Url::parse(base).ok()?.join(input).ok(),
        None => Url::parse(input).ok(),
    }
}

impl WebUrl {
    /// After the URL's query changed: its params object follows.
    fn query_changed(&self) {
        if let Some(params) = &*self.params.borrow()
            && let Some(p) = params.downcast_ref::<WebSearchParams>()
        {
            *p.list.borrow_mut() = parse_query(quirks::search(&self.url.borrow()));
        }
    }
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    let query = query.strip_prefix('?').unwrap_or(query);
    url::form_urlencoded::parse(query.as_bytes()).into_owned().collect()
}

fn serialize(list: &[(String, String)]) -> String {
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(list).finish()
}

impl WebSearchParams {
    /// After the list changed: the owner URL's query follows.
    fn changed(&self) {
        if let Some(owner) = &*self.owner.borrow()
            && let Some(u) = owner.downcast_ref::<WebUrl>()
        {
            let query = serialize(&self.list.borrow());
            u.url.borrow_mut().set_query(if query.is_empty() { None } else { Some(&query) });
        }
    }
}

type Getter = fn(&Url) -> String;
type Setter = fn(&mut Url, &str);

/// The URL's components: (name, getter, setter).
const COMPONENTS: &[(&str, Getter, Option<Setter>)] = &[
    ("href", |u| quirks::href(u).to_owned(), None),
    ("origin", |u| quirks::origin(u), None),
    ("protocol", |u| quirks::protocol(u).to_owned(), Some(|u, v| { let _ = quirks::set_protocol(u, v); })),
    ("username", |u| quirks::username(u).to_owned(), Some(|u, v| { let _ = quirks::set_username(u, v); })),
    ("password", |u| quirks::password(u).to_owned(), Some(|u, v| { let _ = quirks::set_password(u, v); })),
    ("host", |u| quirks::host(u).to_owned(), Some(|u, v| { let _ = quirks::set_host(u, v); })),
    ("hostname", |u| quirks::hostname(u).to_owned(), Some(|u, v| { let _ = quirks::set_hostname(u, v); })),
    ("port", |u| quirks::port(u).to_owned(), Some(|u, v| { let _ = quirks::set_port(u, v); })),
    ("pathname", |u| quirks::pathname(u).to_owned(), Some(quirks::set_pathname)),
    ("search", |u| quirks::search(u).to_owned(), Some(quirks::set_search)),
    ("hash", |u| quirks::hash(u).to_owned(), Some(quirks::set_hash)),
];

impl Class for WebUrl {
    const NAME: &'static str = "URL";
    const LENGTH: usize = 1;

    fn data_constructor(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let input = arg(args, 0, cx)?;
        let base = match args.get(1).filter(|b| !b.is_undefined()) {
            Some(b) => Some(b.to_string(cx)?.to_std_string_escaped()),
            None => None,
        };
        let url = parse(&input, base.as_deref()).ok_or_else(|| type_error(&format!("Invalid URL: {input}")))?;
        Ok(WebUrl { url: RefCell::new(url), params: GcRefCell::new(None) })
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let realm = class.context().realm().clone();
        for (i, &(name, _, setter)) in COMPONENTS.iter().enumerate() {
            // Captures the component's index (closures over a `usize`).
            let get = NativeFunction::from_copy_closure(move |this, _, _| {
                let o = this_object::<WebUrl>(this)?;
                let u = o.downcast_ref::<WebUrl>().expect("checked");
                Ok(js(&(COMPONENTS[i].1)(&u.url.borrow())))
            })
            .to_js_function(&realm);
            let set = match (name, setter) {
                ("href", _) => Some(
                    NativeFunction::from_copy_closure(move |this, args, cx| {
                        let value = arg(args, 0, cx)?;
                        let o = this_object::<WebUrl>(this)?;
                        let u = o.downcast_ref::<WebUrl>().expect("checked");
                        let url = Url::parse(&value).map_err(|_| type_error(&format!("Invalid URL: {value}")))?;
                        *u.url.borrow_mut() = url;
                        u.query_changed();
                        Ok(JsValue::undefined())
                    })
                    .to_js_function(&realm),
                ),
                (_, Some(_)) => Some(
                    NativeFunction::from_copy_closure(move |this, args, cx| {
                        let value = arg(args, 0, cx)?;
                        let o = this_object::<WebUrl>(this)?;
                        let u = o.downcast_ref::<WebUrl>().expect("checked");
                        if let Some(set) = COMPONENTS[i].2 {
                            set(&mut u.url.borrow_mut(), &value);
                        }
                        if COMPONENTS[i].0 == "search" {
                            u.query_changed();
                        }
                        Ok(JsValue::undefined())
                    })
                    .to_js_function(&realm),
                ),
                _ => None,
            };
            class.accessor(JsString::from(name), Some(get), set, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        }
        let search_params = NativeFunction::from_fn_ptr(|this, _, cx| {
            let o = this_object::<WebUrl>(this)?;
            let u = o.downcast_ref::<WebUrl>().expect("checked");
            if let Some(p) = &*u.params.borrow() {
                return Ok(p.clone().into());
            }
            let list = parse_query(quirks::search(&u.url.borrow()));
            let params = WebSearchParams { list: RefCell::new(list), owner: GcRefCell::new(Some(o.clone())) };
            let p = WebSearchParams::from_data(params, cx)?;
            *u.params.borrow_mut() = Some(p.clone());
            Ok(p.into())
        })
        .to_js_function(&realm);
        class.accessor(js_string!("searchParams"), Some(search_params), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        let href = NativeFunction::from_fn_ptr(|this, _, _| {
            let o = this_object::<WebUrl>(this)?;
            let u = o.downcast_ref::<WebUrl>().expect("checked");
            Ok(js(quirks::href(&u.url.borrow())))
        });
        class.method(js_string!("toString"), 0, href.clone());
        class.method(js_string!("toJSON"), 0, href);
        class.static_method(
            js_string!("canParse"),
            1,
            NativeFunction::from_fn_ptr(|_, args, cx| {
                let input = arg(args, 0, cx)?;
                let base = match args.get(1).filter(|b| !b.is_undefined()) {
                    Some(b) => Some(b.to_string(cx)?.to_std_string_escaped()),
                    None => None,
                };
                Ok(JsValue::from(parse(&input, base.as_deref()).is_some()))
            }),
        );
        class.static_method(
            js_string!("parse"),
            1,
            NativeFunction::from_fn_ptr(|_, args, cx| {
                let input = arg(args, 0, cx)?;
                let base = match args.get(1).filter(|b| !b.is_undefined()) {
                    Some(b) => Some(b.to_string(cx)?.to_std_string_escaped()),
                    None => None,
                };
                match parse(&input, base.as_deref()) {
                    Some(url) => Ok(WebUrl::from_data(WebUrl { url: RefCell::new(url), params: GcRefCell::new(None) }, cx)?.into()),
                    None => Ok(JsValue::null()),
                }
            }),
        );
        // Object URLs for blobs (workers load scripts from them).
        class.static_method(js_string!("createObjectURL"), 1, NativeFunction::from_fn_ptr(|_, a, cx| {
            let origin = crate::dom::resolve(cx, "/").map(|u| u.origin().ascii_serialization()).unwrap_or_else(|| "null".into());
            let o = a.first().and_then(JsValue::as_object);
            let url = match o {
                Some(o) => crate::media_source::object_url(&o, &origin, cx).or_else(|| crate::blob::object_url(&o, &origin)),
                None => None,
            };
            Ok(js(url.as_deref().unwrap_or("blob:null/unknown")))
        }));
        class.static_method(js_string!("revokeObjectURL"), 1, NativeFunction::from_fn_ptr(|_, a, cx| {
            let url = a.first().cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
            crate::blob::revoke_object_url(&url);
            crate::media_source::revoke(&url, cx);
            Ok(JsValue::undefined())
        }));
        Ok(())
    }
}

/// The list of a `URLSearchParams` method's `this`.
fn with_list<R>(this: &JsValue, f: impl FnOnce(&WebSearchParams) -> R) -> JsResult<R> {
    let o = this_object::<WebSearchParams>(this)?;
    let p = o.downcast_ref::<WebSearchParams>().expect("checked");
    Ok(f(&p))
}

/// An iterator over `items` (an array's, so `for…of` and spreading work).
fn iterate(items: Vec<JsValue>, cx: &mut Context) -> JsResult<JsValue> {
    let array = JsArray::from_iter(items, cx);
    let values = array.get(js_string!("values"), cx)?;
    let values = values.as_callable().ok_or_else(|| type_error("no Array.prototype.values"))?;
    values.call(&array.into(), &[], cx)
}

impl Class for WebSearchParams {
    const NAME: &'static str = "URLSearchParams";

    fn data_constructor(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let init = args.get_or_undefined(0);
        let mut list = Vec::new();
        if let Some(o) = init.as_object() {
            if let Some(p) = o.downcast_ref::<WebSearchParams>() {
                list = p.list.borrow().clone();
            } else {
                // A sequence of pairs (anything iterable), else a record.
                let iterable = o.has_property(boa_engine::JsSymbol::iterator(), cx)?;
                let pairs: Vec<JsValue> = if iterable {
                    let array_from = cx.global_object().get(js_string!("Array"), cx)?;
                    let from = array_from.as_object().map(|a| a.get(js_string!("from"), cx)).transpose()?.unwrap_or_default();
                    let arr = from.as_callable().ok_or_else(|| type_error("no Array.from"))?.call(&array_from, &[init.clone()], cx)?;
                    let arr = JsArray::from_object(arr.as_object().ok_or_else(|| type_error("not iterable"))?.clone())?;
                    let len = arr.length(cx)?;
                    (0..len).map(|i| arr.get(i, cx)).collect::<JsResult<_>>()?
                } else {
                    let keys = o.own_property_keys(cx)?;
                    let mut out = Vec::new();
                    for key in keys {
                        let name = JsValue::from(match &key {
                            boa_engine::property::PropertyKey::String(s) => s.clone(),
                            boa_engine::property::PropertyKey::Index(i) => JsString::from(i.get().to_string()),
                            boa_engine::property::PropertyKey::Symbol(_) => continue,
                        });
                        let value = o.get(key, cx)?;
                        out.push(JsArray::from_iter([name, value], cx).into());
                    }
                    out
                };
                for pair in pairs {
                    let pair = pair.as_object().ok_or_else(|| type_error("Each pair must be an iterable [name, value]"))?;
                    let k = pair.get(0, cx)?.to_string(cx)?.to_std_string_escaped();
                    let v = pair.get(1, cx)?.to_string(cx)?.to_std_string_escaped();
                    list.push((k, v));
                }
            }
        } else if !init.is_undefined() && !init.is_null() {
            list = parse_query(&init.to_string(cx)?.to_std_string_escaped());
        }
        Ok(WebSearchParams { list: RefCell::new(list), owner: GcRefCell::new(None) })
    }

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        class.method(
            js_string!("append"),
            2,
            NativeFunction::from_fn_ptr(|this, args, cx| {
                let (k, v) = (arg(args, 0, cx)?, arg(args, 1, cx)?);
                with_list(this, |p| {
                    p.list.borrow_mut().push((k, v));
                    p.changed();
                })?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            js_string!("delete"),
            1,
            NativeFunction::from_fn_ptr(|this, args, cx| {
                let k = arg(args, 0, cx)?;
                let v = match args.get(1).filter(|v| !v.is_undefined()) {
                    Some(v) => Some(v.to_string(cx)?.to_std_string_escaped()),
                    None => None,
                };
                with_list(this, |p| {
                    p.list.borrow_mut().retain(|(a, b)| *a != k || v.as_ref().is_some_and(|v| v != b));
                    p.changed();
                })?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            js_string!("get"),
            1,
            NativeFunction::from_fn_ptr(|this, args, cx| {
                let k = arg(args, 0, cx)?;
                with_list(this, |p| p.list.borrow().iter().find(|(a, _)| *a == k).map_or(JsValue::null(), |(_, v)| js(v)))
            }),
        );
        class.method(
            js_string!("getAll"),
            1,
            NativeFunction::from_fn_ptr(|this, args, cx| {
                let k = arg(args, 0, cx)?;
                let values: Vec<JsValue> = with_list(this, |p| p.list.borrow().iter().filter(|(a, _)| *a == k).map(|(_, v)| js(v)).collect())?;
                Ok(JsArray::from_iter(values, cx).into())
            }),
        );
        class.method(
            js_string!("has"),
            1,
            NativeFunction::from_fn_ptr(|this, args, cx| {
                let k = arg(args, 0, cx)?;
                let v = match args.get(1).filter(|v| !v.is_undefined()) {
                    Some(v) => Some(v.to_string(cx)?.to_std_string_escaped()),
                    None => None,
                };
                with_list(this, |p| JsValue::from(p.list.borrow().iter().any(|(a, b)| *a == k && v.as_ref().is_none_or(|v| v == b))))
            }),
        );
        class.method(
            js_string!("set"),
            2,
            NativeFunction::from_fn_ptr(|this, args, cx| {
                let (k, v) = (arg(args, 0, cx)?, arg(args, 1, cx)?);
                with_list(this, |p| {
                    let mut list = p.list.borrow_mut();
                    match list.iter().position(|(a, _)| *a == k) {
                        Some(first) => {
                            list[first].1 = v;
                            let mut ix = 0;
                            list.retain(|(a, _)| {
                                let keep = *a != k || ix == first;
                                ix += 1;
                                keep
                            });
                        }
                        None => list.push((k, v)),
                    }
                    drop(list);
                    p.changed();
                })?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            js_string!("sort"),
            0,
            NativeFunction::from_fn_ptr(|this, _, _| {
                with_list(this, |p| {
                    // By name, in UTF-16 code units, keeping equal names in order.
                    p.list.borrow_mut().sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
                    p.changed();
                })?;
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            js_string!("toString"),
            0,
            NativeFunction::from_fn_ptr(|this, _, _| with_list(this, |p| js(&serialize(&p.list.borrow())))),
        );
        class.method(
            js_string!("forEach"),
            1,
            NativeFunction::from_fn_ptr(|this, args, cx| {
                let f = args.get_or_undefined(0).as_callable().ok_or_else(|| type_error("forEach needs a function"))?;
                let this_arg = args.get_or_undefined(1).clone();
                let list = with_list(this, |p| p.list.borrow().clone())?;
                for (k, v) in list {
                    f.call(&this_arg, &[js(&v), js(&k), this.clone()], cx)?;
                }
                Ok(JsValue::undefined())
            }),
        );
        class.method(
            js_string!("keys"),
            0,
            NativeFunction::from_fn_ptr(|this, _, cx| {
                let items = with_list(this, |p| p.list.borrow().iter().map(|(k, _)| js(k)).collect())?;
                iterate(items, cx)
            }),
        );
        class.method(
            js_string!("values"),
            0,
            NativeFunction::from_fn_ptr(|this, _, cx| {
                let items = with_list(this, |p| p.list.borrow().iter().map(|(_, v)| js(v)).collect())?;
                iterate(items, cx)
            }),
        );
        let entries = NativeFunction::from_fn_ptr(|this, _, cx| {
            let list = with_list(this, |p| p.list.borrow().clone())?;
            let items = list.iter().map(|(k, v)| JsArray::from_iter([js(k), js(v)], cx).into()).collect();
            iterate(items, cx)
        });
        class.method(js_string!("entries"), 0, entries.clone());
        class.method(boa_engine::JsSymbol::iterator(), 0, entries);
        let realm = class.context().realm().clone();
        let size = NativeFunction::from_fn_ptr(|this, _, _| with_list(this, |p| JsValue::from(p.list.borrow().len() as u32)))
            .to_js_function(&realm);
        class.accessor(js_string!("size"), Some(size), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        Ok(())
    }
}

/// Make `URL` and `URLSearchParams` globals.
pub(crate) fn register(cx: &mut Context) {
    if let Err(e) = cx.register_global_class::<WebUrl>() {
        eprintln!("js: URL class: {e}");
    }
    if let Err(e) = cx.register_global_class::<WebSearchParams>() {
        eprintln!("js: URLSearchParams class: {e}");
    }
}
