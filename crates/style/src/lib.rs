//! CSS: stylesheets, selector matching, the cascade, and computed styles.
//!
//! - [`sheet`] parses stylesheets (with `cssparser`), evaluating `@media`.
//! - [`selector`] plugs our page model into Servo's `selectors` matcher.
//! - [`props`] parses property values into typed declarations.
//! - [`cascade`] picks the winning declarations per element and computes
//!   [`ComputedStyle`], which is all the renderer depends on.

pub mod cascade;
pub mod media;
pub mod props;
pub mod selector;
pub mod sheet;
pub mod values;

use std::collections::HashMap;
use std::sync::Arc;

pub use cascade::{Pseudo, StyleData, StyleSource, Styler, collect_style_sources};
pub use media::MediaEnv;
pub use values::*;

/// Custom properties (`--foo`), inherited by sharing.
pub type CustomProps = Arc<HashMap<Arc<str>, Arc<str>>>;

#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    // ---- not inherited ----
    pub display: Display,
    pub position: Position,
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
    pub align_self: Option<Align>,
    pub row_gap: Length,
    pub column_gap: Length,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Length,
    pub grid_columns: Option<u16>,
    pub grid_column: Option<GridSpan>,
    pub overflow: Overflow,
    pub opacity: f32,
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
    pub list_style: ListStyle,
    pub visible: bool,
    pub custom: CustomProps,
}

impl ComputedStyle {
    /// Initial values of every property (the root's parent).
    pub fn initial() -> Self {
        Self {
            display: Display::Inline,
            position: Position::Static,
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
            align_self: None,
            row_gap: Length::ZERO,
            column_gap: Length::ZERO,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Length::Auto,
            grid_columns: None,
            grid_column: None,
            overflow: Overflow::Visible,
            opacity: 1.0,
            masked: false,
            mask_url: None,
            mask_size: BgSize::Auto,
            mask_position: BgPos::TOP_LEFT,
            background_url: None,
            background_size: BgSize::Auto,
            background_position: BgPos::TOP_LEFT,
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
            list_style: ListStyle::Disc,
            visible: true,
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
            list_style: parent.list_style,
            visible: parent.visible,
            custom: parent.custom.clone(),
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
