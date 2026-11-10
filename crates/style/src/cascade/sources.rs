//! Where styles come from: the UA sheet, a document's `<style>`/`<link>` sheets and `@import`s, and presentational attributes.

use super::*;

pub(super) const UA_CSS: &str = include_str!("../ua.css");

/// Extra UA rules for quirks-mode documents (HTML spec, rendering § quirks).
pub(super) const QUIRKS_CSS: &str = "
table {
  font-weight: initial; font-style: initial; font-size: initial;
  line-height: initial; white-space: initial; text-align: initial;
}
";

/// Limits so a hostile page can't make us fetch forever.
pub(super) const MAX_EXTERNAL_SHEETS: usize = 128;

pub(super) const MAX_IMPORT_DEPTH: usize = 3;

/// A stylesheet referenced by the document, in document order.
#[derive(Debug, Clone, PartialEq)]
pub enum StyleSource {
    /// `scope`: the shadow tree (by host) it styles; `None`, the document.
    Inline { css: String, media: String, scope: Option<NodeId> },
    External { url: Url, media: String, scope: Option<NodeId> },
}

/// Find `<style>` and `<link rel=stylesheet>` in document order (each
/// for the tree it's in), then constructed (`adoptedStyleSheets`) ones.
pub fn collect_style_sources(doc: &Document) -> Vec<StyleSource> {
    let mut out = Vec::new();
    for id in doc.descendants(doc.root()) {
        let Some(e) = doc.element(id) else { continue };
        if !e.is_html() {
            continue;
        }
        let media = e.attr("media").unwrap_or("").to_owned();
        let scope = doc.tree_of(id);
        match &*e.name.local {
            "style" => out.push(StyleSource::Inline { css: doc.text_content(id), media, scope }),
            "link" => {
                let rel = e.attr("rel").unwrap_or("").to_ascii_lowercase();
                let is_sheet = rel.split_ascii_whitespace().any(|r| r == "stylesheet");
                let alternate = rel.split_ascii_whitespace().any(|r| r == "alternate");
                if is_sheet && !alternate && e.attr("disabled").is_none()
                    && let Some(url) = e.attr("href").and_then(|h| doc.resolve_url(h))
                {
                    out.push(StyleSource::External { url, media, scope });
                }
            }
            _ => {}
        }
    }
    if let Some(shadow) = &doc.shadow {
        for (scope, css) in &shadow.adopted {
            out.push(StyleSource::Inline { css: css.clone(), media: String::new(), scope: *scope });
        }
    }
    out
}

pub(super) fn source_scope(s: &StyleSource) -> Option<NodeId> {
    match s {
        StyleSource::Inline { scope, .. } | StyleSource::External { scope, .. } => *scope,
    }
}

pub(super) fn source_media(s: &StyleSource) -> &str {
    match s {
        StyleSource::Inline { media, .. } | StyleSource::External { media, .. } => media,
    }
}

/// Parse a sheet and (recursively) the sheets it `@import`s, imports first.
/// A stylesheet's text parsed (with its `@import`s), shared by every
/// styling of every document that has it: scripts adding a `<style>`
/// re-styles the page with the other sheets' parse, and the tab and the
/// page's scripts (getComputedStyle) share one parse.
pub(super) fn parsed_sheets(css: &str, base: &Url, media: &str, fetch: &(dyn Fn(&Url) -> Option<String> + Sync)) -> Vec<Stylesheet> {
    use std::hash::{Hash, Hasher};
    use std::sync::{Arc, Mutex, OnceLock};
    type Parsed = std::collections::HashMap<u64, (Arc<Vec<Stylesheet>>, u64)>;
    static PARSED: OnceLock<Mutex<(Parsed, u64)>> = OnceLock::new();
    const KEEP: usize = 1024;
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (css, base.as_str(), media).hash(&mut h);
        h.finish()
    };
    let cache = PARSED.get_or_init(Default::default);
    {
        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
        c.1 += 1;
        let now = c.1;
        if let Some((sheets, used)) = c.0.get_mut(&key) {
            *used = now;
            return (**sheets).clone();
        }
    }
    let sheets = load_sheet(css, base, media, fetch, 0);
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    if c.0.len() >= KEEP
        && let Some(oldest) = c.0.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| *k)
    {
        c.0.remove(&oldest);
    }
    let now = c.1;
    c.0.insert(key, (Arc::new(sheets.clone()), now));
    sheets
}

pub(super) fn load_sheet(
    css: &str,
    base: &Url,
    media: &str,
    fetch: &(dyn Fn(&Url) -> Option<String> + Sync),
    depth: usize,
) -> Vec<Stylesheet> {
    let mut sheet = crate::props::with_base(Some(base), || parse_stylesheet(css));
    let mut out = Vec::new();
    if depth < MAX_IMPORT_DEPTH {
        for (href, import_media) in &sheet.imports {
            if !media::can_ever_match(import_media) {
                continue;
            }
            if let Ok(url) = base.join(href)
                && let Some(css) = fetch(&url)
            {
                for mut imported in load_sheet(&css, &url, import_media, fetch, depth + 1) {
                    add_condition(&mut imported, media);
                    out.push(imported);
                }
            }
        }
    }
    add_condition(&mut sheet, media);
    out.push(sheet);
    out
}

/// Make every rule of `sheet` also depend on `media` (from `<link media>` etc).
pub(super) fn add_condition(sheet: &mut Stylesheet, media: &str) {
    let media = media.trim();
    if media.is_empty() || media.eq_ignore_ascii_case("all") || media.eq_ignore_ascii_case("screen") {
        return;
    }
    let condition: Arc<str> = media.into();
    for rule in &mut sheet.rules {
        let mut conditions = rule.media.to_vec();
        conditions.push(condition.clone());
        rule.media = conditions.into();
    }
}

/// HTML attributes that act as styles (`bgcolor`, `<font color>`, `align`...),
/// as a CSS declaration block.
pub(super) fn presentational_hints(e: &page::ElementData) -> String {
    let mut css = String::new();
    let tag = &*e.name.local;
    let color = |v: &str| -> String {
        let v = v.trim();
        if v.len() == 6 && v.chars().all(|c| c.is_ascii_hexdigit()) { format!("#{v}") } else { v.to_owned() }
    };
    if let Some(bg) = e.attr("bgcolor") {
        css.push_str(&format!("background-color:{};", color(bg)));
    }
    match tag {
        "font" => {
            if let Some(c) = e.attr("color") {
                css.push_str(&format!("color:{};", color(c)));
            }
            if let Some(size) = e.attr("size").map(str::trim) {
                let n: i32 = if let Some(rel) = size.strip_prefix('+') {
                    3 + rel.parse::<i32>().unwrap_or(0)
                } else if let Some(rel) = size.strip_prefix('-') {
                    3 - rel.parse::<i32>().unwrap_or(0)
                } else {
                    size.parse().unwrap_or(3)
                };
                let px = [10, 13, 16, 18, 24, 32, 48][(n.clamp(1, 7) - 1) as usize];
                css.push_str(&format!("font-size:{px}px;"));
            }
            if let Some(face) = e.attr("face") {
                css.push_str(&format!("font-family:{face};"));
            }
        }
        "body" => {
            if let Some(c) = e.attr("text") {
                css.push_str(&format!("color:{};", color(c)));
            }
        }
        "hr" => {
            if let Some(c) = e.attr("color") {
                css.push_str(&format!("border-color:{};", color(c)));
            }
        }
        _ => {}
    }
    if let Some(align) = e.attr("align").map(|a| a.trim().to_ascii_lowercase()) {
        match tag {
            "table" | "img" if align == "center" => css.push_str("margin-left:auto;margin-right:auto;"),
            "table" | "img" if align == "left" || align == "right" => css.push_str(&format!("float:{align};")),
            "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "td" | "th" | "tr" | "caption"
            | "thead" | "tbody" | "tfoot" | "center" => {
                if matches!(align.as_str(), "left" | "right" | "center" | "justify") {
                    css.push_str(&format!("text-align:{};", if align == "justify" { "left" } else { &align }));
                }
            }
            _ => {}
        }
    }
    if matches!(tag, "table" | "td" | "th" | "img" | "col" | "hr" | "iframe" | "canvas" | "video") {
        for (attr, prop) in [("width", "width"), ("height", "height")] {
            if let Some(v) = e.attr(attr).map(str::trim).filter(|v| !v.is_empty()) {
                if v.ends_with('%') {
                    css.push_str(&format!("{prop}:{v};"));
                } else if let Ok(n) = v.trim_end_matches("px").parse::<f32>() {
                    css.push_str(&format!("{prop}:{n}px;"));
                }
            }
        }
    }
    if matches!(tag, "td" | "th") && e.attr("nowrap").is_some() {
        css.push_str("white-space:nowrap;");
    }
    if matches!(tag, "ol" | "ul" | "li")
        && let Some(t) = e.attr("type")
    {
        let style = match t {
            "1" => "decimal",
            "a" => "lower-alpha",
            "A" => "upper-alpha",
            "i" => "lower-roman",
            "I" => "upper-roman",
            "disc" | "circle" | "square" | "none" => t,
            _ => "",
        };
        if !style.is_empty() {
            css.push_str(&format!("list-style-type:{style};"));
        }
    }
    css
}

pub(super) fn el_is_root(doc: &Document, id: NodeId) -> bool {
    doc.parent(id) == Some(doc.root())
}
