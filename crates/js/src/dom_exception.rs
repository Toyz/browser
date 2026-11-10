//! `DOMException`, native: a name, a message and the legacy numeric code,
//! an `Error` (its prototype's prototype is `Error.prototype`).

use boa_engine::class::{Class, ClassBuilder};
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsData, JsNativeError, JsResult, JsString, JsValue, NativeFunction};
use boa_gc::{Finalize, Trace};

use crate::dom::js_value_string;

/// The legacy codes, by name (and their constants).
const CODES: &[(&str, &str, u16)] = &[
    ("IndexSizeError", "INDEX_SIZE_ERR", 1),
    ("HierarchyRequestError", "HIERARCHY_REQUEST_ERR", 3),
    ("WrongDocumentError", "WRONG_DOCUMENT_ERR", 4),
    ("InvalidCharacterError", "INVALID_CHARACTER_ERR", 5),
    ("NoModificationAllowedError", "NO_MODIFICATION_ALLOWED_ERR", 7),
    ("NotFoundError", "NOT_FOUND_ERR", 8),
    ("NotSupportedError", "NOT_SUPPORTED_ERR", 9),
    ("InvalidStateError", "INVALID_STATE_ERR", 11),
    ("SyntaxError", "SYNTAX_ERR", 12),
    ("InvalidModificationError", "INVALID_MODIFICATION_ERR", 13),
    ("NamespaceError", "NAMESPACE_ERR", 14),
    ("InvalidAccessError", "INVALID_ACCESS_ERR", 15),
    ("TypeMismatchError", "TYPE_MISMATCH_ERR", 17),
    ("SecurityError", "SECURITY_ERR", 18),
    ("NetworkError", "NETWORK_ERR", 19),
    ("AbortError", "ABORT_ERR", 20),
    ("URLMismatchError", "URL_MISMATCH_ERR", 21),
    ("QuotaExceededError", "QUOTA_EXCEEDED_ERR", 22),
    ("TimeoutError", "TIMEOUT_ERR", 23),
    ("InvalidNodeTypeError", "INVALID_NODE_TYPE_ERR", 24),
    ("DataCloneError", "DATA_CLONE_ERR", 25),
];

#[derive(Trace, Finalize, JsData)]
struct DomException {
    #[unsafe_ignore_trace]
    name: String,
    #[unsafe_ignore_trace]
    message: String,
}

fn field(this: &JsValue, f: fn(&DomException) -> JsValue) -> JsResult<JsValue> {
    let o = this.as_object().ok_or_else(|| JsNativeError::typ().with_message("not a DOMException"))?;
    let e = o.downcast_ref::<DomException>().ok_or_else(|| JsNativeError::typ().with_message("not a DOMException"))?;
    Ok(f(&e))
}

impl Class for DomException {
    const NAME: &'static str = "DOMException";

    fn init(class: &mut ClassBuilder<'_>) -> JsResult<()> {
        let getters: [(&str, fn(&DomException) -> JsValue); 3] = [
            ("name", |e| JsString::from(e.name.as_str()).into()),
            ("message", |e| JsString::from(e.message.as_str()).into()),
            ("code", |e| JsValue::from(CODES.iter().find(|(n, _, _)| *n == e.name).map_or(0, |c| c.2))),
        ];
        for (name, f) in getters {
            let realm = class.context().realm().clone();
            let get = NativeFunction::from_copy_closure(move |this, _, _| field(this, f));
            let get = FunctionObjectBuilder::new(&realm, get).name(JsString::from(name)).build();
            class.accessor(JsString::from(name), Some(get), None, Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        }
        for &(_, constant, code) in CODES {
            class.static_property(JsString::from(constant), code, Attribute::empty());
            class.property(JsString::from(constant), code, Attribute::empty());
        }
        Ok(())
    }

    fn data_constructor(_: &JsValue, a: &[JsValue], cx: &mut Context) -> JsResult<Self> {
        let text = |v: Option<&JsValue>, default: &str, cx: &mut Context| match v.filter(|v| !v.is_undefined()) {
            Some(v) => js_value_string(v, cx),
            None => default.to_owned(),
        };
        Ok(DomException { message: text(a.first(), "", cx), name: text(a.get(1), "Error", cx) })
    }
}

/// Register `DOMException`, an `Error`.
pub(crate) fn install(cx: &mut Context) {
    if let Err(e) = cx.register_global_class::<DomException>() {
        eprintln!("js: DOMException: {e}");
        return;
    }
    if let Some(class) = cx.get_global_class::<DomException>() {
        let error = cx.intrinsics().constructors().error().prototype();
        class.prototype().set_prototype(Some(error));
    }
}
