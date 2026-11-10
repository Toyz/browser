//! Value types: specified (as parsed, possibly relative) and computed.

use std::sync::Arc;
/// A computed length, as the renderer sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    Px(f32),
    /// Percent of the containing block (0–100).
    Percent(f32),
    /// `auto` (or `none` for max-width/max-height).
    Auto,
}

impl Length {
    pub const ZERO: Length = Length::Px(0.0);

    /// Pixel value, treating percentages and auto as zero (for margin collapsing etc).
    pub fn px_or_zero(self) -> f32 {
        match self {
            Length::Px(v) => v,
            _ => 0.0,
        }
    }

    pub fn is_auto(self) -> bool {
        matches!(self, Length::Auto)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edges<T> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

impl<T: Copy> Edges<T> {
    pub const fn all(v: T) -> Self {
        Self { top: v, right: v, bottom: v, left: v }
    }

    pub fn get(&self, side: Side) -> T {
        match side {
            Side::Top => self.top,
            Side::Right => self.right,
            Side::Bottom => self.bottom,
            Side::Left => self.left,
        }
    }

    pub fn set(&mut self, side: Side, v: T) {
        match side {
            Side::Top => self.top = v,
            Side::Right => self.right = v,
            Side::Bottom => self.bottom = v,
            Side::Left => self.left = v,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::Top, Side::Right, Side::Bottom, Side::Left];
}

/// A linear combination of length units, the result of parsing a length or a `calc()`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Lin {
    pub px: f32,
    pub em: f32,
    pub rem: f32,
    pub vw: f32,
    pub vh: f32,
    pub vmin: f32,
    pub vmax: f32,
    /// Container query units: % of the query container's width / height.
    pub cqw: f32,
    pub cqh: f32,
    pub pct: f32,
}

impl std::ops::Add for Lin {
    type Output = Lin;
    fn add(self, o: Lin) -> Lin {
        Lin {
            px: self.px + o.px,
            em: self.em + o.em,
            rem: self.rem + o.rem,
            vw: self.vw + o.vw,
            vh: self.vh + o.vh,
            vmin: self.vmin + o.vmin,
            vmax: self.vmax + o.vmax,
            cqw: self.cqw + o.cqw,
            cqh: self.cqh + o.cqh,
            pct: self.pct + o.pct,
        }
    }
}

impl Lin {
    pub fn px(v: f32) -> Self {
        Lin { px: v, ..Default::default() }
    }

    pub fn pct(v: f32) -> Self {
        Lin { pct: v, ..Default::default() }
    }

    pub fn plus(self, o: Lin) -> Lin {
        Lin {
            px: self.px + o.px,
            em: self.em + o.em,
            rem: self.rem + o.rem,
            vw: self.vw + o.vw,
            vh: self.vh + o.vh,
            vmin: self.vmin + o.vmin,
            vmax: self.vmax + o.vmax,
            cqw: self.cqw + o.cqw,
            cqh: self.cqh + o.cqh,
            pct: self.pct + o.pct,
        }
    }

    pub fn scale(self, k: f32) -> Lin {
        Lin {
            px: self.px * k,
            em: self.em * k,
            rem: self.rem * k,
            vw: self.vw * k,
            vh: self.vh * k,
            vmin: self.vmin * k,
            vmax: self.vmax * k,
            cqw: self.cqw * k,
            cqh: self.cqh * k,
            pct: self.pct * k,
        }
    }

    pub fn is_zero(&self) -> bool {
        *self == Lin::default()
    }

    /// Absolute part in px, given the context.
    pub fn abs_px(&self, cx: &ResolveCx) -> f32 {
        self.px
            + self.em * cx.font_size
            + self.rem * cx.root_font_size
            + self.vw * cx.viewport_width / 100.0
            + self.vh * cx.viewport_height / 100.0
            + self.vmin * cx.viewport_width.min(cx.viewport_height) / 100.0
            + self.vmax * cx.viewport_width.max(cx.viewport_height) / 100.0
            + self.cqw * cx.container_width / 100.0
            + self.cqh * cx.container_height / 100.0
    }

    pub fn resolve(&self, cx: &ResolveCx) -> Length {
        // Mixed `calc(100% - 2rem)` can't be expressed to the layout engine;
        // keep the percentage, which is usually the dominant term.
        if self.pct != 0.0 {
            Length::Percent(self.pct)
        } else {
            Length::Px(self.abs_px(cx))
        }
    }
}

/// What a length-valued declaration specified.
#[derive(Debug, Clone, PartialEq)]
pub enum LenSpec {
    Auto,
    Lin(Lin),
    Min(Vec<LenSpec>),
    Max(Vec<LenSpec>),
    Clamp(Box<[LenSpec; 3]>),
    /// `calc()` around `min()`/`max()`/`clamp()`: a sum of scaled terms.
    Sum(Vec<(f32, LenSpec)>),
}

impl LenSpec {
    pub fn px(v: f32) -> Self {
        LenSpec::Lin(Lin::px(v))
    }

    /// A percentage and a fixed part together (`calc(100% - 32px)`), as
    /// (px, percent); `None` for anything else.
    pub fn mixed(&self, cx: &ResolveCx) -> Option<(f32, f32)> {
        let (px, pct) = match self {
            LenSpec::Lin(l) => (l.abs_px(cx), l.pct),
            LenSpec::Sum(terms) => {
                let (mut px, mut pct) = (0.0, 0.0);
                for (k, term) in terms {
                    match term {
                        LenSpec::Lin(l) => {
                            px += k * l.abs_px(cx);
                            pct += k * l.pct;
                        }
                        other => match other.resolve(cx) {
                            Length::Px(v) => px += k * v,
                            _ => return None,
                        },
                    }
                }
                (px, pct)
            }
            _ => return None,
        };
        (pct != 0.0 && px != 0.0).then_some((px, pct))
    }

    pub fn resolve(&self, cx: &ResolveCx) -> Length {
        match self {
            LenSpec::Auto => Length::Auto,
            LenSpec::Lin(l) => l.resolve(cx),
            LenSpec::Min(items) | LenSpec::Max(items) => {
                let resolved: Vec<_> = items.iter().map(|i| i.resolve(cx)).collect();
                let px: Vec<f32> = resolved
                    .iter()
                    .filter_map(|l| if let Length::Px(v) = l { Some(*v) } else { None })
                    .collect();
                let is_min = matches!(self, LenSpec::Min(_));
                if px.len() == resolved.len() {
                    let pick = if is_min { f32::min } else { f32::max };
                    Length::Px(px.into_iter().reduce(pick).unwrap_or(0.0))
                } else if is_min {
                    // `min(100%, 60rem)`: the fixed one is the interesting cap.
                    px.into_iter().reduce(f32::min).map(Length::Px).unwrap_or(resolved[0])
                } else {
                    resolved
                        .into_iter()
                        .find(|l| matches!(l, Length::Percent(_)))
                        .unwrap_or(Length::Auto)
                }
            }
            LenSpec::Sum(terms) => {
                let (mut px, mut pct) = (0.0, 0.0);
                for (k, term) in terms {
                    match term.resolve(cx) {
                        Length::Px(v) => px += k * v,
                        Length::Percent(p) => pct += k * p,
                        Length::Auto => return Length::Auto,
                    }
                }
                // Like a mixed `calc(100% - 2rem)`: the percentage wins.
                if pct != 0.0 { Length::Percent(pct) } else { Length::Px(px) }
            }
            LenSpec::Clamp(parts) => {
                let [lo, pref, hi] = &**parts;
                match pref.resolve(cx) {
                    Length::Px(v) => {
                        let mut v = v;
                        if let Length::Px(hi) = hi.resolve(cx) {
                            v = v.min(hi);
                        }
                        if let Length::Px(lo) = lo.resolve(cx) {
                            v = v.max(lo);
                        }
                        Length::Px(v)
                    }
                    other => other,
                }
            }
        }
    }
}

/// Inputs for resolving relative lengths.
#[derive(Debug, Clone, Copy)]
pub struct ResolveCx {
    /// The font size `em` refers to.
    pub font_size: f32,
    pub root_font_size: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
    /// The nearest query container's size (the viewport without one).
    pub container_width: f32,
    pub container_height: f32,
}

/// A color as the page asked for it. Theme-relative variants (from the UA
/// sheet's system colors) let the renderer choose the actual palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Foreground,
    Link,
    Muted,
    /// Only in specified values; resolved during computation.
    CurrentColor,
    /// 0xRRGGBBAA
    Rgba(u32),
    /// `light-dark(a, b)`: resolved by the element's color scheme after the cascade.
    LightDark(u32, u32),
}

impl Color {
    pub const TRANSPARENT: Color = Color::Rgba(0);

    pub fn is_transparent(self) -> bool {
        matches!(self, Color::Rgba(v) if v & 0xff == 0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    ListItem,
    Flex,
    Grid,
    /// `inline-flex` / `inline-grid`: a flex or grid container that sits
    /// in a line like an inline-block.
    InlineFlex,
    InlineGrid,
    Table,
    TableRow,
    TableCell,
    /// The element itself generates no box; its children take its place.
    Contents,
}

impl Display {
    pub fn is_block_level(self) -> bool {
        !matches!(
            self,
            Display::None | Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineGrid | Display::Contents
        )
    }

    /// What it becomes where boxes must be block-level (flex and grid
    /// items, floats, absolutely positioned boxes).
    pub fn blockified(self) -> Display {
        match self {
            Display::Inline | Display::InlineBlock => Display::Block,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            d => d,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    Solid,
    Dashed,
    Dotted,
    Double,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhiteSpace {
    /// Collapse whitespace, wrap.
    Normal,
    /// Collapse whitespace, don't wrap.
    NoWrap,
    /// Preserve everything, don't wrap.
    Pre,
    /// Preserve everything, wrap.
    PreWrap,
    /// Collapse spaces but keep newlines, wrap.
    PreLine,
}

impl WhiteSpace {
    pub fn collapses_spaces(self) -> bool {
        matches!(self, WhiteSpace::Normal | WhiteSpace::NoWrap | WhiteSpace::PreLine)
    }

    pub fn preserves_newlines(self) -> bool {
        matches!(self, WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine)
    }

    pub fn wraps(self) -> bool {
        !matches!(self, WhiteSpace::NoWrap | WhiteSpace::Pre)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFamily {
    Sans,
    Serif,
    Mono,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFit {
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStyle {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    Normal,
    /// Multiple of the font size.
    Number(f32),
    Px(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justify {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
    Baseline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
    Scroll,
}

/// A translation: pixels plus a percentage of the box's own size
/// (`translateY(calc(100% + 20px))`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Offset {
    pub px: f32,
    pub pct: f32,
}

impl Offset {
    pub fn is_zero(self) -> bool {
        self.px == 0.0 && self.pct == 0.0
    }

    /// In px, for a box `size` px long.
    pub fn resolve(self, size: f32) -> f32 {
        self.px + self.pct / 100.0 * size
    }
}

/// A timing function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Easing {
    CubicBezier(f32, f32, f32, f32),
    /// `steps(n, jump-end)` (`jump_start`: `steps(n, start)`).
    Steps(u32, bool),
}

impl Easing {
    pub const LINEAR: Easing = Easing::CubicBezier(0.0, 0.0, 1.0, 1.0);
    pub const EASE: Easing = Easing::CubicBezier(0.25, 0.1, 0.25, 1.0);

    /// Progress `t` (0..=1) through the timing function.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Steps(n, jump_start) => {
                let n = n.max(1) as f32;
                let step = if jump_start { (t * n).ceil() } else { (t * n).floor() };
                (step / n).min(1.0)
            }
            Easing::CubicBezier(x1, y1, x2, y2) => {
                if (x1, y1, x2, y2) == (0.0, 0.0, 1.0, 1.0) {
                    return t;
                }
                // Solve x(s) = t for the curve parameter s (Newton, then bisection).
                let bez = |a: f32, b: f32, s: f32| 3.0 * a * s * (1.0 - s).powi(2) + 3.0 * b * s * s * (1.0 - s) + s.powi(3);
                let dx = |s: f32| 3.0 * x1 * (1.0 - s).powi(2) + 6.0 * (x2 - x1) * s * (1.0 - s) + 3.0 * (1.0 - x2) * s * s;
                let mut s = t;
                for _ in 0..8 {
                    let err = bez(x1, x2, s) - t;
                    let d = dx(s);
                    if err.abs() < 1e-5 || d.abs() < 1e-6 {
                        break;
                    }
                    s = (s - err / d).clamp(0.0, 1.0);
                }
                if (bez(x1, x2, s) - t).abs() > 1e-3 {
                    let (mut lo, mut hi) = (0.0, 1.0);
                    for _ in 0..30 {
                        s = (lo + hi) / 2.0;
                        if bez(x1, x2, s) < t { lo = s } else { hi = s }
                    }
                }
                bez(y1, y2, s)
            }
        }
    }
}

/// `transition-*`: parallel lists, repeated to the length of `properties`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transitions {
    pub properties: Vec<Arc<str>>,
    /// Seconds.
    pub durations: Vec<f32>,
    pub delays: Vec<f32>,
    pub easings: Vec<Easing>,
}

impl Transitions {
    /// The (duration, delay, easing) for a property, if it transitions.
    pub fn for_property(&self, name: &str) -> Option<(f32, f32, Easing)> {
        let pick = |v: &Vec<f32>, i: usize| if v.is_empty() { 0.0 } else { v[i % v.len()] };
        let i = self.properties.iter().position(|p| &**p == name || &**p == "all")?;
        let duration = pick(&self.durations, i);
        let easing = if self.easings.is_empty() { Easing::EASE } else { self.easings[i % self.easings.len()] };
        (duration > 0.0).then(|| (duration, pick(&self.delays, i), easing))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnimDirection {
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

/// One `animation`.
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    pub name: Arc<str>,
    pub duration: f32,
    pub delay: f32,
    pub easing: Easing,
    /// `f32::INFINITY` for `infinite`.
    pub iterations: f32,
    pub direction: AnimDirection,
    /// `animation-fill-mode` forwards / backwards.
    pub fill_forwards: bool,
    pub fill_backwards: bool,
}

/// One `box-shadow` layer, resolved (px).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
    pub inset: bool,
}

/// One `filter` function, resolved (amounts as fractions, angles in
/// degrees, lengths in px).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Filter {
    Brightness(f32),
    Contrast(f32),
    Grayscale(f32),
    Invert(f32),
    Opacity(f32),
    Saturate(f32),
    Sepia(f32),
    HueRotate(f32),
    Blur(f32),
    DropShadow(Shadow),
}

impl Filter {
    /// `rgba` (0..1 each) through this function, if it changes colors
    /// (blur and drop-shadow don't).
    pub fn color(&self, [r, g, b, a]: [f32; 4]) -> [f32; 4] {
        let lin = |c: f32, slope: f32, icept: f32| (c * slope + icept).clamp(0.0, 1.0);
        let matrix = |m: [f32; 9]| {
            [
                (m[0] * r + m[1] * g + m[2] * b).clamp(0.0, 1.0),
                (m[3] * r + m[4] * g + m[5] * b).clamp(0.0, 1.0),
                (m[6] * r + m[7] * g + m[8] * b).clamp(0.0, 1.0),
                a,
            ]
        };
        match *self {
            Filter::Brightness(v) => [lin(r, v, 0.0), lin(g, v, 0.0), lin(b, v, 0.0), a],
            Filter::Contrast(v) => {
                let i = 0.5 - 0.5 * v;
                [lin(r, v, i), lin(g, v, i), lin(b, v, i), a]
            }
            Filter::Invert(v) => [lin(r, 1.0 - 2.0 * v, v), lin(g, 1.0 - 2.0 * v, v), lin(b, 1.0 - 2.0 * v, v), a],
            Filter::Opacity(v) => [r, g, b, a * v.clamp(0.0, 1.0)],
            Filter::Grayscale(v) => {
                let v = 1.0 - v.clamp(0.0, 1.0);
                matrix([
                    0.2126 + 0.7874 * v, 0.7152 - 0.7152 * v, 0.0722 - 0.0722 * v,
                    0.2126 - 0.2126 * v, 0.7152 + 0.2848 * v, 0.0722 - 0.0722 * v,
                    0.2126 - 0.2126 * v, 0.7152 - 0.7152 * v, 0.0722 + 0.9278 * v,
                ])
            }
            Filter::Sepia(v) => {
                let v = 1.0 - v.clamp(0.0, 1.0);
                matrix([
                    0.393 + 0.607 * v, 0.769 - 0.769 * v, 0.189 - 0.189 * v,
                    0.349 - 0.349 * v, 0.686 + 0.314 * v, 0.168 - 0.168 * v,
                    0.272 - 0.272 * v, 0.534 - 0.534 * v, 0.131 + 0.869 * v,
                ])
            }
            Filter::Saturate(v) => matrix([
                0.213 + 0.787 * v, 0.715 - 0.715 * v, 0.072 - 0.072 * v,
                0.213 - 0.213 * v, 0.715 + 0.285 * v, 0.072 - 0.072 * v,
                0.213 - 0.213 * v, 0.715 - 0.715 * v, 0.072 + 0.928 * v,
            ]),
            Filter::HueRotate(deg) => {
                let (sin, cos) = deg.to_radians().sin_cos();
                matrix([
                    0.213 + cos * 0.787 - sin * 0.213, 0.715 - cos * 0.715 - sin * 0.715, 0.072 - cos * 0.072 + sin * 0.928,
                    0.213 - cos * 0.213 + sin * 0.143, 0.715 + cos * 0.285 + sin * 0.140, 0.072 - cos * 0.072 - sin * 0.283,
                    0.213 - cos * 0.213 - sin * 0.787, 0.715 - cos * 0.715 + sin * 0.715, 0.072 + cos * 0.928 + sin * 0.072,
                ])
            }
            Filter::Blur(_) | Filter::DropShadow(_) => [r, g, b, a],
        }
    }

    /// Does this function change colors (as opposed to adding a shadow or
    /// blurring)?
    pub fn is_color(&self) -> bool {
        !matches!(self, Filter::Blur(_) | Filter::DropShadow(_))
    }
}

/// A `filter` function as specified (lengths unresolved).
#[derive(Debug, Clone, PartialEq)]
pub enum FilterSpec {
    Color(Filter),
    Blur(LenSpec),
    DropShadow(ShadowSpec),
}

/// A `box-shadow` layer as specified.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowSpec {
    pub lengths: Vec<LenSpec>,
    pub color: Color,
    pub inset: bool,
}

/// One column of an explicit `grid-template-columns`, resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Track {
    /// A fixed width, px.
    Px(f32),
    /// A share of the free space.
    Fr(f32),
    /// `minmax(<min>, <length>)`: flexible, but no wider than this, px.
    Max(f32),
    /// `auto`, `min-content`, percentages...: sized by its content.
    Auto,
}

/// A track as specified (lengths may still be em/rem-relative).
#[derive(Debug, Clone, PartialEq)]
pub enum TrackSpec {
    Len(LenSpec),
    Fr(f32),
    Max(LenSpec),
    Auto,
}

/// `grid-template-areas`: rows of area names (`.` is an empty cell).
pub type GridAreas = Arc<[Arc<[Arc<str>]>]>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridSpan {
    Span(u16),
    Full,
}

/// One piece of a `content` value (for `::before`/`::after`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentItem {
    Str(std::sync::Arc<str>),
    /// `attr(name)`: the element's attribute value.
    Attr(std::sync::Arc<str>),
    OpenQuote,
    CloseQuote,
    /// `url(...)`: a picture (resolved).
    Image(std::sync::Arc<str>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Float {
    None,
    Left,
    Right,
}

/// A `font-family` list as written (family names and generic keywords, in
/// order). Resolved against installed fonts at render time.
pub type FontNames = std::sync::Arc<[std::sync::Arc<str>]>;

/// `background-size` / `mask-size`, computed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BgSize {
    Auto,
    Contain,
    Cover,
    /// Width, height (either may be `Auto`: keep the aspect ratio).
    Explicit(Length, Length),
}

/// `background-position` / `mask-position` as fractions of the free space
/// (0 = left/top, 0.5 = center, 1 = right/bottom).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BgPos {
    pub x: f32,
    pub y: f32,
    /// Then moved this far, px (`-9px -125px` picks a sprite out of a sheet).
    pub dx: f32,
    pub dy: f32,
}

impl BgPos {
    pub const TOP_LEFT: BgPos = BgPos { x: 0.0, y: 0.0, dx: 0.0, dy: 0.0 };
    pub const CENTER: BgPos = BgPos { x: 0.5, y: 0.5, dx: 0.0, dy: 0.0 };

    /// At fractions `x`, `y` of the free space, not moved.
    pub const fn at(x: f32, y: f32) -> BgPos {
        BgPos { x, y, dx: 0.0, dy: 0.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VerticalAlign {
    Baseline,
    Sub,
    Super,
    Middle,
    Top,
    Bottom,
    TextTop,
    TextBottom,
    /// Raise (positive) or lower by this many px.
    Shift(f32),
}

/// The `cursor` property, reduced to the cursors a native UI has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    Auto,
    Pointer,
    Text,
    NotAllowed,
    Grab,
    Grabbing,
    Crosshair,
    Move,
    ResizeColumn,
    ResizeRow,
}

/// Gradient text (`background-clip: text` over a gradient): what the glyphs
/// are colored with, sampled along the line.
#[derive(Debug, Clone, PartialEq)]
pub struct TextGradient {
    /// (position 0..1, color), in order.
    pub stops: Arc<[(f32, Color)]>,
    /// The gradient's width over the box's (`background-size: 200%` is 2).
    pub scale: f32,
    /// `background-position` x, as a fraction of the free space.
    pub pos: f32,
    /// Repeats sideways (`background-repeat`).
    pub repeat: bool,
}

impl TextGradient {
    /// The color at `frac` (0..1) across the box.
    pub fn at(&self, frac: f32) -> Color {
        // The tile is `scale` boxes wide and offset by (1 - scale) * pos.
        let scale = self.scale.max(0.01);
        let mut u = (frac + (scale - 1.0) * self.pos) / scale;
        if self.repeat {
            u = u.rem_euclid(1.0);
        }
        let stops = &self.stops;
        let Some(&(first_pos, first)) = stops.first() else { return Color::TRANSPARENT };
        if u <= first_pos {
            return first;
        }
        for pair in stops.windows(2) {
            let ((a, ca), (b, cb)) = (pair[0], pair[1]);
            if u <= b {
                let f = if b > a { (u - a) / (b - a) } else { 1.0 };
                return match (ca, cb) {
                    (Color::Rgba(x), Color::Rgba(y)) => {
                        let ch = |shift: u32| {
                            let (p, q) = (((x >> shift) & 0xff) as f32, ((y >> shift) & 0xff) as f32);
                            ((p + (q - p) * f).round() as u32) << shift
                        };
                        Color::Rgba(ch(24) | ch(16) | ch(8) | ch(0))
                    }
                    _ if f < 0.5 => ca,
                    _ => cb,
                };
            }
        }
        stops[stops.len() - 1].1
    }
}

/// A CSS gradient image.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub shape: GradientShape,
    pub stops: Vec<GradientStop>,
    pub repeating: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradientShape {
    /// Degrees: 0 points up, clockwise.
    Linear(f32),
    /// `to top right` and friends: towards a corner (x: -1 left / 1 right,
    /// y: -1 top / 1 bottom), whose angle depends on the box's shape.
    LinearCorner(f32, f32),
    Radial { circle: bool, size: RadialSize, at: BgPos },
    /// `conic-gradient(from <deg> at <pos>, ...)`: stop positions are
    /// fractions of a turn (angle stops are stored as `Px(degrees)` over a
    /// 360-long line).
    Conic { from: f32, at: BgPos },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RadialSize {
    ClosestSide,
    FarthestSide,
    ClosestCorner,
    FarthestCorner,
    /// Radii, px.
    Explicit(f32, f32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    pub color: Color,
    pub pos: Option<StopPos>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StopPos {
    /// Of the gradient line (0..1).
    Fraction(f32),
    Px(f32),
}

/// `background-repeat`, per axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BgRepeat {
    pub x: bool,
    pub y: bool,
}

impl Default for BgRepeat {
    fn default() -> Self {
        BgRepeat { x: true, y: true }
    }
}


// ---------------------------------------------------------------------------
// CSS grid, as specified (for the grid layout; `GridCols`/`Track` above are
// the older summary the row-based layout uses).

/// A track's size as written (lengths still to resolve).
#[derive(Debug, Clone, PartialEq)]
pub enum TrackSizeSpec {
    Len(LenSpec),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    FitContent(LenSpec),
    MinMax(Box<TrackSizeSpec>, Box<TrackSizeSpec>),
}

/// `repeat()`'s count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatCount {
    Count(u16),
    AutoFill,
    AutoFit,
}

/// An entry of a track list as written.
#[derive(Debug, Clone, PartialEq)]
pub enum TemplateEntrySpec {
    Track(TrackSizeSpec),
    Repeat { count: RepeatCount, tracks: Vec<TrackSizeSpec>, names: Vec<Vec<Arc<str>>> },
}

/// `grid-template-columns`/`-rows` as written: entries, and the names of the
/// lines around them (`entries.len() + 1` lists).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GridTemplateSpec {
    pub entries: Vec<TemplateEntrySpec>,
    pub names: Vec<Vec<Arc<str>>>,
}

/// A track's size, resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackSize {
    Px(f32),
    /// A fraction (0.5 = 50%).
    Percent(f32),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    FitContent(f32),
    MinMax(Box<TrackSize>, Box<TrackSize>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TemplateEntry {
    Track(TrackSize),
    Repeat { count: RepeatCount, tracks: Vec<TrackSize>, names: Vec<Vec<Arc<str>>> },
}

/// `grid-template-columns`/`-rows`, resolved.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GridTemplate {
    pub entries: Vec<TemplateEntry>,
    pub names: Vec<Vec<Arc<str>>>,
}

/// One end of a grid item's placement (`grid-row-start` and friends).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GridLine {
    #[default]
    Auto,
    /// A line number (negative counts from the end).
    Line(i16),
    /// The nth line with this name. 0 is a bare name (no index given): the
    /// matching edge of the area with that name, else the first such line.
    Named(Arc<str>, i16),
    Span(u16),
    NamedSpan(Arc<str>, u16),
}

impl GridLine {
    pub fn is_auto(&self) -> bool {
        *self == GridLine::Auto
    }
}
