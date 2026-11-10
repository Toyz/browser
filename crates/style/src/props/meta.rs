//! What each property is besides its syntax: whether it inherits, and which
//! computed-style fields it owns (what `inherit` / `initial` / `unset` copy).
//!
//! One entry per property, so the cascade's CSS-wide keywords, inheritance
//! and the list of properties can't drift apart.

use crate::ComputedStyle;

/// Copies a property's computed value from `src` into a style.
pub type CopyFn = fn(&mut ComputedStyle, &ComputedStyle);

pub struct Meta {
    /// The property and its aliases (or, for families, a name prefix).
    pub names: &'static [&'static str],
    /// Whether `names` are prefixes (`margin` covers `margin-top`...).
    pub family: bool,
    /// Children take the parent's value unless something sets it.
    pub inherited: bool,
    pub copy: CopyFn,
}

const fn prop(names: &'static [&'static str], inherited: bool, copy: CopyFn) -> Meta {
    Meta { names, family: false, inherited, copy }
}

const fn family(prefix: &'static [&'static str], inherited: bool, copy: CopyFn) -> Meta {
    Meta { names: prefix, family: true, inherited, copy }
}

/// Properties whose computed values we keep, most specific first (families
/// are matched by prefix after exact names).
pub const META: &[Meta] = &[
    // Text and fonts (inherited).
    prop(&["color"], true, |s, src| s.color = src.color),
    prop(&["font"], true, |s, src| {
        s.font_size = src.font_size;
        s.font_weight = src.font_weight;
        s.italic = src.italic;
        s.font_family = src.font_family;
        s.font_names = src.font_names.clone();
        s.line_height = src.line_height;
    }),
    prop(&["font-size"], true, |s, src| s.font_size = src.font_size),
    prop(&["font-weight"], true, |s, src| s.font_weight = src.font_weight),
    prop(&["font-style"], true, |s, src| s.italic = src.italic),
    prop(&["font-family"], true, |s, src| {
        s.font_family = src.font_family;
        s.font_names = src.font_names.clone();
    }),
    prop(&["line-height"], true, |s, src| s.line_height = src.line_height),
    prop(&["text-align"], true, |s, src| s.text_align = src.text_align),
    prop(&["text-transform"], true, |s, src| s.text_transform = src.text_transform),
    prop(&["white-space"], true, |s, src| s.white_space = src.white_space),
    prop(&["visibility"], true, |s, src| s.visible = src.visible),
    prop(&["list-style", "list-style-type"], true, |s, src| s.list_style = src.list_style),
    prop(&["cursor"], true, |s, src| s.cursor = src.cursor),
    prop(&["pointer-events"], true, |s, src| s.pointer_events_none = src.pointer_events_none),
    prop(&["-webkit-text-fill-color"], true, |s, src| s.text_fill = src.text_fill),
    // Not inherited, but a child's decoration adds to the lines it's drawn
    // under (see `apply_wide`).
    prop(&["text-decoration", "text-decoration-line"], false, |s, src| {
        s.underline = src.underline;
        s.strikethrough = src.strikethrough;
    }),
    prop(&["text-overflow"], false, |s, src| s.text_ellipsis = src.text_ellipsis),
    prop(&["-webkit-line-clamp", "line-clamp"], false, |s, src| s.line_clamp = src.line_clamp),
    // Boxes.
    prop(&["display"], false, |s, src| s.display = src.display),
    prop(&["position"], false, |s, src| s.position = src.position),
    prop(&["width", "inline-size"], false, |s, src| s.width = src.width),
    prop(&["height", "block-size"], false, |s, src| s.height = src.height),
    prop(&["max-width", "max-inline-size"], false, |s, src| s.max_width = src.max_width),
    prop(&["min-width", "min-inline-size"], false, |s, src| s.min_width = src.min_width),
    prop(&["max-height", "max-block-size"], false, |s, src| s.max_height = src.max_height),
    prop(&["min-height", "min-block-size"], false, |s, src| s.min_height = src.min_height),
    prop(&["opacity"], false, |s, src| s.opacity = src.opacity),
    prop(&["object-fit"], false, |s, src| s.object_fit = src.object_fit),
    // Flexbox and grid.
    prop(&["gap", "grid-gap"], false, |s, src| {
        s.row_gap = src.row_gap;
        s.column_gap = src.column_gap;
    }),
    prop(&["row-gap", "grid-row-gap"], false, |s, src| s.row_gap = src.row_gap),
    prop(&["column-gap", "grid-column-gap"], false, |s, src| s.column_gap = src.column_gap),
    prop(&["align-items"], false, |s, src| s.align_items = src.align_items),
    prop(&["align-self"], false, |s, src| s.align_self = src.align_self),
    prop(&["align-content"], false, |s, src| s.align_content = src.align_content),
    prop(&["justify-content"], false, |s, src| s.justify_content = src.justify_content),
    prop(&["flex-direction"], false, |s, src| s.flex_direction = src.flex_direction),
    prop(&["flex-wrap"], false, |s, src| s.flex_wrap = src.flex_wrap),
    prop(&["flex-grow"], false, |s, src| s.flex_grow = src.flex_grow),
    prop(&["flex-shrink"], false, |s, src| s.flex_shrink = src.flex_shrink),
    prop(&["flex-basis"], false, |s, src| s.flex_basis = src.flex_basis),
    prop(&["flex"], false, |s, src| {
        s.flex_grow = src.flex_grow;
        s.flex_shrink = src.flex_shrink;
        s.flex_basis = src.flex_basis;
    }),
    // Borders: each one resets just itself (`border-radius: inherit` used to
    // wipe the whole border).
    prop(
        &[
            "border-radius",
            "border-top-left-radius",
            "border-top-right-radius",
            "border-bottom-left-radius",
            "border-bottom-right-radius",
            "border-start-start-radius",
            "border-start-end-radius",
            "border-end-start-radius",
            "border-end-end-radius",
        ],
        false,
        |s, src| s.border_radius = src.border_radius,
    ),
    prop(&["border-collapse", "border-spacing", "border-image"], false, |_, _| {}),
    // Families, by prefix.
    family(&["margin"], false, |s, src| s.margin = src.margin),
    family(&["padding"], false, |s, src| s.padding = src.padding),
    family(&["background"], false, |s, src| {
        s.background = src.background;
        s.background_image = src.background_image;
        s.gradients = src.gradients.clone();
    }),
    family(&["overflow"], false, |s, src| s.overflow = src.overflow),
];

/// Border longhands by component (`border-top-color`, `border-inline-width`).
fn border_component(name: &str) -> Option<CopyFn> {
    let rest = name.strip_prefix("border")?;
    Some(if rest.ends_with("-color") {
        |s, src| s.border_color = src.border_color
    } else if rest.ends_with("-style") {
        |s, src| s.border_style = src.border_style
    } else if rest.ends_with("-width") {
        |s, src| s.border_width = src.border_width
    } else {
        // `border`, `border-top`, `border-inline`...: all three.
        |s, src| {
            s.border_width = src.border_width;
            s.border_style = src.border_style;
            s.border_color = src.border_color;
        }
    })
}

/// What we know about property `name`: whether it inherits, and how to copy
/// its computed value (`None`: we keep nothing for it).
pub fn lookup(name: &str) -> Option<(bool, CopyFn)> {
    if let Some(m) = META.iter().find(|m| !m.family && m.names.contains(&name)) {
        return Some((m.inherited, m.copy));
    }
    if let Some(copy) = border_component(name) {
        return Some((false, copy));
    }
    META.iter()
        .find(|m| m.family && m.names.iter().any(|p| name == *p || name.strip_prefix(p).is_some_and(|r| r.starts_with('-'))))
        .map(|m| (m.inherited, m.copy))
}

/// Does `name` inherit by default?
pub fn inherited(name: &str) -> bool {
    lookup(name).is_some_and(|(inherited, _)| inherited)
}
