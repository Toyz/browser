//! Parsed declarations, and values kept as specified until the cascade resolves them.

use super::*;

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
    /// Equal `fr` columns that never get narrower than their content
    /// (`1fr` is `minmax(auto, 1fr)`; `minmax(0, 1fr)` may).
    GridContentMin(bool),
    GridAreas(Option<GridAreas>),
    GridArea(Option<Arc<str>>),
    GridRowStart(Option<i16>),
    /// `grid-template-columns` (`rows: false`) / `-rows` in full (`None`:
    /// `none`).
    GridTemplateFull { rows: bool, value: Option<Arc<GridTemplateSpec>> },
    /// `grid-auto-columns` / `-rows`.
    GridAutoTracks { rows: bool, tracks: Arc<[TrackSizeSpec]> },
    /// `grid-auto-flow`.
    GridAutoFlow { column: bool, dense: bool },
    /// One end of a placement: `grid-row-start` (`row`, not `end`) and the
    /// rest of the four.
    GridPlace { row: bool, end: bool, line: GridLine },
    JustifyItems(Option<Align>),
    JustifySelf(Option<Align>),
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
    /// `container-type` (0 normal, 1 inline-size, 2 size).
    ContainerType(u8),
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
