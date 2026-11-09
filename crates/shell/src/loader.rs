//! Loading a page: fetch → parse → style → boxes, off the UI thread.

use std::sync::Arc;

use style::ElementState;
use url::Url;

use render::LayoutTree;

/// A fetched, parsed and laid-out page. Cheap to clone (it's all `Arc`s).
#[derive(Clone)]
pub struct LoadedPage {
    pub url: Url,
    pub title: Option<String>,
    pub layout: Arc<LayoutTree>,
    /// Kept so the page can be restyled (window resizes, hover, later: JS).
    pub doc: Option<Arc<page::Document>>,
    pub styles: Option<style::StyleData>,
    /// Which media conditions held when `layout` was built.
    pub signature: Vec<bool>,
    /// Where the tab icon comes from.
    pub favicon: Option<Url>,
    /// `<meta http-equiv="refresh" content="N;url=...">`: go there after N seconds.
    pub refresh: Option<(f32, Url)>,
    /// Source code shown in a code editor instead (CSS, JS, JSON responses;
    /// `view-source:`): (language, text).
    pub source: Option<(&'static str, Arc<str>)>,
}

/// The language to highlight a non-HTML response as, if it's code.
fn code_language(content_type: Option<&str>, url: &Url) -> Option<&'static str> {
    let ct = content_type.unwrap_or_default().to_ascii_lowercase();
    let ext = url.path().rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    if ct.contains("json") || ext == "json" {
        Some("json")
    } else if ct.contains("css") || ext == "css" {
        Some("css")
    } else if ct.contains("javascript") || ct.contains("ecmascript") || matches!(ext.as_str(), "js" | "mjs" | "cjs") {
        Some("javascript")
    } else {
        None
    }
}

impl LoadedPage {
    /// The element `#fragment` points at, for `:target`.
    pub fn target(&self) -> Option<page::NodeId> {
        let doc = self.doc.as_ref()?;
        find_id(doc, self.url.fragment()?)
    }
}

fn find_id(doc: &page::Document, id: &str) -> Option<page::NodeId> {
    doc.descendants(doc.root()).find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some(id))
}

/// May a page from `page` load the subresource `url`? Web pages don't get
/// to read local files.
pub fn may_load(page: &Url, url: &Url) -> bool {
    url.scheme() != "file" || page.scheme() == "file"
}

/// Fetch → parse → style → build boxes. Blocking: run it in the background.
pub fn load(url: &Url, env: style::MediaEnv, post: Option<&str>) -> anyhow::Result<LoadedPage> {
    if url.scheme() == "view-source" {
        let inner = Url::parse(url.path()).map_err(|e| anyhow::anyhow!("bad view-source URL: {e}"))?;
        let response = net::fetch(&inner)?;
        return Ok(LoadedPage {
            favicon: default_favicon(&response.url),
            refresh: None,
            title: Some(format!("view-source:{}", response.url)),
            url: url.clone(),
            layout: Arc::new(LayoutTree::default()),
            doc: None,
            styles: None,
            signature: Vec::new(),
            source: Some(("html", response.body.into())),
        });
    }
    let response = match post {
        Some(body) => net::fetch_post(url, body)?,
        None => net::fetch(url)?,
    };
    let is_html = match &response.content_type {
        Some(ct) => ct.contains("html"),
        None => response.body.trim_start().starts_with('<'),
    };
    if !is_html {
        return Ok(LoadedPage {
            favicon: default_favicon(&response.url),
            refresh: None,
            source: code_language(response.content_type.as_deref(), &response.url).map(|l| (l, Arc::from(&*response.body))),
            url: response.url,
            title: None,
            layout: Arc::new(render::build_plain_text(&response.body)),
            doc: None,
            styles: None,
            signature: Vec::new(),
        });
    }
    let doc = html::parse(&response.body, response.url.clone());
    let page_url = response.url.clone();
    let fetch_text = move |url: &Url| may_load(&page_url, url).then(|| net::fetch_text_cached(url)).flatten();
    let styles = style::StyleData::for_document(&doc, &fetch_text);
    // `:target` from the requested URL's fragment.
    let target = url.fragment().and_then(|f| find_id(&doc, f));
    let state = Arc::new(ElementState { target, ..Default::default() });
    let layout = render::build(&doc, &styles.styler_with_state(env, state));
    Ok(LoadedPage {
        favicon: favicon(&doc).or_else(|| default_favicon(&response.url)),
        refresh: meta_refresh(&doc),
        source: None,
        url: response.url,
        title: doc.title(),
        layout: Arc::new(layout),
        signature: styles.signature(&env),
        doc: Some(Arc::new(doc)),
        styles: Some(styles),
    })
}

/// `<meta http-equiv="refresh" content="0;url=https://...">`
fn meta_refresh(doc: &page::Document) -> Option<(f32, Url)> {
    let el = doc.descendants(doc.root()).filter_map(|n| doc.element(n)).find(|e| {
        &**e.local_name() == "meta" && e.attr("http-equiv").is_some_and(|v| v.eq_ignore_ascii_case("refresh"))
    })?;
    let content = el.attr("content")?;
    let (secs, rest) = content.split_once([';', ',']).unwrap_or((content, ""));
    let secs: f32 = secs.trim().parse().ok()?;
    let rest = rest.trim();
    let target = rest
        .strip_prefix("url=")
        .or_else(|| rest.strip_prefix("URL="))
        .or_else(|| rest.strip_prefix("Url="))
        .unwrap_or(rest)
        .trim()
        .trim_matches(['\'', '"']);
    // No URL: reload the page itself (skip that; it's a loop).
    if target.is_empty() {
        return None;
    }
    Some((secs, doc.resolve_url(target)?))
}

/// `<link rel="icon">`, preferring small raster or SVG icons over huge
/// touch icons.
fn favicon(doc: &page::Document) -> Option<Url> {
    let mut best: Option<(i32, Url)> = None;
    for id in doc.descendants(doc.root()) {
        let Some(el) = doc.element(id) else { continue };
        if &**el.local_name() != "link" {
            continue;
        }
        let rel = el.attr("rel").unwrap_or_default().to_ascii_lowercase();
        let rels: Vec<&str> = rel.split_whitespace().collect();
        let Some(href) = el.attr("href") else { continue };
        let score = if rels.contains(&"icon") {
            let sizes = el.attr("sizes").unwrap_or_default();
            let ty = el.attr("type").unwrap_or_default();
            if ty.contains("svg") || href.ends_with(".svg") {
                30
            } else if sizes.contains("32") || sizes.contains("16") {
                25
            } else {
                20
            }
        } else if rels.contains(&"apple-touch-icon") {
            10
        } else {
            continue;
        };
        if best.as_ref().is_none_or(|(s, _)| score > *s)
            && let Some(url) = doc.resolve_url(href)
        {
            best = Some((score, url));
        }
    }
    best.map(|(_, url)| url)
}

fn default_favicon(url: &Url) -> Option<Url> {
    matches!(url.scheme(), "http" | "https").then(|| url.join("/favicon.ico").ok()).flatten()
}

/// Is `url` safe to load before it's clicked? GETs shouldn't have side
/// effects, but links like "log out" or "upvote" often do.
pub fn prefetchable(url: &Url) -> bool {
    const ACTIONS: &[&str] =
        &["logout", "log_out", "log-out", "signout", "sign_out", "sign-out", "delete", "remove", "vote", "hide", "unsubscribe", "flag", "auth"];
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let tail = format!("{}?{}", url.path(), url.query().unwrap_or_default()).to_ascii_lowercase();
    !ACTIONS.iter().any(|a| tail.contains(a))
}

/// Run `f` on its own thread with a big stack: styling and box building
/// recurse once per level of the page's element tree, and real pages nest
/// deeper than an executor thread's stack allows.
pub fn deep<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = T> {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::Builder::new()
        .name("feather-layout".into())
        .stack_size(64 << 20)
        .spawn(move || {
            tx.send(f()).ok();
        })
        .expect("can't spawn layout thread");
    async move { rx.await.expect("layout thread panicked") }
}

/// Same document, ignoring the fragment?
pub fn same_document(a: &Url, b: &Url) -> bool {
    a.as_str().split('#').next() == b.as_str().split('#').next()
}
