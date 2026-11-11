//! CSS: stylesheets, selector matching, the cascade, and computed styles.
//!
//! - [`sheet`] parses stylesheets (with `cssparser`), evaluating `@media`.
//! - [`selector`] plugs our page model into Servo's `selectors` matcher.
//! - [`props`] parses property values into typed declarations.
//! - [`cascade`] picks the winning declarations per element and computes
//!   [`ComputedStyle`], which is all the renderer depends on.

pub mod cache;
pub mod cascade;
pub mod media;
pub mod props;
pub mod query;
pub mod selector;
pub mod sheet;
pub mod supports;
pub mod values;

use std::collections::HashMap;
use std::sync::Arc;

pub use sheet::FontFace;
pub use cascade::{DARK_CANVAS, Pseudo, StyleData, StyleSource, Styler, collect_style_sources};
pub use selector::ElementState;
pub use cache::{StyleCache, StyleSession};
pub use media::MediaEnv;
pub use values::*;

/// Custom properties (`--foo`), inherited by sharing.
pub type CustomProps = Arc<HashMap<Arc<str>, Arc<str>>>;

/// An SVG paint property as written (`("stroke", "currentColor")`).
pub type SvgPaint = (Arc<str>, Arc<str>);

#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    // ---- not inherited ----
    pub display: Display,
    pub position: Position,
    /// `top`/`right`/`bottom`/`left`.
    pub inset: Edges<Length>,
    pub float: Float,
    pub clear: bool,
    pub vertical_align: VerticalAlign,
    /// `box-sizing: border-box` (else content-box, CSS's default).
    pub border_box: bool,
    pub margin: Edges<Length>,
    pub padding: Edges<Length>,
    pub border_width: Edges<f32>,
    pub border_style: Edges<BorderStyle>,
    pub border_color: Edges<Color>,
    /// top-left, top-right, bottom-right, bottom-left
    pub border_radius: [Length; 4],
    pub background: Option<Color>,
    /// First color of a background gradient; painted instead of `background`.
    pub background_image: Option<Color>,
    pub width: Length,
    pub height: Length,
    pub min_width: Length,
    pub min_height: Length,
    /// `Auto` means none.
    pub max_width: Length,
    pub max_height: Length,
    pub flex_direction: FlexDirection,
    pub flex_wrap: bool,
    pub justify_content: Option<Justify>,
    pub align_items: Option<Align>,
    pub align_content: Option<Justify>,
    pub align_self: Option<Align>,
    pub row_gap: Length,
    pub column_gap: Length,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Length,
    pub grid_columns: Option<u16>,
    /// `repeat(auto-fill | auto-fit, minmax(<px>, 1fr))`: the minimum
    /// track size and whether it's `auto-fit`. Layout picks the count.
    pub grid_auto: Option<(f32, bool)>,
    pub grid_column: Option<GridSpan>,
    /// Explicit column tracks, when they aren't all equal (`12rem 1fr`).
    pub grid_tracks: Option<Arc<[Track]>>,
    /// Equal columns are at least as wide as their content (see
    /// `Decl::GridContentMin`).
    pub grid_content_min: bool,
    /// The grid in full (see `GridTemplate`), for the grid layout.
    pub grid_template_columns: Option<Arc<GridTemplate>>,
    pub grid_template_rows: Option<Arc<GridTemplate>>,
    pub grid_auto_columns: Arc<[TrackSize]>,
    pub grid_auto_rows: Arc<[TrackSize]>,
    /// `grid-auto-flow`: column-major? dense?
    pub grid_auto_flow: (bool, bool),
    /// Placement as a grid item: (start, end) lines.
    pub grid_row_lines: (GridLine, GridLine),
    pub grid_column_lines: (GridLine, GridLine),
    pub justify_items: Option<Align>,
    pub justify_self: Option<Align>,
    /// (Set by the renderer.) Laid out as a real CSS grid, whole: its
    /// placements are more than the row-based layout can express.
    pub taffy_grid: bool,
    pub grid_areas: Option<GridAreas>,
    /// `grid-area: <name>` on a grid item.
    pub grid_area: Option<Arc<str>>,
    /// `width: fit-content` (or min/max-content): shrink to the content
    /// instead of filling the line.
    pub fit_width: bool,
    /// Explicit grid lines (`grid-row-start: 1`): items can share a cell.
    pub grid_row_start: Option<i16>,
    pub grid_column_start: Option<i16>,
    pub overflow: Overflow,
    pub opacity: f32,
    pub transitions: Option<Arc<Transitions>>,
    pub animations: Option<Arc<[Animation]>>,
    /// `box-shadow` layers (front first).
    pub shadows: Option<Arc<[Shadow]>>,
    /// `filter` functions, in order.
    pub filter: Option<Arc<[Filter]>>,
    /// `transform: translate(x, y)`: drawn this far from where it's laid out.
    pub translate: (Offset, Offset),
    /// `transform: rotate()` / `rotate`: degrees, clockwise, about the center.
    pub rotate: f32,
    /// `aspect-ratio` (width / height).
    pub aspect_ratio: Option<f32>,
    /// `background-clip: text` (gradient text).
    pub clip_text: bool,
    /// `background-clip`, per gradient layer (top first): `true` for
    /// `border-box`. `None` (unset): layers fill the padding box.
    pub bg_clip_border: Option<Arc<[bool]>>,
    /// Gradient text: the colors the glyphs take across the box (inherited,
    /// like the transparent fill that lets it show).
    pub text_gradient: Option<Arc<TextGradient>>,
    /// `pointer-events: none` (inherited): the mouse goes through it.
    pub pointer_events_none: bool,
    /// `z-index` (`None`: auto). Only its sign matters so far: negative
    /// positioned boxes paint under their in-flow siblings.
    pub z_index: Option<i32>,
    /// How a replaced element's picture fits its box.
    pub object_fit: ObjectFit,
    /// `text-overflow: ellipsis`: an overflowing unwrapped line ends in "…".
    pub text_ellipsis: bool,
    /// `-webkit-line-clamp`: at most this many lines, the last ending in "…".
    pub line_clamp: Option<u32>,
    /// `-webkit-text-fill-color` (inherited).
    pub text_fill: Option<Color>,
    /// SVG paint properties from CSS (`fill`, `stroke`...): inherited, and
    /// written into inline SVGs.
    pub svg_paint: Option<Arc<[SvgPaint]>>,
    /// `transform: scale()` / `scale`: (x, y), about the center.
    pub scale: (f32, f32),
    /// `transform-origin` (x, y): what scaling is about (default the
    /// center, 50% 50%).
    pub transform_origin: (Length, Length),
    /// `transform` is something other than `none` (even an identity
    /// matrix): the box is the containing block of absolute descendants.
    pub transformed: bool,
    /// `mask-image` is set: the background is a stencil (usually an icon).
    pub masked: bool,
    /// The mask image's (absolute) URL, if it's one we can fetch.
    pub mask_url: Option<Arc<str>>,
    pub mask_size: BgSize,
    pub mask_position: BgPos,
    /// `background-image: url(...)` (absolute URL).
    pub background_url: Option<Arc<str>>,
    pub background_size: BgSize,
    pub background_position: BgPos,
    pub background_repeat: BgRepeat,
    /// `background-image` gradients, top layer first.
    pub gradients: Option<Arc<[Gradient]>>,
    /// Generated content (`::before`/`::after` only); `None` = none/normal.
    pub content: Option<Arc<[ContentItem]>>,

    // ---- inherited (or propagated) ----
    /// Text decorations aren't inherited in CSS, but they propagate to descendants' text.
    pub underline: bool,
    pub strikethrough: bool,
    /// px
    pub font_size: f32,
    pub line_height: LineHeight,
    pub font_weight: u16,
    pub italic: bool,
    pub font_family: FontFamily,
    pub font_names: FontNames,
    pub color: Color,
    pub white_space: WhiteSpace,
    pub text_align: TextAlign,
    pub text_transform: TextTransform,
    /// `font-variant: small-caps` (`Some(false)`) or `all-small-caps`
    /// (`Some(true)`): lowercase (or all) letters as small capitals.
    pub small_caps: Option<bool>,
    pub list_style: ListStyle,
    pub visible: bool,
    pub cursor: Cursor,
    /// The used color scheme is dark: the page allows it (`color-scheme`)
    /// and the user prefers it.
    pub dark: bool,
    /// `container-type`: 0 none, 1 inline-size, 2 size.
    pub container_type: u8,
    /// The size container query units refer to (inherited; `None`: the
    /// viewport).
    pub query_container: Option<(f32, f32)>,
    /// A guess at this element's content size from the cascade alone (px
    /// widths, percentages of the parent's guess), for container units.
    pub est_width: Option<f32>,
    pub est_height: Option<f32>,
    /// `width`, `min-width`, `max-width` given as a percentage and a fixed
    /// part together (`calc(100% - 32px)`), as (px, percent): the layout
    /// can't take a sum, so the box builder works it out against its guess
    /// at the containing block's width (`width` and friends hold the
    /// percentage alone meanwhile).
    pub mixed_widths: Option<Arc<MixedWidths>>,
    pub custom: CustomProps,
}

/// See [`ComputedStyle::mixed_widths`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MixedWidths {
    pub width: Option<(f32, f32)>,
    pub min_width: Option<(f32, f32)>,
    pub max_width: Option<(f32, f32)>,
}

impl ComputedStyle {
    /// Initial values of every property (the root's parent).
    pub fn initial() -> Self {
        Self {
            display: Display::Inline,
            position: Position::Static,
            inset: Edges::all(Length::Auto),
            float: Float::None,
            clear: false,
            vertical_align: VerticalAlign::Baseline,
            border_box: false,
            margin: Edges::all(Length::ZERO),
            padding: Edges::all(Length::ZERO),
            border_width: Edges::all(3.0),
            border_style: Edges::all(BorderStyle::None),
            border_color: Edges::all(Color::CurrentColor),
            border_radius: [Length::ZERO; 4],
            background: None,
            background_image: None,
            width: Length::Auto,
            height: Length::Auto,
            min_width: Length::Auto,
            min_height: Length::Auto,
            max_width: Length::Auto,
            max_height: Length::Auto,
            flex_direction: FlexDirection::Row,
            flex_wrap: false,
            justify_content: None,
            align_items: None,
            align_content: None,
            align_self: None,
            row_gap: Length::ZERO,
            column_gap: Length::ZERO,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Length::Auto,
            grid_columns: None,
            grid_auto: None,
            grid_column: None,
            grid_tracks: None,
            grid_content_min: false,
            grid_template_columns: None,
            grid_template_rows: None,
            grid_auto_columns: Arc::from([]),
            grid_auto_rows: Arc::from([]),
            grid_auto_flow: (false, false),
            grid_row_lines: (GridLine::Auto, GridLine::Auto),
            grid_column_lines: (GridLine::Auto, GridLine::Auto),
            justify_items: None,
            justify_self: None,
            taffy_grid: false,
            grid_areas: None,
            grid_area: None,
            fit_width: false,
            grid_row_start: None,
            grid_column_start: None,
            overflow: Overflow::Visible,
            opacity: 1.0,
            translate: (Offset { px: 0.0, pct: 0.0 }, Offset { px: 0.0, pct: 0.0 }),
            rotate: 0.0,
            svg_paint: None,
            clip_text: false,
            bg_clip_border: None,
            text_gradient: None,
            pointer_events_none: false,
            z_index: None,
            object_fit: ObjectFit::Fill,
            text_ellipsis: false,
            line_clamp: None,
            aspect_ratio: None,
            text_fill: None,
            scale: (1.0, 1.0),
            transform_origin: (Length::Percent(50.0), Length::Percent(50.0)),
            transformed: false,
            shadows: None,
            filter: None,
            transitions: None,
            animations: None,
            masked: false,
            mask_url: None,
            mask_size: BgSize::Auto,
            mask_position: BgPos::TOP_LEFT,
            background_url: None,
            background_size: BgSize::Auto,
            background_position: BgPos::TOP_LEFT,
            background_repeat: BgRepeat { x: true, y: true },
            gradients: None,
            content: None,
            underline: false,
            strikethrough: false,
            font_size: 16.0,
            line_height: LineHeight::Normal,
            font_weight: 400,
            italic: false,
            font_family: FontFamily::Sans,
            font_names: FontNames::from([]),
            color: Color::Foreground,
            white_space: WhiteSpace::Normal,
            text_align: TextAlign::Left,
            text_transform: TextTransform::None,
            small_caps: None,
            list_style: ListStyle::Disc,
            visible: true,
            cursor: Cursor::Auto,
            dark: false,
            container_type: 0,
            query_container: None,
            est_width: None,
            est_height: None,
            mixed_widths: None,
            custom: CustomProps::default(),
        }
    }

    /// A child's starting point: inherited properties copied, the rest initial.
    pub fn inherit(parent: &Self) -> Self {
        Self {
            underline: parent.underline,
            strikethrough: parent.strikethrough,
            font_size: parent.font_size,
            line_height: parent.line_height,
            font_weight: parent.font_weight,
            italic: parent.italic,
            font_family: parent.font_family,
            font_names: parent.font_names.clone(),
            color: parent.color,
            white_space: parent.white_space,
            text_align: parent.text_align,
            text_transform: parent.text_transform,
            small_caps: parent.small_caps,
            list_style: parent.list_style,
            visible: parent.visible,
            cursor: parent.cursor,
            dark: parent.dark,
            query_container: parent.query_container,
            custom: parent.custom.clone(),
            svg_paint: parent.svg_paint.clone(),
            text_fill: parent.text_fill,
            text_gradient: parent.text_gradient.clone(),
            pointer_events_none: parent.pointer_events_none,
            ..Self::initial()
        }
    }

    /// An anonymous box generated by layout (not by an element): inherits, and
    /// has no visual of its own.
    pub fn anonymous(parent: &Self) -> Self {
        Self {
            border_width: Edges::all(0.0),
            ..Self::inherit(parent)
        }
    }

    /// The style used for the document root's parent (block, so the root is too).
    pub fn root_parent() -> Self {
        Self {
            display: Display::Block,
            // `medium` borders only count once a border-style is set; nothing set here.
            border_width: Edges::all(0.0),
            ..Self::initial()
        }
    }

    pub fn is_bold(&self) -> bool {
        self.font_weight >= 600
    }

    /// The painted background: a gradient's first stop wins over the color.
    pub fn painted_background(&self) -> Option<Color> {
        if self.masked {
            // Painting the unmasked background would draw a solid box instead of the icon.
            return None;
        }
        self.background_image
            .or(self.background)
            .filter(|c| !c.is_transparent())
    }

    /// The background color a box paints under its gradients (with no
    /// gradients, the gradient stand-in color, if any, as before).
    pub fn box_background(&self) -> Option<Color> {
        if self.gradients.is_some() && !self.masked {
            return self.background.filter(|c| !c.is_transparent());
        }
        self.painted_background()
    }

    /// The color a mask stencil is filled with (the unmasked background).
    pub fn mask_color(&self) -> Option<Color> {
        self.background_image.or(self.background).filter(|c| !c.is_transparent())
    }

    /// Horizontal padding + border, px (percent padding counts as 0).
    pub fn horizontal_edges(&self) -> f32 {
        self.padding.left.px_or_zero() + self.padding.right.px_or_zero() + self.border_width.left + self.border_width.right
    }

    pub fn vertical_edges(&self) -> f32 {
        self.padding.top.px_or_zero() + self.padding.bottom.px_or_zero() + self.border_width.top + self.border_width.bottom
    }

    /// A specified `width`-type length as a border-box length (what GPUI's
    /// layout uses): content-box sizes grow by padding and border.
    pub fn outer_width(&self, l: Length) -> Length {
        match l {
            Length::Px(v) if !self.border_box => Length::Px(v + self.horizontal_edges()),
            other => other,
        }
    }

    pub fn outer_height(&self, l: Length) -> Length {
        match l {
            Length::Px(v) if !self.border_box => Length::Px(v + self.vertical_edges()),
            other => other,
        }
    }

    pub fn has_border(&self) -> bool {
        Side::ALL.iter().any(|&s| self.border_width.get(s) > 0.0)
    }
}
