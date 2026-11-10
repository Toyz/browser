//! `getComputedStyle()`: the style engine's computed values for an element,
//! as the CSS strings scripts read (`"block"`, `"16px"`, `"rgb(0, 0, 0)"`).
//! Layout-dependent ones (`width`, `height`) come from where the tab laid the
//! element out, when it has.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use page::{Document, NodeId};
use style::{Color, ComputedStyle, Length, MediaEnv, StyleData, Styler};
use url::Url;

/// The document's parsed stylesheets and the styles computed from them,
/// kept while the DOM and the viewport don't change.
#[derive(Default)]
pub(crate) struct StyleCache {
    /// What the sheets were made of (see `sheets_key`).
    key: u64,
    styler: Option<Styler>,
    data: Option<StyleData>,
    /// The DOM generation and viewport the computed styles are for.
    generation: u64,
    env: (u32, u32, bool),
    styles: HashMap<NodeId, Arc<ComputedStyle>>,
}

impl StyleCache {
    pub fn style_of(
        &mut self,
        doc: &Document,
        id: NodeId,
        generation: u64,
        env: MediaEnv,
        fetch: &(dyn Fn(&Url) -> Option<String> + Sync),
        cssom: &HashMap<NodeId, String>,
    ) -> Option<Arc<ComputedStyle>> {
        doc.element(id)?;
        let env_key = (env.width as u32, env.height as u32, env.dark);
        if generation != self.generation || env_key != self.env {
            self.styles.clear();
            self.generation = generation;
            let key = sheets_key(doc, cssom);
            if self.data.is_none() || key != self.key {
                // `<style>` sheets scripts changed through the CSSOM count as
                // what they hold now (CSS-in-JS: styled-components empties
                // its tags and inserts every rule).
                self.data = Some(StyleData::for_document_with(doc, fetch, &|n| cssom.get(&n).cloned()));
                self.key = key;
                self.styler = None;
            }
            if self.styler.is_none() || env_key != self.env {
                self.styler = self.data.as_ref().map(|d| d.styler(env));
            }
            self.env = env_key;
        }
        let styler = self.styler.as_ref()?;
        // Root first: each element's style inherits from its parent's.
        let mut chain = Vec::new();
        let mut node = Some(id);
        while let Some(n) = node {
            if self.styles.contains_key(&n) {
                break;
            }
            if doc.element(n).is_some() {
                chain.push(n);
            }
            node = doc.parent(n);
        }
        let mut parent = node.and_then(|n| self.styles.get(&n).cloned()).unwrap_or_else(|| Arc::new(ComputedStyle::initial()));
        for &n in chain.iter().rev() {
            let style = Arc::new(styler.compute(doc, n, &parent));
            self.styles.insert(n, style.clone());
            parent = style;
        }
        self.styles.get(&id).cloned()
    }
}

/// What a document's stylesheets are made of, cheaply: each sheet's
/// element and size, and the adopted ones (re-parsed when this changes).
fn sheets_key(doc: &Document, cssom: &HashMap<NodeId, String>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut overridden: Vec<_> = cssom.iter().collect();
    overridden.sort_by_key(|(n, _)| **n);
    overridden.hash(&mut h);
    for n in doc.descendants(doc.root()) {
        let Some(e) = doc.element(n) else { continue };
        match &*e.name.local {
            "style" => (n, doc.text_content(n).len()).hash(&mut h),
            "link" => (n, e.attr("href"), e.attr("rel"), e.attr("media")).hash(&mut h),
            _ => {}
        }
    }
    if let Some(shadow) = &doc.shadow {
        shadow.adopted.hash(&mut h);
    }
    h.finish()
}

/// The value of `property` (kebab-case) as `getComputedStyle` reports it.
/// `size`: the element's laid-out border box, when there is one.
pub(crate) fn value(s: &ComputedStyle, property: &str, size: Option<(f32, f32)>, dark: bool) -> String {
    let px = |v: f32| format!("{}px", trim(v));
    let len = |l: Length| match l {
        Length::Px(v) => px(v),
        Length::Percent(p) => format!("{}%", trim(p)),
        Length::Auto => "auto".into(),
    };
    let side = |e: &style::Edges<Length>, name: &str| match name {
        "top" => len(e.top),
        "right" => len(e.right),
        "bottom" => len(e.bottom),
        _ => len(e.left),
    };
    let color = |c: Color| rgb(c, s.color, dark);
    // A border with no style has no width.
    let bw = |w: f32, st: style::BorderStyle| if matches!(st, style::BorderStyle::None) { 0.0 } else { w };
    let (bt, br, bb, bl) = (
        bw(s.border_width.top, s.border_style.top),
        bw(s.border_width.right, s.border_style.right),
        bw(s.border_width.bottom, s.border_style.bottom),
        bw(s.border_width.left, s.border_style.left),
    );
    let kebab = |d: &dyn std::fmt::Debug| kebab(&format!("{d:?}"));
    // The used content size, from the laid-out box.
    let content = |outer: f32, a: Length, b: Length, ba: f32, bb: f32| {
        if s.border_box { outer } else { outer - a.px_or_zero() - b.px_or_zero() - ba - bb }
    };
    if let Some(name) = property.strip_prefix("--") {
        return s.custom.get(format!("--{name}").as_str()).map(|v| v.to_string()).unwrap_or_default();
    }
    match property {
        "display" => kebab(&s.display),
        "position" => kebab(&s.position),
        "float" => kebab(&s.float),
        "visibility" => if s.visible { "visible" } else { "hidden" }.into(),
        "opacity" => trim(s.opacity),
        "z-index" => s.z_index.map_or("auto".into(), |z| z.to_string()),
        "overflow" | "overflow-x" | "overflow-y" => kebab(&s.overflow),
        "box-sizing" => if s.border_box { "border-box" } else { "content-box" }.into(),
        "width" => match (size, s.display) {
            (_, style::Display::None) | (None, _) => len(s.width),
            (Some((w, _)), _) => px(content(w, s.padding.left, s.padding.right, bl, br).max(0.0)),
        },
        "height" => match (size, s.display) {
            (_, style::Display::None) | (None, _) => len(s.height),
            (Some((_, h)), _) => px(content(h, s.padding.top, s.padding.bottom, bt, bb).max(0.0)),
        },
        "min-width" => len(s.min_width),
        "min-height" => len(s.min_height),
        "max-width" => if s.max_width.is_auto() { "none".into() } else { len(s.max_width) },
        "max-height" => if s.max_height.is_auto() { "none".into() } else { len(s.max_height) },
        "top" | "right" | "bottom" | "left" => side(&s.inset, property),
        p if p.starts_with("margin-") => side(&s.margin, &p["margin-".len()..]),
        p if p.starts_with("padding-") => side(&s.padding, &p["padding-".len()..]),
        "border-top-width" => px(bt),
        "border-right-width" => px(br),
        "border-bottom-width" => px(bb),
        "border-left-width" => px(bl),
        "color" => color(s.color),
        "background-color" => s.background.map_or("rgba(0, 0, 0, 0)".into(), color),
        "font-size" => px(s.font_size),
        "font-weight" => s.font_weight.to_string(),
        "font-style" => if s.italic { "italic" } else { "normal" }.into(),
        "font-family" => s.font_names.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", "),
        "line-height" => match s.line_height {
            style::LineHeight::Normal => "normal".into(),
            style::LineHeight::Number(n) => px(n * s.font_size),
            style::LineHeight::Px(v) => px(v),
        },
        "text-align" => kebab(&s.text_align),
        "white-space" => kebab(&s.white_space),
        "flex-direction" => kebab(&s.flex_direction),
        "flex-wrap" => if s.flex_wrap { "wrap" } else { "nowrap" }.into(),
        "flex-grow" => trim(s.flex_grow),
        "flex-shrink" => trim(s.flex_shrink),
        "cursor" => kebab(&s.cursor),
        "pointer-events" => if s.pointer_events_none { "none" } else { "auto" }.into(),
        "transform" => {
            let moved = s.translate != Default::default() || s.rotate != 0.0 || s.scale != (1.0, 1.0);
            if moved { "matrix(1, 0, 0, 1, 0, 0)" } else { "none" }.into()
        }
        "transition-duration" | "animation-duration" | "transition-delay" | "animation-delay" => "0s".into(),
        "animation-name" => "none".into(),
        "direction" => "ltr".into(),
        "writing-mode" => "horizontal-tb".into(),
        _ => String::new(),
    }
}

fn rgb(c: Color, current: Color, dark: bool) -> String {
    let rgba = match c {
        Color::Rgba(v) => v,
        Color::LightDark(l, d) => if dark { d } else { l },
        Color::CurrentColor if current != Color::CurrentColor => return rgb(current, Color::Foreground, dark),
        Color::Link => 0x0000eeff,
        Color::Muted => 0x767676ff,
        _ => if dark { 0xffffffff } else { 0x000000ff },
    };
    let (r, g, b, a) = (rgba >> 24, (rgba >> 16) & 0xff, (rgba >> 8) & 0xff, rgba & 0xff);
    if a == 0xff { format!("rgb({r}, {g}, {b})") } else { format!("rgba({r}, {g}, {b}, {})", trim(a as f32 / 255.0)) }
}

/// `1.0` → `1`, `0.5` → `0.5` (as browsers print numbers).
fn trim(v: f32) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    if r.fract() == 0.0 { format!("{}", r as i64) } else { format!("{r}") }
}

/// `InlineBlock` → `inline-block`.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_read_like_a_browser() {
        let mut s = ComputedStyle::initial();
        s.display = style::Display::InlineBlock;
        s.padding.left = Length::Px(10.0);
        s.padding.right = Length::Px(10.0);
        s.color = Color::Rgba(0x11223380);
        assert_eq!(value(&s, "display", None, false), "inline-block");
        assert_eq!(value(&s, "width", Some((120.0, 40.0)), false), "100px");
        assert_eq!(value(&s, "width", None, false), "auto");
        assert_eq!(value(&s, "color", None, false), "rgba(17, 34, 51, 0.502)");
        assert_eq!(value(&s, "background-color", None, false), "rgba(0, 0, 0, 0)");
        assert_eq!(value(&s, "max-width", None, false), "none");
        assert_eq!(value(&s, "opacity", None, false), "1");
    }
}
