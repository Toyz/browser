//! ES modules: `<script type="module">`, `import`, and `import()`, fetched
//! over the network relative to the importing module.
//!
//! Boa names modules by path; ours are named by their URL, so a module's
//! "path" is its URL and relative specifiers resolve against it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use boa_engine::module::{ModuleLoader, ModuleRequest, Referrer};
use boa_engine::{Context, JsNativeError, JsObject, JsResult, JsString, Module, Source};
use url::Url;

use crate::FetchText;

pub struct WebModules {
    pub fetch: FetchText,
    /// What specifiers resolve against outside any module (classic scripts).
    pub base: Url,
    pub debug: bool,
    cache: RefCell<HashMap<String, Module>>,
}

impl WebModules {
    pub fn new(fetch: FetchText, base: Url, debug: bool) -> WebModules {
        WebModules { fetch, base, debug, cache: RefCell::default() }
    }

    /// The URL a module was loaded from.
    pub fn url_of(path: Option<&Path>) -> Option<Url> {
        Url::parse(path?.to_str()?).ok()
    }

    fn resolve(&self, referrer: &Referrer, specifier: &str) -> JsResult<Url> {
        let base = Self::url_of(referrer.path()).unwrap_or_else(|| self.base.clone());
        // Bare specifiers ("lit") need an import map, which we don't have.
        let relative = specifier.starts_with("./") || specifier.starts_with("../") || specifier.starts_with('/');
        let url = if relative { base.join(specifier).ok() } else { Url::parse(specifier).ok() };
        url.ok_or_else(|| JsNativeError::typ().with_message(format!("can't resolve module specifier \"{specifier}\"")).into())
    }

    /// Parse a module (from a `<script type="module">` or the network).
    pub fn parse(&self, code: &str, url: &Url, cx: &mut Context) -> JsResult<Module> {
        let path = url.as_str().to_owned();
        let source = Source::from_bytes(code).with_path(Path::new(&path));
        Module::parse(source, None, cx)
    }

    fn load(&self, url: Url, cx: &mut Context) -> JsResult<Module> {
        if let Some(m) = self.cache.borrow().get(url.as_str()) {
            return Ok(m.clone());
        }
        let t = std::time::Instant::now();
        let code = (self.fetch)(&url).ok_or_else(|| JsNativeError::typ().with_message(format!("failed to fetch module {url}")))?;
        let module = self.parse(&code, &url, cx)?;
        if self.debug {
            eprintln!("js: module {url} ({} bytes) in {:?}", code.len(), t.elapsed());
        }
        self.cache.borrow_mut().insert(url.to_string(), module.clone());
        Ok(module)
    }
}

impl ModuleLoader for WebModules {
    fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> impl Future<Output = JsResult<Module>> {
        let specifier = request.specifier().to_std_string_escaped();
        let result = self.resolve(&referrer, &specifier).and_then(|url| self.load(url, &mut context.borrow_mut()));
        std::future::ready(result)
    }

    fn init_import_meta(self: Rc<Self>, import_meta: &JsObject, module: &Module, context: &mut Context) {
        let url = Self::url_of(module.path()).unwrap_or_else(|| self.base.clone());
        import_meta.set(JsString::from("url"), JsString::from(url.as_str()), false, context).ok();
    }
}
