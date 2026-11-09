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
}

impl LenSpec {
    pub fn px(v: f32) -> Self {
        LenSpec::Lin(Lin::px(v))
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
            Display::None | Display::Inline | Display::InlineBlock | Display::Contents
        )
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
}

impl BgPos {
    pub const TOP_LEFT: BgPos = BgPos { x: 0.0, y: 0.0 };
    pub const CENTER: BgPos = BgPos { x: 0.5, y: 0.5 };
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
