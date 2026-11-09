//! CSS property parsing: declaration text → typed [`Decl`]s.
//!
//! Shorthands expand into longhands here. Values that depend on context (`em`,
//! `%`, `currentColor`) stay symbolic until the cascade resolves them.
//! Declarations containing `var()` can't be parsed until the element's custom
//! properties are known, so they're kept as [`Decl::Unresolved`].

use std::sync::Arc;

use cssparser::{ParseError, Parser, Token};

use crate::values::*;

type PResult<'i, T> = Result<T, ParseError<()>>;

thread_local! {
    /// The URL relative `url()`s resolve against while parsing a stylesheet.
    static CSS_BASE: std::cell::RefCell<Option<url::Url>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with `url()` values resolving against `base` (a stylesheet's URL,
/// or the document's for inline styles).
pub fn with_base<R>(base: Option<&url::Url>, f: impl FnOnce() -> R) -> R {
    let previous = CSS_BASE.with(|b| b.replace(base.cloned()));
    let result = f();
    CSS_BASE.with(|b| *b.borrow_mut() = previous);
    result
}

fn resolve_css_url(raw: &str) -> Option<Arc<str>> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('#') {
        return None; // `url(#fragment)` refers to SVG in the page itself
    }
    CSS_BASE.with(|base| match &*base.borrow() {
        Some(base) => base.join(raw).ok(),
        None => url::Url::parse(raw).ok(),
    })
    .map(|u| Arc::from(u.as_str()))
}

/// `url(...)` (quoted or not), resolved.
pub(crate) fn parse_url<'i>(p: &mut Parser<'i>) -> PResult<'i, Option<Arc<str>>> {
    match p.next()?.clone() {
        Token::UnquotedUrl(u) => Ok(resolve_css_url(&u)),
        Token::Function(f) if f.eq_ignore_ascii_case("url") => {
            let raw = p.parse_nested_block(|p| Ok(p.expect_string()?.to_string()))?;
            Ok(resolve_css_url(&raw))
        }
        _ => err(p),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wide {
    Inherit,
    Initial,
    Unset,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FontSizeSpec {
    /// `em`/`%` here are relative to the *parent's* font size.
    Len(LenSpec),
    Smaller,
    Larger,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LineHeightSpec {
    Normal,
    Number(f32),
    Len(LenSpec),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeightSpec {
    Abs(u16),
    Bolder,
    Lighter,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GridCols {
    None,
    Count(u16),
    /// `repeat(auto-fill, minmax(<min>, 1fr))`: as many as fit. `true` for
    /// `auto-fit` (empty tracks collapse, so the items stretch).
    AutoFill(LenSpec, bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum VAlignSpec {
    Keyword(VerticalAlign),
    Len(LenSpec),
}

/// `background-size` as specified (lengths still relative).
#[derive(Debug, Clone, PartialEq)]
pub enum BgSizeSpec {
    Auto,
    Contain,
    Cover,
    Explicit(LenSpec, LenSpec),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decl {
    Display(Display),
    Position(Position),
    Inset(Side, LenSpec),
    Margin(Side, LenSpec),
    Padding(Side, LenSpec),
    BorderWidth(Side, LenSpec),
    BorderStyle(Side, BorderStyle),
    BorderColor(Side, Color),
    BorderRadius(usize, LenSpec),
    BackgroundColor(Color),
    /// A gradient approximated by its first color stop (painted over the background color).
    BackgroundImage(Option<Color>),
    Gradients(Option<Arc<[Gradient]>>),
    BackgroundRepeat(BgRepeat),
    BackgroundUrl(Option<Arc<str>>),
    BackgroundSize(BgSizeSpec),
    BackgroundPosition(BgPos),
    Width(LenSpec),
    Height(LenSpec),
    MinWidth(LenSpec),
    MinHeight(LenSpec),
    MaxWidth(LenSpec),
    MaxHeight(LenSpec),
    FlexDirection(FlexDirection),
    FlexWrap(bool),
    JustifyContent(Justify),
    AlignItems(Align),
    /// Where wrapped flex lines go (as `Justify`: it has the same values).
    AlignContent(Justify),
    AlignSelf(Option<Align>),
    RowGap(LenSpec),
    ColumnGap(LenSpec),
    FlexGrow(f32),
    FlexShrink(f32),
    FlexBasis(LenSpec),
    GridColumns(GridCols),
    /// Explicit tracks (`None`: equal columns or none).
    GridTracks(Option<Arc<[TrackSpec]>>),
    GridAreas(Option<GridAreas>),
    GridArea(Option<Arc<str>>),
    GridRowStart(Option<i16>),
    /// `width: fit-content` and friends.
    FitWidth(bool),
    GridColumnStart(Option<i16>),
    GridColumn(Option<GridSpan>),
    Overflow(Overflow),
    Opacity(f32),
    BoxShadow(Option<Arc<[ShadowSpec]>>),
    TransitionProperty(Vec<Arc<str>>),
    TransitionDuration(Vec<f32>),
    TransitionDelay(Vec<f32>),
    TransitionTiming(Vec<Easing>),
    /// The `transition` shorthand: (property, duration, delay, easing) per item.
    Transition(Vec<(Arc<str>, f32, f32, Easing)>),
    Animation(Option<Arc<[Animation]>>),
    AnimationName(Vec<Arc<str>>),
    AnimationDuration(Vec<f32>),
    AnimationDelay(Vec<f32>),
    AnimationTiming(Vec<Easing>),
    AnimationIterations(Vec<f32>),
    AnimationDirection(Vec<AnimDirection>),
    AnimationFill(Vec<(bool, bool)>),
    /// `transform` / `translate`: just the translation (x, y).
    Translate(LenSpec, LenSpec),
    ClipText(bool),
    /// `background-clip` per layer: `true` where a layer paints under the
    /// border too (`border-box`).
    BgClipBorder(Arc<[bool]>),
    AspectRatio(Option<f32>),
    TextFill(Color),
    /// An SVG paint property and its value as written.
    SvgPaint(Arc<str>, Arc<str>),
    /// Degrees.
    Rotate(f32),
    Scale(f32, f32),
    Masked(bool),
    MaskUrl(Option<Arc<str>>),
    MaskSize(BgSizeSpec),
    MaskPosition(BgPos),
    Content(Option<Arc<[ContentItem]>>),
    Float(Float),
    Clear(bool),
    VerticalAlign(VAlignSpec),
    BorderBox(bool),
    Cursor(Cursor),
    /// `pointer-events: none` (`true`) or anything else.
    PointerEventsNone(bool),
    /// `z-index` (`None`: auto).
    ZIndex(Option<i32>),
    ObjectFit(crate::ObjectFit),
    TextEllipsis(bool),
    LineClamp(Option<u32>),
    /// `color-scheme` allows dark.
    ColorScheme(bool),
    TextDecoration { underline: bool, strikethrough: bool },
    FontSize(FontSizeSpec),
    LineHeight(LineHeightSpec),
    FontWeight(WeightSpec),
    Italic(bool),
    FontFamily(FontFamily),
    FontNames(FontNames),
    Color(Color),
    WhiteSpace(WhiteSpace),
    TextAlign(TextAlign),
    ListStyle(ListStyle),
    TextTransform(TextTransform),
    Visible(bool),
    /// `--name: value` (raw text, substituted into `var()` later).
    Custom(Arc<str>, Arc<str>),
    /// A declaration containing `var()`; parsed once custom properties are known.
    Unresolved { name: Arc<str>, raw: Arc<str> },
    /// `inherit` / `initial` / `unset` for the named property.
    Wide(Arc<str>, Wide),
}

impl Decl {
    /// Font size must be computed before anything that uses `em`.
    pub fn is_font_size(&self) -> bool {
        match self {
            Decl::FontSize(_) => true,
            Decl::Unresolved { name, .. } | Decl::Wide(name, _) => matches!(&**name, "font-size" | "font"),
            _ => false,
        }
    }
}

pub fn contains_var(value: &str) -> bool {
    value.as_bytes().windows(4).any(|w| w.eq_ignore_ascii_case(b"var("))
}

/// Parse one declaration. Invalid declarations produce nothing (CSS's error recovery).
/// SVG presentation properties, passed through to inline SVGs.
pub const SVG_PAINT: &[&str] = &["fill", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin", "fill-opacity", "stroke-opacity", "fill-rule"];

pub fn parse_declaration(name: &str, value: &str, out: &mut Vec<Decl>) {
    if name.starts_with("--") {
        out.push(Decl::Custom(name.into(), value.trim().into()));
        return;
    }
    let name = name.to_ascii_lowercase();
    if contains_var(value) {
        out.push(Decl::Unresolved { name: name.into(), raw: value.trim().into() });
        return;
    }
    let trimmed = value.trim();
    let wide = match trimmed.to_ascii_lowercase().as_str() {
        "inherit" => Some(Wide::Inherit),
        "initial" => Some(Wide::Initial),
        "unset" | "revert" | "revert-layer" => Some(Wide::Unset),
        _ => None,
    };
    if let Some(wide) = wide {
        out.push(Decl::Wide(name.into(), wide));
        return;
    }
    // SVG paint: kept as written, for the SVG renderer to read.
    if SVG_PAINT.contains(&name.as_str()) {
        out.push(Decl::SvgPaint(name.into(), trimmed.into()));
        return;
    }
    let start = out.len();
    let mut p = Parser::new(trimmed);
    let ok = parse_property(&name, &mut p, out).is_ok() && p.expect_exhausted().is_ok();
    if !ok {
        out.truncate(start);
    }
}

fn err<'i, T>(_p: &Parser<'i>) -> PResult<'i, T> {
    Err(ParseError::custom(()))
}

fn ident<'i>(p: &mut Parser<'i>) -> PResult<'i, String> {
    Ok(p.expect_ident()?.to_ascii_lowercase())
}

fn keyword<'i, T: Copy>(p: &mut Parser<'i>, table: &[(&str, T)]) -> PResult<'i, T> {
    let id = ident(p)?;
    match table.iter().find(|(k, _)| *k == id) {
        Some(&(_, v)) => Ok(v),
        None => err(p),
    }
}

/// Up to four values in TRBL shorthand order.
fn four<'i, T: Clone>(
    p: &mut Parser<'i>,
    mut f: impl FnMut(&mut Parser<'i>) -> PResult<'i, T>,
) -> PResult<'i, [T; 4]> {
    let mut v = vec![f(p)?];
    while v.len() < 4 {
        match p.try_parse(&mut f) {
            Ok(x) => v.push(x),
            Err(_) => break,
        }
    }
    Ok(match v.len() {
        1 => [v[0].clone(), v[0].clone(), v[0].clone(), v[0].clone()],
        2 => [v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()],
        3 => [v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()],
        _ => [v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()],
    })
}

/// Logical (`-inline`/`-block`, `-start`/`-end`) to physical sides, assuming LTR horizontal text.
fn logical_sides(suffix: &str) -> Option<&'static [Side]> {
    Some(match suffix {
        "inline" => &[Side::Left, Side::Right],
        "block" => &[Side::Top, Side::Bottom],
        "inline-start" | "left" => &[Side::Left],
        "inline-end" | "right" => &[Side::Right],
        "block-start" | "top" => &[Side::Top],
        "block-end" | "bottom" => &[Side::Bottom],
        _ => return None,
    })
}

fn parse_property<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;

    // margin / padding (+ sides, + logical)
    for (prefix, make) in [
        ("margin", D::Margin as fn(Side, LenSpec) -> Decl),
        ("padding", D::Padding as fn(Side, LenSpec) -> Decl),
    ] {
        if name == prefix {
            let v = four(p, parse_len)?;
            for (side, len) in Side::ALL.into_iter().zip(v) {
                out.push(make(side, len));
            }
            return Ok(());
        }
        if let Some(sides) = name.strip_prefix(prefix).and_then(|s| s.strip_prefix('-')).and_then(logical_sides) {
            let first = parse_len(p)?;
            let second = p.try_parse(parse_len).unwrap_or_else(|_| first.clone());
            for (i, &side) in sides.iter().enumerate() {
                out.push(make(side, if i == 0 { first.clone() } else { second.clone() }));
            }
            return Ok(());
        }
    }

    if let Some(rest) = name.strip_prefix("border") {
        return parse_border_inner(rest, p, out);
    }

    match name {
        "display" => out.push(D::Display(parse_display(p)?)),
        "position" => out.push(D::Position(keyword(p, &[
            ("static", Position::Static),
            ("relative", Position::Relative),
            ("absolute", Position::Absolute),
            ("fixed", Position::Fixed),
            ("sticky", Position::Sticky),
            ("-webkit-sticky", Position::Sticky),
        ])?)),
        "top" | "right" | "bottom" | "left" | "inset-block-start" | "inset-block-end"
        | "inset-inline-start" | "inset-inline-end" => {
            let side = match name.trim_start_matches("inset-") {
                "top" | "block-start" => Side::Top,
                "right" | "inline-end" => Side::Right,
                "bottom" | "block-end" => Side::Bottom,
                _ => Side::Left,
            };
            out.push(D::Inset(side, parse_len(p)?));
        }
        "inset" => {
            for (side, v) in Side::ALL.into_iter().zip(four(p, parse_len)?) {
                out.push(D::Inset(side, v));
            }
        }
        "inset-inline" | "inset-block" => {
            let first = parse_len(p)?;
            let second = p.try_parse(parse_len).unwrap_or_else(|_| first.clone());
            let sides = if name == "inset-inline" { [Side::Left, Side::Right] } else { [Side::Top, Side::Bottom] };
            out.push(D::Inset(sides[0], first));
            out.push(D::Inset(sides[1], second));
        }
        "width" | "inline-size" => {
            // `fit-content` / `max-content` / `min-content`: shrink to the content.
            let fit = p
                .try_parse(|p| match p.next()?.clone() {
                    Token::Ident(id)
                        if matches!(
                            &*id.to_ascii_lowercase(),
                            "fit-content" | "max-content" | "min-content" | "-moz-fit-content" | "-webkit-fit-content" | "-moz-max-content" | "-webkit-max-content"
                        ) =>
                    {
                        Ok(())
                    }
                    Token::Function(f) if f.eq_ignore_ascii_case("fit-content") => p.parse_nested_block(|p| {
                        while p.next().is_ok() {}
                        Ok(())
                    }),
                    _ => err(p),
                })
                .is_ok();
            if fit {
                out.push(D::Width(LenSpec::Auto));
                out.push(D::FitWidth(true));
            } else {
                out.push(D::Width(parse_len(p)?));
            }
        }
        "height" | "block-size" => out.push(D::Height(parse_len(p)?)),
        "min-width" | "min-inline-size" => out.push(D::MinWidth(parse_len(p)?)),
        "min-height" | "min-block-size" => out.push(D::MinHeight(parse_len(p)?)),
        "max-width" | "max-inline-size" => out.push(D::MaxWidth(parse_len(p)?)),
        "max-height" | "max-block-size" => out.push(D::MaxHeight(parse_len(p)?)),

        "color" => out.push(D::Color(parse_color(p)?)),
        "background-color" => out.push(D::BackgroundColor(parse_color(p)?)),
        // `text`: the background shows through the glyphs (gradient text).
        "background-clip" | "-webkit-background-clip" => {
            let mut text = false;
            let mut layers = vec![true];
            while !p.is_exhausted() {
                match p.next() {
                    Ok(Token::Ident(id)) => {
                        text |= id.eq_ignore_ascii_case("text");
                        if let Some(last) = layers.last_mut() {
                            *last = id.eq_ignore_ascii_case("border-box");
                        }
                    }
                    Ok(Token::Comma) => layers.push(true),
                    _ => {}
                }
            }
            out.push(D::ClipText(text));
            out.push(D::BgClipBorder(layers.into()));
        }
        "-webkit-text-fill-color" => out.push(D::TextFill(parse_color(p)?)),
        // `1`, `16 / 9`, `auto 4/3` (`auto` alone: none).
        "aspect-ratio" => {
            let mut nums = Vec::new();
            while !p.is_exhausted() {
                match p.next()?.clone() {
                    Token::Number { value, .. } => nums.push(value),
                    Token::Ident(_) | Token::Delim('/') => {}
                    _ => return err(p),
                }
            }
            let ratio = match nums.as_slice() {
                [w] => Some(*w),
                [w, h] if *h > 0.0 => Some(w / h),
                _ => None,
            };
            out.push(D::AspectRatio(ratio.filter(|r| *r > 0.0)));
        }
        "background" => parse_background(p, out)?,
        "background-image" => {
            let layer = parse_image_layer(p)?;
            out.push(D::BackgroundImage(layer.gradient));
            out.push(D::Gradients(layer.gradients()));
            out.push(D::BackgroundUrl(layer.url));
        }
        "background-repeat" => {
            let layer = parse_image_layer(p)?;
            out.push(D::BackgroundRepeat(layer.repeat.unwrap_or_default()));
        }

        "font" => parse_font(p, out)?,
        "font-size" => out.push(D::FontSize(parse_font_size(p)?)),
        "font-weight" => out.push(D::FontWeight(parse_weight(p)?)),
        "font-style" => {
            let v = ident(p)?;
            // `oblique 10deg`
            let _ = p.try_parse(|p| p.next().map(|_| ()));
            out.push(D::Italic(matches!(v.as_str(), "italic" | "oblique")));
        }
        "font-family" => {
            let (family, names) = parse_font_family(p)?;
            out.extend([D::FontFamily(family), D::FontNames(names)]);
        }
        "line-height" => out.push(D::LineHeight(parse_line_height(p)?)),

        "text-align" => out.push(D::TextAlign(keyword(p, &[
            ("left", TextAlign::Left),
            ("start", TextAlign::Left),
            ("justify", TextAlign::Left),
            ("-webkit-left", TextAlign::Left),
            ("center", TextAlign::Center),
            ("-webkit-center", TextAlign::Center),
            ("-moz-center", TextAlign::Center),
            ("right", TextAlign::Right),
            ("end", TextAlign::Right),
            ("-webkit-right", TextAlign::Right),
            ("match-parent", TextAlign::Left),
        ])?)),
        "text-decoration" | "text-decoration-line" => parse_text_decoration(p, out)?,
        "text-transform" => out.push(D::TextTransform(keyword(p, &[
            ("none", TextTransform::None),
            ("uppercase", TextTransform::Uppercase),
            ("lowercase", TextTransform::Lowercase),
            ("capitalize", TextTransform::Capitalize),
        ])?)),
        "white-space" => out.push(D::WhiteSpace(keyword(p, &[
            ("normal", WhiteSpace::Normal),
            ("nowrap", WhiteSpace::NoWrap),
            ("pre", WhiteSpace::Pre),
            ("pre-wrap", WhiteSpace::PreWrap),
            ("break-spaces", WhiteSpace::PreWrap),
            ("pre-line", WhiteSpace::PreLine),
        ])?)),
        "visibility" => out.push(D::Visible(ident(p)? == "visible")),
        "list-style" | "list-style-type" => {
            let mut found = None;
            while !p.is_exhausted() {
                if let Ok(ls) = p.try_parse(parse_list_style_type) {
                    found = Some(ls);
                } else {
                    p.next()?; // image / position: ignored
                }
            }
            match found {
                Some(ls) => out.push(D::ListStyle(ls)),
                None if name == "list-style" => out.push(D::ListStyle(ListStyle::Disc)),
                None => return err(p),
            }
        }
        "content" => out.push(D::Content(parse_content(p)?)),
        "float" => out.push(D::Float(keyword(p, &[
            ("none", Float::None),
            ("left", Float::Left),
            ("inline-start", Float::Left),
            ("right", Float::Right),
            ("inline-end", Float::Right),
        ])?)),
        "clear" => out.push(D::Clear(ident(p)? != "none")),
        "color-scheme" => {
            let mut dark = false;
            while let Ok(id) = p.expect_ident_cloned() {
                dark |= id.eq_ignore_ascii_case("dark");
            }
            out.push(D::ColorScheme(dark));
        }
        "z-index" => {
            let z = p.try_parse(|p| p.expect_integer()).ok();
            out.push(D::ZIndex(z));
        }
        "text-overflow" => {
            let v = ident(p)?;
            out.push(D::TextEllipsis(v.eq_ignore_ascii_case("ellipsis")));
        }
        "-webkit-line-clamp" | "line-clamp" => {
            let n = p.try_parse(|p| p.expect_integer()).ok().filter(|&n| n > 0).map(|n| n as u32);
            out.push(D::LineClamp(n));
        }
        "object-fit" => {
            let fit = match ident(p)?.to_ascii_lowercase().as_str() {
                "contain" => crate::ObjectFit::Contain,
                "cover" => crate::ObjectFit::Cover,
                "none" => crate::ObjectFit::None,
                "scale-down" => crate::ObjectFit::ScaleDown,
                _ => crate::ObjectFit::Fill,
            };
            out.push(D::ObjectFit(fit));
        }
        "pointer-events" => {
            let v = ident(p)?;
            out.push(D::PointerEventsNone(v.eq_ignore_ascii_case("none")));
        }
        "cursor" => {
            // `cursor: url(x.cur), pointer`: skip images, take the first keyword we know.
            let mut cursor = None;
            while !p.is_exhausted() {
                if let Ok(Token::Ident(id)) = p.next().cloned() {
                    let c = match id.to_ascii_lowercase().as_str() {
                        "auto" | "default" | "none" | "context-menu" | "help" | "progress" | "wait" => Some(Cursor::Auto),
                        "pointer" => Some(Cursor::Pointer),
                        "text" | "vertical-text" => Some(Cursor::Text),
                        "not-allowed" | "no-drop" => Some(Cursor::NotAllowed),
                        "grab" => Some(Cursor::Grab),
                        "grabbing" => Some(Cursor::Grabbing),
                        "crosshair" | "cell" => Some(Cursor::Crosshair),
                        "move" | "all-scroll" => Some(Cursor::Move),
                        "col-resize" | "ew-resize" | "e-resize" | "w-resize" => Some(Cursor::ResizeColumn),
                        "row-resize" | "ns-resize" | "n-resize" | "s-resize" => Some(Cursor::ResizeRow),
                        _ => None,
                    };
                    if cursor.is_none() {
                        cursor = c;
                    }
                }
            }
            out.push(D::Cursor(cursor.unwrap_or(Cursor::Auto)));
        }
        "box-sizing" | "-webkit-box-sizing" | "-moz-box-sizing" => {
            out.push(D::BorderBox(ident(p)? == "border-box"))
        }
        "vertical-align" => {
            let v = match p.try_parse(|p| keyword(p, &[
                ("baseline", VerticalAlign::Baseline),
                ("sub", VerticalAlign::Sub),
                ("super", VerticalAlign::Super),
                ("middle", VerticalAlign::Middle),
                ("top", VerticalAlign::Top),
                ("bottom", VerticalAlign::Bottom),
                ("text-top", VerticalAlign::TextTop),
                ("text-bottom", VerticalAlign::TextBottom),
            ])) {
                Ok(k) => VAlignSpec::Keyword(k),
                Err(_) => VAlignSpec::Len(parse_len(p)?),
            };
            out.push(D::VerticalAlign(v));
        }
        "mask" | "-webkit-mask" => {
            let layer = parse_image_layer(p)?;
            out.push(D::Masked(layer.url.is_some() || layer.other_image));
            out.push(D::MaskUrl(layer.url));
            out.push(D::MaskSize(layer.size.unwrap_or(BgSizeSpec::Auto)));
            out.push(D::MaskPosition(layer.position.unwrap_or(BgPos::TOP_LEFT)));
        }
        "mask-image" | "-webkit-mask-image" => {
            let layer = parse_image_layer(p)?;
            out.push(D::Masked(layer.url.is_some() || layer.other_image));
            out.push(D::MaskUrl(layer.url));
        }
        "mask-size" | "-webkit-mask-size" => out.push(D::MaskSize(parse_bg_size(p)?)),
        "mask-position" | "-webkit-mask-position" => out.push(D::MaskPosition(parse_bg_position(p)?)),
        "background-size" => out.push(D::BackgroundSize(parse_bg_size(p)?)),
        "background-position" => out.push(D::BackgroundPosition(parse_bg_position(p)?)),
        "transition-property" => out.push(D::TransitionProperty(p.parse_comma_separated(|p| {
            Ok(Arc::from(&*p.expect_ident_cloned()?.to_ascii_lowercase()))
        })?)),
        "transition-duration" => out.push(D::TransitionDuration(p.parse_comma_separated(parse_time)?)),
        "transition-delay" => out.push(D::TransitionDelay(p.parse_comma_separated(parse_time)?)),
        "transition-timing-function" => out.push(D::TransitionTiming(p.parse_comma_separated(parse_easing)?)),
        "transition" => {
            let items = p.parse_comma_separated(|p| {
                let (mut property, mut times, mut easing) = (Arc::from("all"), Vec::new(), Easing::EASE);
                while !p.is_exhausted() {
                    if let Ok(t) = p.try_parse(parse_time) {
                        times.push(t);
                    } else if let Ok(e) = p.try_parse(parse_easing) {
                        easing = e;
                    } else if let Ok(id) = p.try_parse(|p| p.expect_ident_cloned()) {
                        property = Arc::from(&*id.to_ascii_lowercase());
                    } else {
                        p.next()?;
                    }
                }
                Ok((property, times.first().copied().unwrap_or(0.0), times.get(1).copied().unwrap_or(0.0), easing))
            })?;
            out.push(D::Transition(items));
        }
        "animation-name" => out.push(D::AnimationName(p.parse_comma_separated(|p| {
            Ok(match p.next()?.clone() {
                Token::Ident(s) | Token::QuotedString(s) => Arc::from(&*s),
                _ => Arc::from("none"),
            })
        })?)),
        "animation-duration" => out.push(D::AnimationDuration(p.parse_comma_separated(parse_time)?)),
        "animation-delay" => out.push(D::AnimationDelay(p.parse_comma_separated(parse_time)?)),
        "animation-timing-function" => out.push(D::AnimationTiming(p.parse_comma_separated(parse_easing)?)),
        "animation-iteration-count" => out.push(D::AnimationIterations(p.parse_comma_separated(parse_iterations)?)),
        "animation-direction" => out.push(D::AnimationDirection(p.parse_comma_separated(parse_direction)?)),
        "animation-fill-mode" => out.push(D::AnimationFill(p.parse_comma_separated(parse_fill)?)),
        "animation" => {
            let items = p.parse_comma_separated(|p| {
                let mut a = Animation {
                    name: Arc::from("none"),
                    duration: 0.0,
                    delay: 0.0,
                    easing: Easing::EASE,
                    iterations: 1.0,
                    direction: AnimDirection::Normal,
                    fill_forwards: false,
                    fill_backwards: false,
                };
                let mut times = 0;
                while !p.is_exhausted() {
                    if let Ok(t) = p.try_parse(parse_time) {
                        if times == 0 { a.duration = t } else { a.delay = t }
                        times += 1;
                    } else if let Ok(e) = p.try_parse(parse_easing) {
                        a.easing = e;
                    } else if let Ok(n) = p.try_parse(parse_iterations) {
                        a.iterations = n;
                    } else if let Ok(d) = p.try_parse(parse_direction) {
                        a.direction = d;
                    } else if let Ok((f, b)) = p.try_parse(parse_fill) {
                        (a.fill_forwards, a.fill_backwards) = (f, b);
                    } else if let Ok(name) = p.try_parse(|p| match p.next()?.clone() {
                        Token::Ident(s) if !matches!(&*s.to_ascii_lowercase(), "running" | "paused") => Ok(s),
                        Token::QuotedString(s) => Ok(s),
                        _ => err(p),
                    }) {
                        a.name = Arc::from(&*name);
                    } else {
                        p.next()?;
                    }
                }
                Ok(a)
            })?;
            let items: Vec<Animation> = items.into_iter().filter(|a| &*a.name != "none").collect();
            out.push(D::Animation((!items.is_empty()).then(|| items.into())));
        }
        "box-shadow" => {
            if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
                out.push(D::BoxShadow(None));
            } else {
                let layers = p.parse_comma_separated(|p| {
                    let mut lengths = Vec::new();
                    let mut color = None;
                    let mut inset = false;
                    while !p.is_exhausted() {
                        if p.try_parse(|p| p.expect_ident_matching("inset")).is_ok() {
                            inset = true;
                        } else if let Ok(l) = p.try_parse(parse_len) {
                            lengths.push(l);
                        } else if let Ok(c) = p.try_parse(parse_color) {
                            color = Some(c);
                        } else {
                            p.next()?;
                        }
                    }
                    Ok(ShadowSpec { lengths, color: color.unwrap_or(Color::CurrentColor), inset })
                })?;
                let layers: Vec<ShadowSpec> = layers.into_iter().filter(|l| l.lengths.len() >= 2).collect();
                out.push(D::BoxShadow((!layers.is_empty()).then(|| layers.into())));
            }
        }
        "rotate" => {
            // `rotate: 45deg` (axis forms like `rotate: x 45deg` aren't 2D).
            let deg = if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() { 0.0 } else { parse_angle(p)? };
            out.push(D::Rotate(deg));
        }
        "scale" => {
            let (x, y) = if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
                (1.0, 1.0)
            } else {
                let x = parse_factor(p)?;
                (x, p.try_parse(parse_factor).unwrap_or(x))
            };
            out.push(D::Scale(x, y));
        }
        "transform" | "translate" => {
            // Translations, rotations and scales (skews and 3D are ignored).
            let (mut x, mut y) = (Vec::new(), Vec::new());
            let (mut rotate, mut scale) = (0.0f32, (1.0f32, 1.0f32));
            if name == "translate" {
                if let Ok(v) = parse_len(p) {
                    x.push(v);
                }
                if let Ok(v) = p.try_parse(parse_len) {
                    y.push(v);
                }
            }
            while !p.is_exhausted() {
                let Ok(token) = p.next().cloned() else { break };
                let Token::Function(f) = token else { continue };
                let f = f.to_ascii_lowercase();
                p.parse_nested_block(|p| {
                    match f.as_str() {
                        "rotate" | "rotatez" => rotate += parse_angle(p).unwrap_or(0.0),
                        "scale" | "scalex" | "scaley" | "scale3d" => {
                            let a = parse_factor(p).unwrap_or(1.0);
                            let b = p.try_parse(|p| {
                                p.expect_comma()?;
                                parse_factor(p)
                            });
                            match f.as_str() {
                                "scalex" => scale.0 *= a,
                                "scaley" => scale.1 *= a,
                                _ => {
                                    scale.0 *= a;
                                    scale.1 *= b.unwrap_or(a);
                                }
                            }
                        }
                        "matrix" => {
                            let mut m = [0.0f32; 6];
                            for (i, v) in m.iter_mut().enumerate() {
                                if i > 0 {
                                    p.expect_comma()?;
                                }
                                *v = p.expect_number()?;
                            }
                            let [a, b, c, d, e, f] = m;
                            let sx = (a * a + b * b).sqrt();
                            rotate += b.atan2(a).to_degrees();
                            scale.0 *= sx;
                            scale.1 *= if sx > 0.0 { (a * d - b * c) / sx } else { 0.0 };
                            x.push(LenSpec::px(e));
                            y.push(LenSpec::px(f));
                        }
                        _ => {
                            let first = parse_len(p);
                            let second = p.try_parse(|p| {
                                p.expect_comma()?;
                                parse_len(p)
                            });
                            match f.as_str() {
                                "translate" | "translate3d" => {
                                    x.extend(first.ok());
                                    y.extend(second.ok());
                                }
                                "translatex" => x.extend(first.ok()),
                                "translatey" => y.extend(first.ok()),
                                _ => {}
                            }
                        }
                    }
                    while p.next().is_ok() {}
                    Ok(())
                })?;
            }
            let sum = |v: Vec<LenSpec>| match v.len() {
                0 => LenSpec::px(0.0),
                1 => v.into_iter().next().expect("one"),
                _ => LenSpec::Lin(v.iter().filter_map(|l| match l { LenSpec::Lin(l) => Some(*l), _ => None }).fold(Lin::default(), |a, b| a + b)),
            };
            out.push(D::Translate(sum(x), sum(y)));
            if name == "transform" {
                out.push(D::Rotate(rotate));
                out.push(D::Scale(scale.0, scale.1));
            }
        }
        "opacity" => {
            let v = match p.next()?.clone() {
                Token::Number { value, .. } => value,
                Token::Percentage { unit_value, .. } => unit_value,
                _ => return err(p),
            };
            out.push(D::Opacity(v.clamp(0.0, 1.0)));
        }
        "overflow" | "overflow-x" | "overflow-y" => {
            let mut overflow = Overflow::Visible;
            while !p.is_exhausted() {
                overflow = overflow.max_with(match ident(p)?.as_str() {
                    "hidden" | "clip" => Overflow::Hidden,
                    "auto" | "scroll" | "overlay" => Overflow::Scroll,
                    _ => Overflow::Visible,
                });
            }
            out.push(D::Overflow(overflow));
        }

        // ---- flexbox ----
        "flex-direction" => out.push(D::FlexDirection(parse_flex_direction(p)?)),
        "flex-wrap" => out.push(D::FlexWrap(ident(p)? != "nowrap")),
        "flex-flow" => {
            while !p.is_exhausted() {
                if let Ok(d) = p.try_parse(parse_flex_direction) {
                    out.push(D::FlexDirection(d));
                } else {
                    out.push(D::FlexWrap(ident(p)? != "nowrap"));
                }
            }
        }
        "justify-content" => out.push(D::JustifyContent(parse_justify(p)?)),
        "align-items" => out.push(D::AlignItems(parse_align(p)?)),
        "align-self" => {
            out.push(D::AlignSelf(match p.try_parse(|p| p.expect_ident_matching("auto")) {
                Ok(()) => None,
                Err(_) => Some(parse_align(p)?),
            }))
        }
        "align-content" => out.push(D::AlignContent(parse_justify(p)?)),
        "place-items" => {
            out.push(D::AlignItems(parse_align(p)?));
            let _ = p.try_parse(parse_align);
        }
        "place-content" => {
            let a = parse_justify(p)?;
            let j = p.try_parse(parse_justify).unwrap_or(a);
            out.push(D::AlignContent(a));
            out.push(D::JustifyContent(j));
        }
        "gap" | "grid-gap" => {
            let row = parse_len(p)?;
            let col = p.try_parse(parse_len).unwrap_or_else(|_| row.clone());
            out.push(D::RowGap(row));
            out.push(D::ColumnGap(col));
        }
        "row-gap" | "grid-row-gap" => out.push(D::RowGap(parse_len(p)?)),
        "column-gap" | "grid-column-gap" => out.push(D::ColumnGap(parse_len(p)?)),
        "flex-grow" => out.push(D::FlexGrow(p.expect_number()?)),
        "flex-shrink" => out.push(D::FlexShrink(p.expect_number()?)),
        "flex-basis" => out.push(D::FlexBasis(parse_len(p)?)),
        "flex" => parse_flex(p, out)?,

        // ---- grid ----
        "grid-template-columns" => parse_template_columns(p, out)?,
        "grid-template-areas" => out.push(D::GridAreas(parse_areas(p))),
        "grid-template" => {
            // `<rows> / <columns>`, or `"a b" "c d" / <columns>`.
            if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
                out.push(D::GridColumns(GridCols::None));
                out.push(D::GridTracks(None));
                out.push(D::GridAreas(None));
            } else {
                let mut areas: Vec<Arc<[Arc<str>]>> = Vec::new();
                let mut slash = false;
                while !p.is_exhausted() {
                    match p.next()?.clone() {
                        Token::Delim('/') => {
                            slash = true;
                            break;
                        }
                        Token::QuotedString(row) => areas.push(area_row(&row)),
                        _ => {}
                    }
                }
                if slash {
                    parse_template_columns(p, out)?;
                }
                if !areas.is_empty() {
                    out.push(D::GridAreas(Some(areas.into())));
                }
            }
        }
        "grid-area" => {
            let name = p.try_parse(|p| p.expect_ident_cloned()).ok().filter(|n| !n.eq_ignore_ascii_case("auto"));
            // `grid-area: 1 / 1` (row start / column start / ...): line numbers.
            let mut lines = Vec::new();
            while let Ok(t) = p.next().cloned() {
                if let Token::Number { int_value: Some(n), .. } = t {
                    lines.push(n);
                }
            }
            if name.is_none() && !lines.is_empty() {
                out.push(D::GridRowStart(lines.first().map(|&n| n as i16)));
                out.push(D::GridColumnStart(lines.get(1).map(|&n| n as i16)));
            }
            out.push(D::GridArea(name.map(|n| Arc::from(&*n))));
        }
        "grid-row-start" | "grid-row" => {
            let n = p.try_parse(|p| p.expect_integer()).ok();
            while p.next().is_ok() {}
            out.push(D::GridRowStart(n.map(|n| n as i16)));
        }
        "grid-column-start" => {
            let n = p.try_parse(|p| p.expect_integer()).ok();
            while p.next().is_ok() {}
            out.push(D::GridColumnStart(n.map(|n| n as i16)));
        }
        "grid-column" | "grid-column-end" => {
            if name == "grid-column"
                && let Ok(n) = p.try_parse(|p| p.expect_integer())
            {
                // `grid-column: 2 / 4`: a start line (and maybe an end).
                out.push(D::GridColumnStart(Some(n as i16)));
            }
            let mut span = None;
            let mut values = Vec::new();
            while !p.is_exhausted() {
                match p.next()?.clone() {
                    Token::Ident(id) if id.eq_ignore_ascii_case("span") => {
                        if let Ok(n) = p.expect_integer() {
                            span = Some(GridSpan::Span(n.clamp(1, 100) as u16));
                        }
                    }
                    Token::Number { int_value: Some(n), .. } => values.push(n),
                    _ => {}
                }
            }
            if values.last() == Some(&-1) {
                span = Some(GridSpan::Full);
            }
            out.push(D::GridColumn(span));
        }

        _ => {
            // Unsupported property: consume it so it isn't "invalid", just ignored.
            while p.next().is_ok() {}
        }
    }
    Ok(())
}

impl Overflow {
    fn max_with(self, other: Overflow) -> Overflow {
        use Overflow::*;
        match (self, other) {
            (Hidden, _) | (_, Hidden) => Hidden,
            (Scroll, _) | (_, Scroll) => Scroll,
            _ => Visible,
        }
    }
}

/// Icon fonts put glyphs in the Private Use Area; without the font they're tofu.
fn strip_private_use(s: &str) -> String {
    s.chars().filter(|&c| !matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0x10FFFF)).collect()
}

fn parse_content<'i>(p: &mut Parser<'i>) -> PResult<'i, Option<Arc<[ContentItem]>>> {
    if p.try_parse(|p| p.expect_ident_matching("none").or_else(|_| p.expect_ident_matching("normal"))).is_ok() {
        return Ok(None);
    }
    let mut items = Vec::new();
    while !p.is_exhausted() {
        match p.next()?.clone() {
            Token::QuotedString(s) => items.push(ContentItem::Str(strip_private_use(&s).into())),
            Token::Ident(id) => match id.to_ascii_lowercase().as_str() {
                "open-quote" => items.push(ContentItem::OpenQuote),
                "close-quote" => items.push(ContentItem::CloseQuote),
                _ => {} // no-open-quote etc.
            },
            Token::Function(f) if f.eq_ignore_ascii_case("attr") => {
                let name = p.parse_nested_block(|p| {
                    let name = p.expect_ident()?.to_ascii_lowercase();
                    while p.next().is_ok() {}
                    Ok(name)
                })?;
                items.push(ContentItem::Attr(name.into()));
            }
            // `content: "x" / "alt text"`: the alt part is for screen readers.
            Token::Delim('/') => {
                while p.next().is_ok() {}
            }
            _ => {} // counter(), url(), image-set()...: not supported yet
        }
    }
    Ok(Some(items.into()))
}

fn parse_display<'i>(p: &mut Parser<'i>) -> PResult<'i, Display> {
    let first = ident(p)?;
    let second = p.try_parse(ident).ok();
    let d = match (first.as_str(), second.as_deref()) {
        ("none", _) => Display::None,
        ("contents", _) => Display::Contents,
        ("block" | "flow-root" | "run-in" | "table-caption" | "table-row-group" | "table-header-group"
        | "table-footer-group", None) => Display::Block,
        ("inline", None) | ("ruby" | "ruby-text" | "ruby-base", _) => Display::Inline,
        ("inline-block", _) | ("inline", Some("flow-root")) => Display::InlineBlock,
        ("list-item", _) => Display::ListItem,
        ("inline-flex" | "-webkit-inline-box" | "-webkit-inline-flex" | "-ms-inline-flexbox", _) | ("inline", Some("flex")) => {
            Display::InlineFlex
        }
        ("flex" | "-webkit-box" | "-webkit-flex" | "-ms-flexbox", _) | (_, Some("flex")) => Display::Flex,
        ("inline-grid" | "-ms-inline-grid", _) | ("inline", Some("grid")) => Display::InlineGrid,
        ("grid" | "-ms-grid", _) | (_, Some("grid")) => Display::Grid,
        ("table" | "inline-table", _) => Display::Table,
        ("table-row", _) => Display::TableRow,
        ("table-cell", _) => Display::TableCell,
        ("table-column" | "table-column-group", _) => Display::None,
        ("block", Some(_)) => Display::Block,
        _ => return err(p),
    };
    Ok(d)
}

fn parse_flex_direction<'i>(p: &mut Parser<'i>) -> PResult<'i, FlexDirection> {
    keyword(p, &[
        ("row", FlexDirection::Row),
        ("row-reverse", FlexDirection::RowReverse),
        ("column", FlexDirection::Column),
        ("column-reverse", FlexDirection::ColumnReverse),
    ])
}

fn parse_justify<'i>(p: &mut Parser<'i>) -> PResult<'i, Justify> {
    // `safe`/`unsafe` prefixes
    let _ = p.try_parse(|p| p.expect_ident_matching("safe").or_else(|_| p.expect_ident_matching("unsafe")));
    keyword(p, &[
        ("normal", Justify::Start),
        ("flex-start", Justify::Start),
        ("start", Justify::Start),
        ("left", Justify::Start),
        ("stretch", Justify::Start),
        ("center", Justify::Center),
        ("flex-end", Justify::End),
        ("end", Justify::End),
        ("right", Justify::End),
        ("space-between", Justify::SpaceBetween),
        ("space-around", Justify::SpaceAround),
        ("space-evenly", Justify::SpaceEvenly),
    ])
}

fn parse_align<'i>(p: &mut Parser<'i>) -> PResult<'i, Align> {
    let _ = p.try_parse(|p| p.expect_ident_matching("safe").or_else(|_| p.expect_ident_matching("unsafe")));
    let _ = p.try_parse(|p| p.expect_ident_matching("first").or_else(|_| p.expect_ident_matching("last")));
    keyword(p, &[
        ("normal", Align::Stretch),
        ("stretch", Align::Stretch),
        ("flex-start", Align::Start),
        ("start", Align::Start),
        ("self-start", Align::Start),
        ("center", Align::Center),
        ("flex-end", Align::End),
        ("end", Align::End),
        ("self-end", Align::End),
        ("baseline", Align::Baseline),
    ])
}

fn parse_flex<'i>(p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        out.extend([Decl::FlexGrow(0.), Decl::FlexShrink(0.), Decl::FlexBasis(LenSpec::Auto)]);
        return Ok(());
    }
    if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
        out.extend([Decl::FlexGrow(1.), Decl::FlexShrink(1.), Decl::FlexBasis(LenSpec::Auto)]);
        return Ok(());
    }
    let mut grow = None;
    let mut shrink = None;
    let mut basis = None;
    while !p.is_exhausted() {
        if let Ok(n) = p.try_parse(|p| p.expect_number()) {
            if grow.is_none() {
                grow = Some(n);
            } else if shrink.is_none() {
                shrink = Some(n);
            } else if n == 0.0 && basis.is_none() {
                // `flex: 1 1 0`: a unitless zero basis.
                basis = Some(LenSpec::px(0.0));
            } else {
                return err(p);
            }
        } else if basis.is_none() {
            basis = Some(parse_len(p)?);
        } else {
            return err(p);
        }
    }
    let (grow, shrink, basis) = match (grow, shrink, basis) {
        (None, None, Some(b)) => (1.0, 1.0, b),
        (g, s, b) => (g.unwrap_or(1.0), s.unwrap_or(1.0), b.unwrap_or(LenSpec::Lin(Lin::pct(0.0)))),
    };
    out.extend([Decl::FlexGrow(grow), Decl::FlexShrink(shrink), Decl::FlexBasis(basis)]);
    Ok(())
}

/// `grid-template-columns`: the column count (for equal columns) and, if
/// the tracks differ, the tracks themselves.
fn parse_template_columns<'i>(p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let start = p.state();
    let tracks = parse_tracks(p).ok().flatten();
    p.reset(&start);
    out.push(Decl::GridColumns(parse_grid_columns(p)?));
    let uniform = tracks.as_ref().is_some_and(|t| t.windows(2).all(|w| w[0] == w[1]));
    out.push(Decl::GridTracks(if uniform { None } else { tracks.map(Into::into) }));
    Ok(())
}

/// Explicit tracks; `None` for anything we can't list (`auto-fill`).
fn parse_tracks<'i>(p: &mut Parser<'i>) -> PResult<'i, Option<Vec<TrackSpec>>> {
    fn track<'i>(p: &mut Parser<'i>) -> PResult<'i, Option<TrackSpec>> {
        Ok(Some(match p.next()?.clone() {
            Token::SquareBracketBlock => return Ok(None),
            Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("fr") => TrackSpec::Fr(value),
            Token::Dimension { value, unit, .. } => match unit_lin(value, &unit) {
                Some(l) => TrackSpec::Len(LenSpec::Lin(l)),
                None => TrackSpec::Auto,
            },
            Token::Number { value: 0.0, .. } => TrackSpec::Len(LenSpec::px(0.0)),
            Token::Function(f) if f.eq_ignore_ascii_case("minmax") => p.parse_nested_block(|p| {
                while !p.is_exhausted() && !matches!(p.next()?, Token::Comma) {}
                Ok(match p.next()?.clone() {
                    Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("fr") => TrackSpec::Fr(value),
                    Token::Dimension { value, unit, .. } => match unit_lin(value, &unit) {
                        Some(l) => TrackSpec::Max(LenSpec::Lin(l)),
                        None => TrackSpec::Auto,
                    },
                    _ => TrackSpec::Auto,
                })
            })?,
            Token::Function(_) => {
                p.parse_nested_block(|p| {
                    while p.next().is_ok() {}
                    Ok(())
                })?;
                TrackSpec::Auto
            }
            _ => TrackSpec::Auto,
        }))
    }
    let mut out = Vec::new();
    while !p.is_exhausted() {
        let state = p.state();
        if let Ok(Token::Function(f)) = p.next().cloned()
            && f.eq_ignore_ascii_case("repeat")
        {
            let repeated = p.parse_nested_block(|p| {
                let n = match p.next()?.clone() {
                    Token::Number { int_value: Some(n), .. } => n.clamp(1, 64) as usize,
                    _ => return Ok(None), // auto-fill / auto-fit
                };
                p.expect_comma()?;
                let mut inner = Vec::new();
                while !p.is_exhausted() {
                    if let Some(t) = track(p)? {
                        inner.push(t);
                    }
                }
                Ok(Some((n, inner)))
            })?;
            let Some((n, inner)) = repeated else { return Ok(None) };
            for _ in 0..n {
                out.extend(inner.iter().cloned());
            }
            continue;
        }
        p.reset(&state);
        if let Some(t) = track(p)? {
            out.push(t);
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

fn area_row(row: &str) -> Arc<[Arc<str>]> {
    row.split_whitespace().map(Arc::from).collect::<Vec<_>>().into()
}

fn parse_areas(p: &mut Parser) -> Option<GridAreas> {
    let mut rows = Vec::new();
    while let Ok(token) = p.next() {
        if let Token::QuotedString(row) = token {
            rows.push(area_row(row));
        }
    }
    (!rows.is_empty()).then(|| rows.into())
}

fn parse_grid_columns<'i>(p: &mut Parser<'i>) -> PResult<'i, GridCols> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        return Ok(GridCols::None);
    }
    if p.try_parse(|p| p.expect_ident_matching("subgrid")).is_ok() || p.try_parse(|p| p.expect_ident_matching("masonry")).is_ok() {
        while p.next().is_ok() {}
        return Ok(GridCols::None);
    }
    let mut count: u32 = 0;
    let mut auto_fill = None;
    let mut fit = false;
    while !p.is_exhausted() {
        match p.next()?.clone() {
            Token::SquareBracketBlock => {} // line names
            Token::Function(name) if name.eq_ignore_ascii_case("repeat") => {
                p.parse_nested_block(|p| {
                    let n = match p.next()?.clone() {
                        Token::Number { int_value: Some(n), .. } => Some(n.max(1) as u32),
                        Token::Ident(ref i) => {
                            fit = i.eq_ignore_ascii_case("auto-fit");
                            None
                        }
                        _ => return err(p),
                    };
                    p.expect_comma()?;
                    let mut inner = 0u32;
                    let mut min = None;
                    while !p.is_exhausted() {
                        match p.next()?.clone() {
                            Token::SquareBracketBlock => {}
                            Token::Function(f) if f.eq_ignore_ascii_case("minmax") => {
                                inner += 1;
                                if min.is_none() {
                                    min = p.parse_nested_block(|p| {
                                        let m = parse_len(p);
                                        while p.next().is_ok() {}
                                        Ok(m.ok())
                                    })?;
                                }
                            }
                            Token::Dimension { value, unit, .. } if min.is_none() && !unit.eq_ignore_ascii_case("fr") => {
                                inner += 1;
                                min = unit_lin(value, &unit).map(LenSpec::Lin);
                            }
                            _ => inner += 1,
                        }
                    }
                    match n {
                        Some(n) => count += n * inner.max(1),
                        None => auto_fill = Some(min.unwrap_or(LenSpec::px(200.0))),
                    }
                    Ok(())
                })?;
            }
            _ => count += 1,
        }
    }
    Ok(match auto_fill {
        Some(min) if count == 0 => GridCols::AutoFill(min, fit),
        _ => GridCols::Count(count.clamp(1, 64) as u16),
    })
}

/// `150ms`, `.3s`, `0`.
fn parse_time<'i>(p: &mut Parser<'i>) -> PResult<'i, f32> {
    match p.next()?.clone() {
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("ms") => Ok(value / 1000.0),
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("s") => Ok(value),
        Token::Number { value: 0.0, .. } => Ok(0.0),
        _ => err(p),
    }
}

fn parse_easing<'i>(p: &mut Parser<'i>) -> PResult<'i, Easing> {
    match p.next()?.clone() {
        Token::Ident(id) => Ok(match &*id.to_ascii_lowercase() {
            "linear" => Easing::LINEAR,
            "ease" => Easing::EASE,
            "ease-in" => Easing::CubicBezier(0.42, 0.0, 1.0, 1.0),
            "ease-out" => Easing::CubicBezier(0.0, 0.0, 0.58, 1.0),
            "ease-in-out" => Easing::CubicBezier(0.42, 0.0, 0.58, 1.0),
            "step-start" => Easing::Steps(1, true),
            "step-end" => Easing::Steps(1, false),
            _ => return err(p),
        }),
        Token::Function(f) if f.eq_ignore_ascii_case("cubic-bezier") => p.parse_nested_block(|p| {
            let mut v = [0.0f32; 4];
            for (i, slot) in v.iter_mut().enumerate() {
                if i > 0 {
                    p.expect_comma()?;
                }
                *slot = p.expect_number()?;
            }
            Ok(Easing::CubicBezier(v[0], v[1], v[2], v[3]))
        }),
        Token::Function(f) if f.eq_ignore_ascii_case("steps") => p.parse_nested_block(|p| {
            let n = p.expect_integer()?.max(1) as u32;
            let start = p
                .try_parse(|p| {
                    p.expect_comma()?;
                    p.expect_ident_cloned()
                })
                .is_ok_and(|id| matches!(&*id.to_ascii_lowercase(), "start" | "jump-start" | "jump-both"));
            Ok(Easing::Steps(n, start))
        }),
        Token::Function(f) if f.eq_ignore_ascii_case("linear") => p.parse_nested_block(|p| {
            while p.next().is_ok() {}
            Ok(Easing::LINEAR)
        }),
        _ => err(p),
    }
}

fn parse_iterations<'i>(p: &mut Parser<'i>) -> PResult<'i, f32> {
    match p.next()?.clone() {
        Token::Ident(id) if id.eq_ignore_ascii_case("infinite") => Ok(f32::INFINITY),
        Token::Number { value, .. } if value >= 0.0 => Ok(value),
        _ => err(p),
    }
}

fn parse_direction<'i>(p: &mut Parser<'i>) -> PResult<'i, AnimDirection> {
    keyword(p, &[
        ("normal", AnimDirection::Normal),
        ("reverse", AnimDirection::Reverse),
        ("alternate", AnimDirection::Alternate),
        ("alternate-reverse", AnimDirection::AlternateReverse),
    ])
}

/// `animation-fill-mode` → (forwards, backwards).
fn parse_fill<'i>(p: &mut Parser<'i>) -> PResult<'i, (bool, bool)> {
    keyword(p, &[("none", (false, false)), ("forwards", (true, false)), ("backwards", (false, true)), ("both", (true, true))])
}

fn parse_list_style_type<'i>(p: &mut Parser<'i>) -> PResult<'i, ListStyle> {
    keyword(p, &[
        ("none", ListStyle::None),
        ("disc", ListStyle::Disc),
        ("circle", ListStyle::Circle),
        ("square", ListStyle::Square),
        ("decimal", ListStyle::Decimal),
        ("decimal-leading-zero", ListStyle::Decimal),
        ("lower-alpha", ListStyle::LowerAlpha),
        ("lower-latin", ListStyle::LowerAlpha),
        ("upper-alpha", ListStyle::UpperAlpha),
        ("upper-latin", ListStyle::UpperAlpha),
        ("lower-roman", ListStyle::LowerRoman),
        ("upper-roman", ListStyle::UpperRoman),
        ("disclosure-open", ListStyle::Disc),
        ("disclosure-closed", ListStyle::Disc),
    ])
}

fn parse_text_decoration<'i>(p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let mut underline = false;
    let mut strikethrough = false;
    while !p.is_exhausted() {
        // Colors and thickness are ignored; so are `none`, `overline` and styles.
        if let Token::Ident(id) = p.next()?.clone() {
            match id.to_ascii_lowercase().as_str() {
                "underline" => underline = true,
                "line-through" => strikethrough = true,
                _ => {}
            }
        }
    }
    out.push(Decl::TextDecoration { underline, strikethrough });
    Ok(())
}

// ---------------------------------------------------------------------------
// Borders

fn parse_border_style<'i>(p: &mut Parser<'i>) -> PResult<'i, BorderStyle> {
    keyword(p, &[
        ("none", BorderStyle::None),
        ("hidden", BorderStyle::None),
        ("solid", BorderStyle::Solid),
        ("dashed", BorderStyle::Dashed),
        ("dotted", BorderStyle::Dotted),
        ("double", BorderStyle::Double),
        ("groove", BorderStyle::Other),
        ("ridge", BorderStyle::Other),
        ("inset", BorderStyle::Other),
        ("outset", BorderStyle::Other),
    ])
}

fn parse_border_width<'i>(p: &mut Parser<'i>) -> PResult<'i, LenSpec> {
    if let Ok(k) = p.try_parse(|p| keyword(p, &[("thin", 1.0), ("medium", 3.0), ("thick", 5.0)])) {
        return Ok(LenSpec::px(k));
    }
    parse_len(p)
}

fn parse_border_inner<'i>(rest: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    // border-radius family
    if rest == "-radius" {
        let v = four(p, parse_len)?;
        // Elliptical radii (`a / b`): ignore the vertical part.
        if p.try_parse(|p| p.expect_delim('/')).is_ok() {
            while p.next().is_ok() {}
        }
        for (i, r) in v.into_iter().enumerate() {
            out.push(Decl::BorderRadius(i, r));
        }
        return Ok(());
    }
    let corner = match rest {
        "-top-left-radius" | "-start-start-radius" => Some(0),
        "-top-right-radius" | "-start-end-radius" => Some(1),
        "-bottom-right-radius" | "-end-end-radius" => Some(2),
        "-bottom-left-radius" | "-end-start-radius" => Some(3),
        _ => None,
    };
    if let Some(corner) = corner {
        let r = parse_len(p)?;
        let _ = p.try_parse(parse_len);
        out.push(Decl::BorderRadius(corner, r));
        return Ok(());
    }

    // Split into sides + which component(s).
    let (sides, component): (&[Side], &str) = match rest {
        "" => (&Side::ALL, ""),
        "-width" | "-style" | "-color" => (&Side::ALL, rest),
        _ => {
            let rest = rest.strip_prefix('-').unwrap_or(rest);
            let (side_name, component) = match rest.rsplit_once('-') {
                Some((s, c @ ("width" | "style" | "color"))) => (s, c),
                _ => (rest, ""),
            };
            let Some(sides) = logical_sides(side_name) else {
                while p.next().is_ok() {}
                return Ok(()); // border-collapse, border-spacing, border-image...
            };
            (sides, component)
        }
    };

    match component.trim_start_matches('-') {
        "width" if sides.len() == 4 => {
            for (side, w) in Side::ALL.into_iter().zip(four(p, parse_border_width)?) {
                out.push(Decl::BorderWidth(side, w));
            }
        }
        "style" if sides.len() == 4 => {
            for (side, s) in Side::ALL.into_iter().zip(four(p, parse_border_style)?) {
                out.push(Decl::BorderStyle(side, s));
            }
        }
        "color" if sides.len() == 4 => {
            for (side, c) in Side::ALL.into_iter().zip(four(p, parse_color)?) {
                out.push(Decl::BorderColor(side, c));
            }
        }
        "width" => {
            let w = parse_border_width(p)?;
            sides.iter().for_each(|&s| out.push(Decl::BorderWidth(s, w.clone())));
        }
        "style" => {
            let st = parse_border_style(p)?;
            sides.iter().for_each(|&s| out.push(Decl::BorderStyle(s, st)));
        }
        "color" => {
            let c = parse_color(p)?;
            sides.iter().for_each(|&s| out.push(Decl::BorderColor(s, c)));
        }
        _ => {
            // Shorthand: <width> || <style> || <color>, any order.
            let mut width = None;
            let mut style = None;
            let mut color = None;
            while !p.is_exhausted() {
                if style.is_none()
                    && let Ok(s) = p.try_parse(parse_border_style)
                {
                    style = Some(s);
                } else if width.is_none()
                    && let Ok(w) = p.try_parse(parse_border_width)
                {
                    width = Some(w);
                } else if color.is_none() {
                    color = Some(parse_color(p)?);
                } else {
                    return err(p);
                }
            }
            for &side in sides {
                out.push(Decl::BorderWidth(side, width.clone().unwrap_or(LenSpec::px(3.0))));
                out.push(Decl::BorderStyle(side, style.unwrap_or(BorderStyle::None)));
                out.push(Decl::BorderColor(side, color.unwrap_or(Color::CurrentColor)));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Backgrounds

/// One color standing in for a gradient: linear gradients by their first
/// stop; radial ones (dot patterns, vignettes) by their last, which is
/// usually what fills most of the box.
fn gradient_color<'i>(p: &mut Parser<'i>, radial: bool) -> PResult<'i, Color> {
    p.parse_nested_block(|p| {
        let mut stops = Vec::new();
        loop {
            if let Ok(c) = p.try_parse(parse_color) {
                stops.push(c);
                continue;
            }
            if p.next().is_err() {
                break;
            }
        }
        // A transparent stop: a pattern (grid lines, dots) or a fade
        // overlay. One solid color would cover the box; drawing none is
        // closer.
        if stops.iter().any(|c| c.is_transparent()) {
            return Ok(Color::TRANSPARENT);
        }
        let pick = if radial { stops.last() } else { stops.first() };
        pick.copied().ok_or_else(|| p.new_error_for_next_token())
    })
}

/// What we understand of a `background` / `mask` value (first image layer).
#[derive(Default)]
struct ImageLayer {
    color: Option<Color>,
    url: Option<Arc<str>>,
    gradient: Option<Color>,
    /// Every layer's gradient, top first.
    full: Vec<Gradient>,
    repeat: Option<BgRepeat>,
    /// An image we can't use (e.g. `image-set()`), still meaning "masked".
    other_image: bool,
    position: Option<BgPos>,
    size: Option<BgSizeSpec>,
}

fn parse_image_layer<'i>(p: &mut Parser<'i>) -> PResult<'i, ImageLayer> {
    let mut layer = ImageLayer::default();
    while !p.is_exhausted() {
        if let Ok(c) = p.try_parse(parse_color) {
            layer.color = Some(c);
            continue;
        }
        if let Ok(url) = p.try_parse(parse_url) {
            if layer.url.is_none() {
                layer.url = url;
            }
            continue;
        }
        if layer.position.is_none()
            && let Ok(pos) = p.try_parse(parse_bg_position)
        {
            layer.position = Some(pos);
            if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                layer.size = p.try_parse(parse_bg_size).ok();
            }
            continue;
        }
        // Other tokens (attachment, origin, commas between layers) are skipped.
        let token = p.next()?.clone();
        if let Token::Ident(id) = &token {
            let r = match id.to_ascii_lowercase().as_str() {
                "no-repeat" => Some(BgRepeat { x: false, y: false }),
                "repeat-x" => Some(BgRepeat { x: true, y: false }),
                "repeat-y" => Some(BgRepeat { x: false, y: true }),
                "repeat" | "space" | "round" => Some(BgRepeat { x: true, y: true }),
                _ => None,
            };
            if layer.repeat.is_none() {
                layer.repeat = r;
            }
        }
        if let Token::Function(name) = token {
            let name = name.to_ascii_lowercase();
            if name.contains("gradient") {
                let state = p.state();
                if layer.gradient.is_none() {
                    layer.gradient = gradient_color(p, name.contains("radial")).ok();
                }
                p.reset(&state);
                if let Ok(g) = parse_gradient(p, &name) {
                    layer.full.push(g);
                }
            } else if name.contains("image") || name == "element" || name == "paint" {
                layer.other_image = true;
            }
        }
    }
    Ok(layer)
}

impl ImageLayer {
    fn gradients(&self) -> Option<Arc<[Gradient]>> {
        (!self.full.is_empty()).then(|| self.full.clone().into())
    }
}

/// A whole `*-gradient()`: the shape and every color stop. (Positioned
/// just after the function token.)
fn parse_gradient<'i>(p: &mut Parser<'i>, name: &str) -> PResult<'i, Gradient> {
    let repeating = name.starts_with("repeating-");
    let radial = name.contains("radial");
    let conic = name.contains("conic");
    if !radial && !conic && !name.contains("linear") {
        return err(p);
    }
    p.parse_nested_block(|p| {
        let mut shape = if conic {
            GradientShape::Conic { from: 0.0, at: BgPos::CENTER }
        } else if radial {
            GradientShape::Radial { circle: false, size: RadialSize::FarthestCorner, at: BgPos::CENTER }
        } else {
            GradientShape::Linear(180.0)
        };
        // The part before the first color, if any.
        let state = p.state();
        let starts_with_color = parse_color(p).is_ok();
        p.reset(&state);
        if !starts_with_color {
            if conic {
                let (mut from, mut at) = (0.0, BgPos::CENTER);
                while !p.is_exhausted() {
                    match p.next()?.clone() {
                        Token::Comma => break,
                        Token::Ident(id) if id.eq_ignore_ascii_case("from") => from = p.try_parse(parse_angle).unwrap_or(0.0),
                        Token::Ident(id) if id.eq_ignore_ascii_case("at") => at = parse_bg_position(p).unwrap_or(BgPos::CENTER),
                        _ => {}
                    }
                }
                shape = GradientShape::Conic { from, at };
            } else if radial {
                let (mut circle, mut size, mut at) = (false, RadialSize::FarthestCorner, BgPos::CENTER);
                let mut lengths = Vec::new();
                while !p.is_exhausted() {
                    let token = p.next()?.clone();
                    match token {
                        Token::Comma => break,
                        Token::Ident(id) => match id.to_ascii_lowercase().as_str() {
                            "circle" => circle = true,
                            "ellipse" => circle = false,
                            "closest-side" => size = RadialSize::ClosestSide,
                            "farthest-side" => size = RadialSize::FarthestSide,
                            "closest-corner" => size = RadialSize::ClosestCorner,
                            "farthest-corner" => size = RadialSize::FarthestCorner,
                            "at" => at = parse_bg_position(p).unwrap_or(BgPos::CENTER),
                            _ => {}
                        },
                        Token::Dimension { value, unit, .. } => lengths.push(stop_px(value, &unit)),
                        _ => {}
                    }
                }
                if let Some(&rx) = lengths.first() {
                    let ry = lengths.get(1).copied().unwrap_or(rx);
                    if lengths.len() == 1 {
                        circle = true;
                    }
                    size = RadialSize::Explicit(rx, ry);
                }
                shape = GradientShape::Radial { circle, size, at };
            } else {
                if let Ok(deg) = p.try_parse(parse_angle) {
                    shape = GradientShape::Linear(deg);
                } else if p.try_parse(|p| p.expect_ident_matching("to")).is_ok() {
                    let (mut x, mut y) = (0.0f32, 0.0f32);
                    while let Ok(side) = p.try_parse(|p| p.expect_ident().cloned()) {
                        match side.to_ascii_lowercase().as_str() {
                            "left" => x = -1.0,
                            "right" => x = 1.0,
                            "top" => y = -1.0,
                            "bottom" => y = 1.0,
                            _ => {}
                        }
                    }
                    shape = match (x, y) {
                        (0.0, y) => GradientShape::Linear(if y < 0.0 { 0.0 } else { 180.0 }),
                        (x, 0.0) => GradientShape::Linear(if x > 0.0 { 90.0 } else { 270.0 }),
                        (x, y) => GradientShape::LinearCorner(x, y),
                    };
                }
                p.expect_comma()?;
            }
        }
        let mut stops = Vec::new();
        while !p.is_exhausted() {
            match p.try_parse(parse_color) {
                Ok(color) => {
                    let mut positions = Vec::new();
                    while let Ok(pos) = p.try_parse(parse_stop_pos) {
                        positions.push(pos);
                    }
                    if positions.is_empty() {
                        stops.push(GradientStop { color, pos: None });
                    }
                    for pos in positions.into_iter().take(2) {
                        stops.push(GradientStop { color, pos: Some(pos) });
                    }
                }
                // A color hint (`10%` alone): ignored.
                Err(_) => {
                    while let Ok(t) = p.next() {
                        if matches!(t, Token::Comma) {
                            break;
                        }
                    }
                    continue;
                }
            }
            if p.is_exhausted() {
                break;
            }
            p.expect_comma()?;
        }
        if stops.len() < 2 {
            return err(p);
        }
        Ok(Gradient { shape, stops, repeating })
    })
}

fn stop_px(value: f32, unit: &str) -> f32 {
    match unit.to_ascii_lowercase().as_str() {
        "em" | "rem" => value * 16.0,
        // Conic stops: angles, as degrees.
        "turn" => value * 360.0,
        "rad" => value.to_degrees(),
        "grad" => value * 0.9,
        _ => value,
    }
}

fn parse_stop_pos<'i>(p: &mut Parser<'i>) -> PResult<'i, StopPos> {
    match p.next()?.clone() {
        Token::Percentage { unit_value, .. } => Ok(StopPos::Fraction(unit_value)),
        Token::Dimension { value, unit, .. } => Ok(StopPos::Px(stop_px(value, &unit))),
        Token::Number { value: 0.0, .. } => Ok(StopPos::Fraction(0.0)),
        _ => err(p),
    }
}

fn parse_bg_size<'i>(p: &mut Parser<'i>) -> PResult<'i, BgSizeSpec> {
    if p.try_parse(|p| p.expect_ident_matching("contain")).is_ok() {
        return Ok(BgSizeSpec::Contain);
    }
    if p.try_parse(|p| p.expect_ident_matching("cover")).is_ok() {
        return Ok(BgSizeSpec::Cover);
    }
    let w = parse_len(p)?;
    let h = p.try_parse(parse_len).unwrap_or(LenSpec::Auto);
    Ok(match (&w, &h) {
        (LenSpec::Auto, LenSpec::Auto) => BgSizeSpec::Auto,
        _ => BgSizeSpec::Explicit(w, h),
    })
}

fn parse_bg_position<'i>(p: &mut Parser<'i>) -> PResult<'i, BgPos> {
    let mut pos = BgPos::TOP_LEFT;
    let mut seen = 0;
    let mut x_set = false;
    while seen < 4 {
        let state = p.state();
        let fraction = match p.next() {
            Ok(Token::Ident(id)) => match id.to_ascii_lowercase().as_str() {
                "left" => Some(("x", 0.0)),
                "right" => Some(("x", 1.0)),
                "top" => Some(("y", 0.0)),
                "bottom" => Some(("y", 1.0)),
                "center" => Some(("c", 0.5)),
                _ => None,
            },
            Ok(Token::Percentage { unit_value, .. }) => Some(("n", *unit_value)),
            Ok(Token::Dimension { .. }) | Ok(Token::Number { .. }) => Some(("n", 0.0)),
            _ => None,
        };
        let Some((axis, v)) = fraction else {
            p.reset(&state);
            break;
        };
        match axis {
            "x" => {
                pos.x = v;
                x_set = true;
            }
            "y" => pos.y = v,
            "c" if !x_set && seen == 0 => {
                pos.x = v;
                pos.y = v;
            }
            "c" => pos.y = v,
            _ if !x_set => {
                pos.x = v;
                x_set = true;
            }
            _ => pos.y = v,
        }
        seen += 1;
    }
    if seen == 0 { err(p) } else { Ok(pos) }
}

fn parse_background<'i>(p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let layer = parse_image_layer(p)?;
    out.push(Decl::BackgroundColor(layer.color.unwrap_or(Color::TRANSPARENT)));
    out.push(Decl::BackgroundImage(layer.gradient));
    out.push(Decl::Gradients(layer.gradients()));
    out.push(Decl::BackgroundRepeat(layer.repeat.unwrap_or_default()));
    out.push(Decl::BackgroundUrl(layer.url));
    out.push(Decl::BackgroundSize(layer.size.unwrap_or(BgSizeSpec::Auto)));
    out.push(Decl::BackgroundPosition(layer.position.unwrap_or(BgPos::TOP_LEFT)));
    Ok(())
}

// ---------------------------------------------------------------------------
// Fonts

fn parse_weight<'i>(p: &mut Parser<'i>) -> PResult<'i, WeightSpec> {
    if let Ok(n) = p.try_parse(|p| p.expect_number()) {
        return if (1.0..=1000.0).contains(&n) { Ok(WeightSpec::Abs(n as u16)) } else { err(p) };
    }
    Ok(match ident(p)?.as_str() {
        "normal" => WeightSpec::Abs(400),
        "bold" => WeightSpec::Abs(700),
        "bolder" => WeightSpec::Bolder,
        "lighter" => WeightSpec::Lighter,
        _ => return err(p),
    })
}

fn parse_font_size<'i>(p: &mut Parser<'i>) -> PResult<'i, FontSizeSpec> {
    if let Ok(k) = p.try_parse(ident) {
        let px = match k.as_str() {
            "xx-small" => 9.0,
            "x-small" => 10.0,
            "small" => 13.0,
            "medium" => 16.0,
            "large" => 18.0,
            "x-large" => 24.0,
            "xx-large" => 32.0,
            "xxx-large" => 48.0,
            "smaller" => return Ok(FontSizeSpec::Smaller),
            "larger" => return Ok(FontSizeSpec::Larger),
            _ => return err(p),
        };
        return Ok(FontSizeSpec::Len(LenSpec::px(px)));
    }
    Ok(FontSizeSpec::Len(parse_len(p)?))
}

fn parse_line_height<'i>(p: &mut Parser<'i>) -> PResult<'i, LineHeightSpec> {
    if p.try_parse(|p| p.expect_ident_matching("normal")).is_ok() {
        return Ok(LineHeightSpec::Normal);
    }
    if let Ok(n) = p.try_parse(|p| p.expect_number()) {
        return Ok(LineHeightSpec::Number(n));
    }
    // `calc(1em + 4px)` or a plain length/percentage
    match parse_len(p)? {
        LenSpec::Lin(l) if l.pct != 0.0 && Lin { pct: 0.0, ..l }.is_zero() => {
            Ok(LineHeightSpec::Number(l.pct / 100.0))
        }
        other => Ok(LineHeightSpec::Len(other)),
    }
}

fn classify_family(name: &str) -> Option<FontFamily> {
    let n = name.to_ascii_lowercase();
    let n = n.trim();
    const MONO: &[&str] = &["mono", "courier", "consolas", "menlo", "monaco", "source code", "fira code", "jetbrains", "cascadia", "inconsolata", "sfmono"];
    const SERIF: &[&str] = &["georgia", "times", "palatino", "garamond", "cambria", "charter", "baskerville", "merriweather", "iowan", "bookman", "didot", "hoefler", "minion", "libertine", "lora", "playfair"];
    const SANS: &[&str] = &["system-ui", "-apple-system", "blinkmacsystemfont", "helvetica", "arial", "inter", "roboto", "segoe", "verdana", "tahoma", "lato", "ubuntu", "cantarell", "geneva", "lucida grande", "avenir", "futura", "gill sans", "trebuchet", "montserrat", "poppins", "nunito"];
    if n == "monospace" || MONO.iter().any(|m| n.contains(m)) {
        Some(FontFamily::Mono)
    } else if n.contains("sans") || SANS.iter().any(|s| n.contains(s)) {
        Some(FontFamily::Sans)
    } else if n.contains("serif") || SERIF.iter().any(|s| n.contains(s)) {
        Some(FontFamily::Serif)
    } else {
        None
    }
}

fn parse_font_family<'i>(p: &mut Parser<'i>) -> PResult<'i, (FontFamily, FontNames)> {
    let families = p.parse_comma_separated(|p| {
        if let Ok(s) = p.try_parse(|p| p.expect_string().map(|s| s.to_string())) {
            return Ok(s);
        }
        let mut name = String::new();
        while let Ok(id) = p.try_parse(|p| p.expect_ident().map(|i| i.to_string())) {
            if !name.is_empty() {
                name.push(' ');
            }
            name.push_str(&id);
        }
        if name.is_empty() { err(p) } else { Ok(name) }
    })?;
    let generic = families.iter().find_map(|f| classify_family(f)).unwrap_or(FontFamily::Sans);
    let names: Vec<Arc<str>> = families.iter().map(|f| Arc::from(f.as_str())).collect();
    Ok((generic, names.into()))
}

fn parse_font<'i>(p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    // System fonts: `font: menu` etc.
    if let Ok(()) = p.try_parse(|p| {
        keyword(p, &[("caption", ()), ("icon", ()), ("menu", ()), ("message-box", ()), ("small-caption", ()), ("status-bar", ())])
    }) {
        out.extend([
            Decl::FontSize(FontSizeSpec::Len(LenSpec::px(13.0))),
            Decl::FontFamily(FontFamily::Sans),
            Decl::FontNames(FontNames::from([])),
            Decl::FontWeight(WeightSpec::Abs(400)),
            Decl::Italic(false),
            Decl::LineHeight(LineHeightSpec::Normal),
        ]);
        return Ok(());
    }
    let mut italic = false;
    let mut weight = WeightSpec::Abs(400);
    for _ in 0..4 {
        if let Ok(k) = p.try_parse(ident) {
            match k.as_str() {
                "normal" | "small-caps" | "condensed" | "expanded" | "semi-condensed" | "semi-expanded" => continue,
                "italic" | "oblique" => {
                    italic = true;
                    continue;
                }
                "bold" => weight = WeightSpec::Abs(700),
                "bolder" => weight = WeightSpec::Bolder,
                "lighter" => weight = WeightSpec::Lighter,
                _ => return err(p),
            }
            continue;
        }
        if let Ok(n) = p.try_parse(|p| p.expect_number()) {
            weight = WeightSpec::Abs(n.clamp(1.0, 1000.0) as u16);
            continue;
        }
        break;
    }
    let size = parse_font_size(p)?;
    let line_height = if p.try_parse(|p| p.expect_delim('/')).is_ok() {
        parse_line_height(p)?
    } else {
        LineHeightSpec::Normal
    };
    let (family, names) = parse_font_family(p)?;
    out.extend([
        Decl::FontSize(size),
        Decl::LineHeight(line_height),
        Decl::FontWeight(weight),
        Decl::Italic(italic),
        Decl::FontFamily(family),
        Decl::FontNames(names),
    ]);
    Ok(())
}

// ---------------------------------------------------------------------------
// Lengths & calc()

pub(crate) fn unit_lin(value: f32, unit: &str) -> Option<Lin> {
    let mut l = Lin::default();
    match unit.to_ascii_lowercase().as_str() {
        "px" => l.px = value,
        "pt" => l.px = value * 4.0 / 3.0,
        "pc" => l.px = value * 16.0,
        "in" => l.px = value * 96.0,
        "cm" => l.px = value * 96.0 / 2.54,
        "mm" => l.px = value * 96.0 / 25.4,
        "q" => l.px = value * 96.0 / 101.6,
        "em" | "cap" | "ic" | "lh" => l.em = value,
        "ex" | "ch" => l.em = value * 0.5,
        "rem" | "rlh" | "rch" | "rex" => l.rem = value,
        "vw" | "svw" | "lvw" | "dvw" | "vi" | "svi" | "lvi" | "dvi" | "cqw" | "cqi" => l.vw = value,
        "vh" | "svh" | "lvh" | "dvh" | "vb" | "svb" | "lvb" | "dvb" | "cqh" | "cqb" => l.vh = value,
        "vmin" | "svmin" | "lvmin" | "dvmin" | "cqmin" => l.vmin = value,
        "vmax" | "svmax" | "lvmax" | "dvmax" | "cqmax" => l.vmax = value,
        _ => return None,
    }
    Some(l)
}

/// An angle in degrees (`45deg`, `0.5turn`, `1rad`, `100grad`, `0`).
fn parse_angle<'i>(p: &mut Parser<'i>) -> PResult<'i, f32> {
    match p.next()?.clone() {
        Token::Dimension { value, unit, .. } => Ok(match unit.to_ascii_lowercase().as_str() {
            "deg" => value,
            "rad" => value.to_degrees(),
            "turn" => value * 360.0,
            "grad" => value * 0.9,
            _ => return err(p),
        }),
        Token::Number { value: 0.0, .. } => Ok(0.0),
        _ => err(p),
    }
}

/// A scale factor: `1.5` or `150%`.
fn parse_factor<'i>(p: &mut Parser<'i>) -> PResult<'i, f32> {
    match p.next()?.clone() {
        Token::Number { value, .. } => Ok(value),
        Token::Percentage { unit_value, .. } => Ok(unit_value),
        _ => err(p),
    }
}

pub(crate) fn parse_len<'i>(p: &mut Parser<'i>) -> PResult<'i, LenSpec> {
    let token = p.next()?.clone();
    match token {
        Token::Dimension { value, ref unit, .. } => match unit_lin(value, unit) {
            Some(l) => Ok(LenSpec::Lin(l)),
            None => err(p),
        },
        Token::Percentage { unit_value, .. } => Ok(LenSpec::Lin(Lin::pct(unit_value * 100.0))),
        Token::Number { value: 0.0, .. } => Ok(LenSpec::px(0.0)),
        Token::Ident(ref id) => match id.to_ascii_lowercase().as_str() {
            "auto" | "none" | "min-content" | "max-content" | "fit-content" | "stretch"
            | "-webkit-fill-available" | "-moz-available" | "-moz-fit-content" | "-webkit-fit-content" => {
                Ok(LenSpec::Auto)
            }
            _ => err(p),
        },
        Token::Function(ref name) => {
            let name = name.to_ascii_lowercase();
            p.parse_nested_block(|p| match name.as_str() {
                "calc" | "-webkit-calc" | "-moz-calc" => calc_to_len(p),
                "min" | "max" => {
                    let items = p.parse_comma_separated(calc_or_len)?;
                    Ok(if name == "min" { LenSpec::Min(items) } else { LenSpec::Max(items) })
                }
                "clamp" => {
                    let lo = calc_or_len(p)?;
                    p.expect_comma()?;
                    let pref = calc_or_len(p)?;
                    p.expect_comma()?;
                    let hi = calc_or_len(p)?;
                    Ok(LenSpec::Clamp(Box::new([lo, pref, hi])))
                }
                "fit-content" => {
                    while p.next().is_ok() {}
                    Ok(LenSpec::Auto)
                }
                _ => err(p),
            })
        }
        _ => err(p),
    }
}

fn calc_or_len<'i>(p: &mut Parser<'i>) -> PResult<'i, LenSpec> {
    p.try_parse(calc_to_len).or_else(|_| parse_len(p))
}

fn calc_to_len<'i>(p: &mut Parser<'i>) -> PResult<'i, LenSpec> {
    match calc_sum(p)? {
        CalcVal::Len(l) => Ok(LenSpec::Lin(l)),
        CalcVal::Num(0.0) => Ok(LenSpec::px(0.0)),
        CalcVal::Num(_) => err(p),
        CalcVal::Terms(terms) => Ok(LenSpec::Sum(terms)),
    }
}

#[derive(Clone)]
enum CalcVal {
    Num(f32),
    Len(Lin),
    /// Lengths that aren't linear (`min()`/`max()`/`clamp()` inside): Σ k·term.
    Terms(Vec<(f32, LenSpec)>),
}

impl CalcVal {
    /// As a sum of terms (numbers only as `0`).
    fn terms(self) -> Option<Vec<(f32, LenSpec)>> {
        match self {
            CalcVal::Terms(t) => Some(t),
            CalcVal::Len(l) => Some(vec![(1.0, LenSpec::Lin(l))]),
            CalcVal::Num(0.0) => Some(Vec::new()),
            CalcVal::Num(_) => None,
        }
    }

    fn scaled(self, k: f32) -> CalcVal {
        match self {
            CalcVal::Num(a) => CalcVal::Num(a * k),
            CalcVal::Len(l) => CalcVal::Len(l.scale(k)),
            CalcVal::Terms(t) => CalcVal::Terms(t.into_iter().map(|(a, s)| (a * k, s)).collect()),
        }
    }
}

fn calc_sum<'i>(p: &mut Parser<'i>) -> PResult<'i, CalcVal> {
    let mut acc = calc_product(p)?;
    while let Ok((negative, rhs)) = p.try_parse(|p| {
        let negative = match p.next()? {
            Token::Delim('+') => false,
            Token::Delim('-') => true,
            _ => return err(p),
        };
        Ok((negative, calc_product(p)?))
    }) {
        let sign = if negative { -1.0 } else { 1.0 };
        acc = match (acc, rhs) {
            (CalcVal::Num(a), CalcVal::Num(b)) => CalcVal::Num(a + sign * b),
            (CalcVal::Len(a), CalcVal::Len(b)) => CalcVal::Len(a.plus(b.scale(sign))),
            // `0` mixed with lengths is fine in practice
            (CalcVal::Len(a), CalcVal::Num(0.0)) | (CalcVal::Num(0.0), CalcVal::Len(a)) => CalcVal::Len(a),
            (a, b) => match (a.terms(), b.scaled(sign).terms()) {
                (Some(mut a), Some(b)) => {
                    a.extend(b);
                    CalcVal::Terms(a)
                }
                _ => return err(p),
            },
        };
    }
    Ok(acc)
}

fn calc_product<'i>(p: &mut Parser<'i>) -> PResult<'i, CalcVal> {
    let mut acc = calc_value(p)?;
    while let Ok((divide, rhs)) = p.try_parse(|p| {
        let divide = match p.next()? {
            Token::Delim('*') => false,
            Token::Delim('/') => true,
            _ => return err(p),
        };
        Ok((divide, calc_value(p)?))
    }) {
        acc = match (acc, rhs, divide) {
            (CalcVal::Num(a), CalcVal::Num(b), false) => CalcVal::Num(a * b),
            (CalcVal::Num(a), CalcVal::Num(b), true) if b != 0.0 => CalcVal::Num(a / b),
            (v, CalcVal::Num(b), false) | (CalcVal::Num(b), v, false) => v.scaled(b),
            (v, CalcVal::Num(b), true) if b != 0.0 => v.scaled(1.0 / b),
            _ => return err(p),
        };
    }
    Ok(acc)
}

fn calc_value<'i>(p: &mut Parser<'i>) -> PResult<'i, CalcVal> {
    match p.next()?.clone() {
        Token::Number { value, .. } => Ok(CalcVal::Num(value)),
        Token::Dimension { value, ref unit, .. } => match unit_lin(value, unit) {
            Some(l) => Ok(CalcVal::Len(l)),
            None => err(p),
        },
        Token::Percentage { unit_value, .. } => Ok(CalcVal::Len(Lin::pct(unit_value * 100.0))),
        Token::ParenthesisBlock => p.parse_nested_block(calc_sum),
        Token::Function(ref f) if f.eq_ignore_ascii_case("calc") => p.parse_nested_block(calc_sum),
        // `calc(clamp(...) - 32px)`: a term of its own.
        Token::Function(ref f) if ["min", "max", "clamp"].iter().any(|n| f.eq_ignore_ascii_case(n)) => {
            let name = f.to_ascii_lowercase();
            let spec = p.parse_nested_block(|p| match name.as_str() {
                "clamp" => {
                    let lo = calc_or_len(p)?;
                    p.expect_comma()?;
                    let pref = calc_or_len(p)?;
                    p.expect_comma()?;
                    let hi = calc_or_len(p)?;
                    Ok(LenSpec::Clamp(Box::new([lo, pref, hi])))
                }
                _ => {
                    let items = p.parse_comma_separated(calc_or_len)?;
                    Ok(if name == "min" { LenSpec::Min(items) } else { LenSpec::Max(items) })
                }
            })?;
            Ok(CalcVal::Terms(vec![(1.0, spec)]))
        }
        _ => err(p),
    }
}

// ---------------------------------------------------------------------------
// Colors

/// A CSS color from a string (`#fff`, `rgb(0 0 0 / 50%)`, `red`...), as
/// 0xRRGGBBAA; `None` if it isn't one (or depends on context).
pub fn parse_color_str(text: &str) -> Option<u32> {
    let mut p = Parser::new(text.trim());
    match parse_color(&mut p).ok()? {
        Color::Rgba(v) => Some(v),
        Color::LightDark(light, _) => Some(light),
        Color::Foreground | Color::CurrentColor => Some(0x000000ff),
        _ => None,
    }
}

pub(crate) fn parse_color<'i>(p: &mut Parser<'i>) -> PResult<'i, Color> {
    if let Ok(c) = p.try_parse(|p| {
        let name = p.expect_ident()?.to_ascii_lowercase();
        match system_color(&name) {
            Some(c) => Ok(c),
            None => err(p),
        }
    }) {
        return Ok(c);
    }
    if let Ok(c) = p.try_parse(|p| {
        p.expect_function_matching("light-dark")?;
        p.parse_nested_block(|p| {
            let light = parse_color(p)?;
            p.expect_comma()?;
            let dark = parse_color(p)?;
            Ok(match (light, dark) {
                (Color::Rgba(l), Color::Rgba(d)) => Color::LightDark(l, d),
                // Keywords on either side: no way to pick later; light it is.
                _ => light,
            })
        })
    }) {
        return Ok(c);
    }
    let c = cssparser_color::Color::parse(p)?;
    Ok(convert_color(c))
}

fn system_color(name: &str) -> Option<Color> {
    Some(match name {
        "canvastext" | "buttontext" | "fieldtext" | "marktext" | "selecteditemtext" | "highlighttext" => {
            Color::Foreground
        }
        "linktext" | "visitedtext" | "activetext" | "accentcolor" => Color::Link,
        "graytext" => Color::Muted,
        "canvas" | "field" => Color::Rgba(0xffffffff),
        "buttonface" | "buttonborder" => Color::Rgba(0xefefefff),
        "highlight" | "selecteditem" => Color::Rgba(0xb4d5feff),
        "mark" => Color::Rgba(0xffff00ff),
        _ => return None,
    })
}

fn pack(r: f32, g: f32, b: f32, a: f32) -> Color {
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    Color::Rgba((ch(r) << 24) | (ch(g) << 16) | (ch(b) << 8) | ch(a))
}

fn gamma(c: f32) -> f32 {
    if c <= 0.0031308 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 360.0;
    if s == 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

fn oklab_to_srgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_.powi(3), m_.powi(3), s_.powi(3));
    (
        gamma(4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3),
        gamma(-1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3),
        gamma(-0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3),
    )
}

fn lab_to_srgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    // CIE Lab (D50) → XYZ → linear sRGB (Bradford-adapted matrix).
    const E: f32 = 216.0 / 24389.0;
    const K: f32 = 24389.0 / 27.0;
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let inv = |f: f32| if f.powi(3) > E { f.powi(3) } else { (116.0 * f - 16.0) / K };
    let x = 0.964_22 * inv(fx);
    let y = if l > K * E { fy.powi(3) } else { l / K };
    let z = 0.825_21 * inv(fz);
    (
        gamma(3.133_856 * x - 1.616_866_7 * y - 0.490_614_6 * z),
        gamma(-0.978_768_4 * x + 1.916_141_5 * y + 0.033_454 * z),
        gamma(0.071_945_3 * x - 0.228_991_4 * y + 1.405_242_7 * z),
    )
}

fn convert_color(c: cssparser_color::Color) -> Color {
    use cssparser_color::Color as C;
    let o = |v: Option<f32>| v.unwrap_or(0.0);
    let alpha = |v: Option<f32>| v.unwrap_or(1.0);
    match c {
        C::CurrentColor => Color::CurrentColor,
        C::Rgba(rgba) => Color::Rgba(
            ((rgba.red as u32) << 24)
                | ((rgba.green as u32) << 16)
                | ((rgba.blue as u32) << 8)
                | (rgba.alpha.clamp(0.0, 1.0) * 255.0).round() as u32,
        ),
        C::Hsl(h) => {
            let (r, g, b) = hsl_to_rgb(o(h.hue), o(h.saturation), o(h.lightness));
            pack(r, g, b, alpha(h.alpha))
        }
        C::Hwb(h) => {
            let (w, bl) = (o(h.whiteness), o(h.blackness));
            let (r, g, b) = if w + bl >= 1.0 {
                let gray = w / (w + bl);
                (gray, gray, gray)
            } else {
                let (r, g, b) = hsl_to_rgb(o(h.hue), 1.0, 0.5);
                let f = |c: f32| c * (1.0 - w - bl) + w;
                (f(r), f(g), f(b))
            };
            pack(r, g, b, alpha(h.alpha))
        }
        C::Lab(c) => {
            let (r, g, b) = lab_to_srgb(o(c.lightness), o(c.a), o(c.b));
            pack(r, g, b, alpha(c.alpha))
        }
        C::Lch(c) => {
            let h = o(c.hue).to_radians();
            let (r, g, b) = lab_to_srgb(o(c.lightness), o(c.chroma) * h.cos(), o(c.chroma) * h.sin());
            pack(r, g, b, alpha(c.alpha))
        }
        C::Oklab(c) => {
            let (r, g, b) = oklab_to_srgb(o(c.lightness), o(c.a), o(c.b));
            pack(r, g, b, alpha(c.alpha))
        }
        C::Oklch(c) => {
            let h = o(c.hue).to_radians();
            let (r, g, b) = oklab_to_srgb(o(c.lightness), o(c.chroma) * h.cos(), o(c.chroma) * h.sin());
            pack(r, g, b, alpha(c.alpha))
        }
        C::ColorFunction(f) => {
            // `color(srgb ...)` and friends; wide-gamut spaces are treated as sRGB.
            let (c1, c2, c3) = (o(f.c1), o(f.c2), o(f.c3));
            let (r, g, b) = if format!("{:?}", f.color_space) == "SrgbLinear" {
                (gamma(c1), gamma(c2), gamma(c3))
            } else {
                (c1, c2, c3)
            };
            pack(r, g, b, alpha(f.alpha))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(name: &str, value: &str) -> Vec<Decl> {
        let mut out = Vec::new();
        parse_declaration(name, value, &mut out);
        out
    }

    #[test]
    fn colors() {
        assert_eq!(parse("color", "#ff6600"), vec![Decl::Color(Color::Rgba(0xff6600ff))]);
        assert_eq!(parse("color", "rgb(0 128 255 / 50%)"), vec![Decl::Color(Color::Rgba(0x0080ff80))]);
        assert_eq!(parse("color", "hsl(0 100% 50%)"), vec![Decl::Color(Color::Rgba(0xff0000ff))]);
        assert_eq!(parse("color", "LinkText"), vec![Decl::Color(Color::Link)]);
        assert_eq!(parse("color", "light-dark(#111, #eee)"), vec![Decl::Color(Color::LightDark(0x111111ff, 0xeeeeeeff))]);
        // oklch(62.8% 0.2577 29.23) ≈ pure red
        let Decl::Color(Color::Rgba(v)) = parse("color", "oklch(62.8% 0.2577 29.23)")[0] else { panic!() };
        assert!((v >> 24) > 0xf0 && ((v >> 16) & 0xff) < 0x10, "{v:08x}");
        assert!(parse("color", "nonsense").is_empty());
        // The full CSS named-color table (via cssparser-color).
        assert_eq!(parse("color", "rebeccapurple"), vec![Decl::Color(Color::Rgba(0x663399ff))]);
        assert_eq!(parse("color", "LightGoldenrodYellow"), vec![Decl::Color(Color::Rgba(0xfafad2ff))]);
        assert_eq!(parse("background-color", "transparent"), vec![Decl::BackgroundColor(Color::TRANSPARENT)]);
    }

    #[test]
    fn lengths_and_calc() {
        assert_eq!(parse("width", "10px"), vec![Decl::Width(LenSpec::px(10.0))]);
        assert_eq!(parse("width", "50%"), vec![Decl::Width(LenSpec::Lin(Lin::pct(50.0)))]);
        assert_eq!(
            parse("width", "calc(2rem + 4px * 2)"),
            vec![Decl::Width(LenSpec::Lin(Lin { rem: 2.0, px: 8.0, ..Default::default() }))]
        );
        assert!(matches!(parse("max-width", "min(100%, 60rem)")[0], Decl::MaxWidth(LenSpec::Min(_))));
        assert!(parse("width", "10 px").is_empty());
    }

    #[test]
    fn shorthands() {
        let m = parse("margin", "0 auto");
        assert_eq!(m.len(), 4);
        assert_eq!(m[1], Decl::Margin(Side::Right, LenSpec::Auto));
        assert_eq!(m[2], Decl::Margin(Side::Bottom, LenSpec::px(0.0)));

        let b = parse("border-bottom", "1px solid #ccc");
        assert_eq!(b, vec![
            Decl::BorderWidth(Side::Bottom, LenSpec::px(1.0)),
            Decl::BorderStyle(Side::Bottom, BorderStyle::Solid),
            Decl::BorderColor(Side::Bottom, Color::Rgba(0xccccccff)),
        ]);

        let f = parse("font", "italic bold 12px/1.5 Georgia, serif");
        assert!(f.contains(&Decl::Italic(true)));
        assert!(f.contains(&Decl::FontWeight(WeightSpec::Abs(700))));
        assert!(f.contains(&Decl::LineHeight(LineHeightSpec::Number(1.5))));
        assert!(f.contains(&Decl::FontFamily(FontFamily::Serif)));

        assert_eq!(parse("flex", "1"), vec![Decl::FlexGrow(1.0), Decl::FlexShrink(1.0), Decl::FlexBasis(LenSpec::Lin(Lin::pct(0.0)))]);
        let bg = with_base(Some(&url::Url::parse("https://e.com/css/site.css").unwrap()), || {
            parse("background", "url(\"../img/x.png\") no-repeat center / 10px #fafafa")
        });
        assert_eq!(bg[0], Decl::BackgroundColor(Color::Rgba(0xfafafaff)));
        assert!(bg.contains(&Decl::BackgroundUrl(Some("https://e.com/img/x.png".into()))));
        assert!(bg.contains(&Decl::BackgroundRepeat(BgRepeat { x: false, y: false })));
        assert!(bg.contains(&Decl::BackgroundSize(BgSizeSpec::Explicit(LenSpec::px(10.0), LenSpec::Auto))));
        assert!(bg.contains(&Decl::BackgroundPosition(BgPos::CENTER)));
        let mask = parse("mask-image", "url(data:image/svg+xml,%3Csvg%3E)");
        assert_eq!(mask[0], Decl::Masked(true));
        assert!(matches!(&mask[1], Decl::MaskUrl(Some(u)) if u.starts_with("data:image/svg+xml")));
    }

    #[test]
    fn grid_columns() {
        assert_eq!(
            parse("grid-template-columns", "1fr 2fr [end] 100px"),
            vec![
                Decl::GridColumns(GridCols::Count(3)),
                Decl::GridTracks(Some(vec![TrackSpec::Fr(1.0), TrackSpec::Fr(2.0), TrackSpec::Len(LenSpec::px(100.0))].into())),
            ]
        );
        // Equal columns don't need tracks.
        assert_eq!(
            parse("grid-template-columns", "repeat(4, minmax(0, 1fr))"),
            vec![Decl::GridColumns(GridCols::Count(4)), Decl::GridTracks(None)]
        );
        assert_eq!(
            parse("flex", "1 1 0"),
            vec![Decl::FlexGrow(1.0), Decl::FlexShrink(1.0), Decl::FlexBasis(LenSpec::px(0.0))]
        );
        assert_eq!(
            parse("grid-template-columns", "repeat(auto-fill, minmax(200px, 1fr))"),
            vec![Decl::GridColumns(GridCols::AutoFill(LenSpec::px(200.0), false)), Decl::GridTracks(None)]
        );
    }

    #[test]
    fn grid_templates() {
        let d = parse("grid-template", "min-content 1fr / 12.25rem minmax(0,1fr)");
        assert!(d.contains(&Decl::GridTracks(Some(
            vec![TrackSpec::Len(LenSpec::Lin(Lin { rem: 12.25, ..Default::default() })), TrackSpec::Fr(1.0)].into()
        ))));
        let areas = parse("grid-template-areas", "'a a' 'b c'");
        let Decl::GridAreas(Some(rows)) = &areas[0] else { panic!("{areas:?}") };
        assert_eq!(rows.len(), 2);
        assert_eq!(&*rows[1][1], "c");
        assert_eq!(parse("grid-area", "pageContent"), vec![Decl::GridArea(Some("pageContent".into()))]);
    }

    #[test]
    fn var_and_wide_keywords_are_deferred() {
        assert!(matches!(&parse("color", "var(--x, red)")[0], Decl::Unresolved { name, .. } if &**name == "color"));
        assert_eq!(parse("color", "inherit"), vec![Decl::Wide("color".into(), Wide::Inherit)]);
        assert_eq!(parse("--Brand", " #fff "), vec![Decl::Custom("--Brand".into(), "#fff".into())]);
    }

    #[test]
    fn display_and_font_family() {
        assert_eq!(parse("display", "inline-flex"), vec![Decl::Display(Display::InlineFlex)]);
        assert_eq!(
            parse("content", r#"" [" attr(title) "\f007" open-quote"#),
            vec![Decl::Content(Some(vec![
                ContentItem::Str(" [".into()),
                ContentItem::Attr("title".into()),
                ContentItem::Str("".into()),
                ContentItem::OpenQuote,
            ].into()))]
        );
        assert_eq!(parse("content", "none"), vec![Decl::Content(None)]);
        assert_eq!(parse("display", "flow-root"), vec![Decl::Display(Display::Block)]);
        let serif = parse("font-family", "'Linux Libertine', 'Georgia', Times, serif");
        assert_eq!(serif[0], Decl::FontFamily(FontFamily::Serif));
        assert_eq!(
            serif[1],
            Decl::FontNames(vec!["Linux Libertine".into(), "Georgia".into(), "Times".into(), "serif".into()].into())
        );
        assert_eq!(parse("font-family", "ui-monospace, SFMono-Regular, Menlo")[0], Decl::FontFamily(FontFamily::Mono));
    }
}
