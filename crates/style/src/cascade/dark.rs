//! Dark mode: the UA colors and which pages opt in.

use super::*;

pub(super) fn meta_allows_dark(doc: &Document) -> bool {
    doc.descendants(doc.root()).any(|n| {
        doc.element(n).is_some_and(|e| {
            &*e.name.local == "meta"
                && e.attr("name").is_some_and(|v| v.eq_ignore_ascii_case("color-scheme"))
                && e.attr("content").is_some_and(|c| c.split_whitespace().any(|w| w.eq_ignore_ascii_case("dark")))
        })
    })
}

/// The browser's own colors in a dark color scheme (Chrome's).
pub(super) const DARK_TEXT: u32 = 0xe8eaedff;

pub(super) const DARK_LINK: u32 = 0x8ab4f8ff;

pub(super) const DARK_MUTED: u32 = 0x9aa0a6ff;

/// The canvas behind a dark page.
pub const DARK_CANVAS: u32 = 0x121212ff;

/// `light-dark()` and system colors, for this element's color scheme.
pub(super) fn scheme_color(c: Color, dark: bool) -> Color {
    match c {
        Color::LightDark(l, d) => Color::Rgba(if dark { d } else { l }),
        Color::Foreground if dark => Color::Rgba(DARK_TEXT),
        Color::Link if dark => Color::Rgba(DARK_LINK),
        Color::Muted if dark => Color::Rgba(DARK_MUTED),
        c => c,
    }
}
