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
    /// Imports being fetched ahead (in parallel, as soon as the module
    /// importing them is parsed), by URL.
    prefetched: RefCell<HashMap<String, std::sync::mpsc::Receiver<Option<String>>>>,
}

/// The specifiers of a module's static imports and re-exports
/// (`import x from "./a.js"`, `import "./b.js"`, `export * from "./c.js"`),
/// found by a quick scan (it may find a few in strings or comments too;
/// those are only fetched early, never run).
pub(crate) fn static_imports(code: &str) -> Vec<&str> {
    fn search<'a>(code: &'a str, keyword: &str, out: &mut Vec<&'a str>) {
        let bytes = code.as_bytes();
        let mut from = 0;
        while let Some(i) = code[from..].find(keyword) {
            let at = from + i;
            from = at + keyword.len();
            // A whole word.
            if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_' || bytes[at - 1] == b'$' || bytes[at - 1] == b'.') {
                continue;
            }
            let rest = code[from..].trim_start();
            let rest = rest.strip_prefix('(').unwrap_or(rest);
            let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
            let body = &rest[1..];
            if let Some(end) = body.find(quote) {
                let spec = &body[..end];
                let pathlike = spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') || spec.starts_with("http");
                if pathlike && spec.len() < 300 && !spec.contains('\n') {
                    out.push(spec);
                }
            }
        }
    }
    let mut out = Vec::new();
    search(code, "from", &mut out);
    search(code, "import", &mut out);
    out.sort_unstable();
    out.dedup();
    out
}

impl WebModules {
    pub fn new(fetch: FetchText, base: Url, debug: bool) -> WebModules {
        WebModules { fetch, base, debug, cache: RefCell::default(), prefetched: RefCell::default() }
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
        self.prefetch_imports(code, url);
        let path = url.as_str().to_owned();
        let source = Source::from_bytes(code).with_path(Path::new(&path));
        Module::parse(source, None, cx)
    }

    /// A `<script type="module" src>`: one instance per URL, shared with
    /// `import`s of the same URL (a page's entry module is often imported
    /// again by its lazy chunks, and two copies would split its state).
    pub fn entry(&self, code: &str, url: &Url, cx: &mut Context) -> JsResult<Module> {
        if let Some(m) = self.cache.borrow().get(url.as_str()) {
            return Ok(m.clone());
        }
        let module = self.parse(code, url, cx)?;
        self.cache.borrow_mut().insert(url.to_string(), module.clone());
        Ok(module)
    }

    /// Start fetching what `code` imports, all at once (the loader then
    /// asks for them one by one, each a network round trip otherwise).
    fn prefetch_imports(&self, code: &str, url: &Url) {
        for spec in static_imports(code) {
            let relative = spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/');
            let Some(target) = (if relative { url.join(spec).ok() } else { None }) else { continue };
            if !matches!(target.scheme(), "http" | "https") {
                continue;
            }
            let key = target.to_string();
            if self.cache.borrow().contains_key(&key) || self.prefetched.borrow().contains_key(&key) {
                continue;
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let fetch = self.fetch.clone();
            let spawned = std::thread::Builder::new().name("feather-js-prefetch".into()).spawn(move || {
                tx.send(fetch(&target)).ok();
            });
            if spawned.is_ok() {
                self.prefetched.borrow_mut().insert(key, rx);
            }
        }
    }

    fn load(&self, url: Url, cx: &mut Context) -> JsResult<Module> {
        if let Some(m) = self.cache.borrow().get(url.as_str()) {
            return Ok(m.clone());
        }
        let t = std::time::Instant::now();
        let ahead = self.prefetched.borrow_mut().remove(url.as_str());
        let code = match ahead.and_then(|rx| rx.recv().ok()) {
            Some(code) => code,
            None => (self.fetch)(&url),
        };
        let code = code.ok_or_else(|| JsNativeError::typ().with_message(format!("failed to fetch module {url}")))?;
        let module = self.parse(&code, &url, cx)?;
        if self.debug {
            eprintln!("js: module {url} ({} bytes) in {:?}", code.len(), t.elapsed());
        }
        self.cache.borrow_mut().insert(url.to_string(), module.clone());
        Ok(module)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_static_imports() {
        let code = r#"import{a as b}from"./index-X.js";import"./tip.js";export*from './c.js';const s="from";x.import("nope");import('./lazy.js')"#;
        assert_eq!(super::static_imports(code), vec!["./c.js", "./index-X.js", "./lazy.js", "./tip.js"]);
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
