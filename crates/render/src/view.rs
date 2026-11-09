//! Layout tree → GPUI elements.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use style::{
    Align, BgPos, BgSize, Color, ComputedStyle, Edges, FlexDirection, FontFamily, GridSpan, Justify, Length as CssLength, LineHeight,
    Overflow,
};
use url::Url;

use crate::boxes::{Block, BlockKind, ImageBox, InlineStyle, LayoutBox, Paragraph, Row};
use crate::images::{ImageState, ImageStore};

/// The fonts installed on this system, for resolving CSS `font-family` lists.
#[derive(Default)]
pub struct FontBook {
    /// lowercase name → the system's spelling
    installed: HashMap<String, SharedString>,
    /// Resolved lists, keyed by the names themselves. (Not by `Arc` address:
    /// after a restyle frees the old lists, new ones can reuse the addresses.)
    cache: RefCell<HashMap<style::FontNames, Option<SharedString>>>,
}

impl FontBook {
    pub fn new(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            installed: names.into_iter().map(|n| (n.to_lowercase(), n.into())).collect(),
            cache: RefCell::default(),
        }
    }

    /// First installed family in the list, or `None` if a generic keyword
    /// (or nothing installed) comes first: then the generic default applies.
    fn resolve(&self, names: &style::FontNames) -> Option<SharedString> {
        if names.is_empty() {
            return None;
        }
        if let Some(hit) = self.cache.borrow().get(names) {
            return hit.clone();
        }
        let mut found = None;
        for name in names.iter() {
            let lower = name.to_lowercase();
            let generic = matches!(
                lower.as_str(),
                "serif" | "sans-serif" | "monospace" | "cursive" | "fantasy" | "system-ui" | "ui-serif"
                    | "ui-sans-serif" | "ui-monospace" | "ui-rounded" | "-apple-system" | "blinkmacsystemfont"
                    | "math" | "emoji"
            );
            if generic {
                break;
            }
            if let Some(installed) = self.installed.get(&lower) {
                found = Some(installed.clone());
                break;
            }
        }
        self.cache.borrow_mut().insert(names.clone(), found.clone());
        found
    }
}

/// Colors and fonts for page content. Kept separate from the app theme: pages
/// render on a light "canvas" like other browsers do unless they opt into dark.
#[derive(Clone)]
pub struct Palette {
    pub canvas: Hsla,
    pub foreground: Hsla,
    pub link: Hsla,
    pub muted: Hsla,
    pub sans: SharedString,
    pub serif: SharedString,
    pub mono: SharedString,
    pub fonts: Rc<FontBook>,
}

impl Palette {
    pub fn light(fonts: Rc<FontBook>) -> Self {
        Self {
            canvas: rgb(0xffffff).into(),
            foreground: rgb(0x1a1a1a).into(),
            link: rgb(0x1a0dab).into(),
            muted: rgb(0x6b6b6b).into(),
            sans: ".SystemUIFont".into(),
            serif: "Times New Roman".into(),
            mono: "Menlo".into(),
            fonts,
        }
    }

    pub fn color(&self, c: Color) -> Hsla {
        match c {
            Color::Foreground | Color::CurrentColor => self.foreground,
            Color::Link => self.link,
            Color::Muted => self.muted,
            Color::Rgba(v) | Color::LightDark(v, _) => rgba(v).into(),
        }
    }

    /// The family to draw with: the page's first installed font, else our default for its kind.
    pub(crate) fn font(&self, names: &style::FontNames, generic: FontFamily) -> SharedString {
        self.fonts.resolve(names).unwrap_or_else(|| self.family(generic))
    }

    fn family(&self, f: FontFamily) -> SharedString {
        match f {
            FontFamily::Sans => self.sans.clone(),
            FontFamily::Serif => self.serif.clone(),
            FontFamily::Mono => self.mono.clone(),
        }
    }
}

pub type LinkClick = Rc<dyn Fn(&Url, &mut Window, &mut App)>;
pub type LinkHover = Rc<dyn Fn(Option<&Url>, &mut Window, &mut App)>;
/// A block's element started (true) or stopped (false) being under the pointer.
pub type BlockHover = Rc<dyn Fn(page::NodeId, bool, &mut Window, &mut App)>;
/// The inline element under the pointer within paragraph `id` changed.
pub type InlineHover = Rc<dyn Fn(usize, Option<page::NodeId>, &mut Window, &mut App)>;

/// Everything rendering needs besides the boxes themselves.
#[derive(Clone)]
pub struct RenderCx {
    pub palette: Palette,
    pub images: ImageStore,
    /// Bumped whenever an image finishes loading (cached inline layouts with
    /// images in them must be redone: their sizes may have changed).
    pub images_generation: u64,
    pub on_link_click: LinkClick,
    pub on_link_hover: LinkHover,
    /// Report hover per element (only when the page has `:hover`-style rules).
    pub track_hover: bool,
    pub on_block_hover: BlockHover,
    pub on_inline_hover: InlineHover,
}

pub(crate) fn cursor_style(c: style::Cursor) -> Option<CursorStyle> {
    use style::Cursor as C;
    Some(match c {
        C::Auto => return None,
        C::Pointer => CursorStyle::PointingHand,
        C::Text => CursorStyle::IBeam,
        C::NotAllowed => CursorStyle::OperationNotAllowed,
        C::Grab => CursorStyle::OpenHand,
        C::Grabbing => CursorStyle::ClosedHand,
        C::Crosshair => CursorStyle::Crosshair,
        C::Move => CursorStyle::OpenHand,
        C::ResizeColumn => CursorStyle::ResizeLeftRight,
        C::ResizeRow => CursorStyle::ResizeUpDown,
    })
}

fn len(l: CssLength) -> Length {
    match l {
        CssLength::Px(v) => px(v).into(),
        CssLength::Percent(p) => relative(p / 100.0).into(),
        CssLength::Auto => Length::Auto,
    }
}

fn definite(l: CssLength) -> DefiniteLength {
    match l {
        CssLength::Px(v) => px(v).into(),
        CssLength::Percent(p) => relative(p / 100.0),
        CssLength::Auto => px(0.).into(),
    }
}

fn radius(l: CssLength) -> Pixels {
    match l {
        CssLength::Px(v) => px(v),
        // Percent radii are mostly `50%` circles/pills; GPUI clamps (we hope) —
        // keep it to a size that can't overflow small boxes into nothing.
        CssLength::Percent(p) if p >= 50.0 => px(500.),
        CssLength::Percent(p) => px(p),
        CssLength::Auto => px(0.),
    }
}

fn line_height(lh: LineHeight) -> DefiniteLength {
    match lh {
        LineHeight::Normal => relative(1.3),
        LineHeight::Number(n) => relative(n),
        LineHeight::Px(v) => px(v).into(),
    }
}

/// Render one top-level row (a list item in the virtualized page list).
pub fn render_row(row: &Row, cx: &RenderCx) -> AnyElement {
    let margin = row.content.margin();
    let mut inner = div()
        .w_full()
        .flex()
        .flex_col()
        .pt(px(row.space_before))
        .pl(px(row.indent_left))
        .pr(px(row.indent_right))
        .child(render_box(
            &row.content,
            cx,
            Edges { top: CssLength::ZERO, bottom: CssLength::ZERO, ..margin },
            true,
        ))
        .children(row.absolutes.iter().map(|b| deferred(render_box(b, cx, b.margin(), false)).into_any_element()))
        .into_any_element();

    // Re-create each split container around this slice, innermost first.
    for (depth, wrapper) in row.wrappers.iter().enumerate().rev() {
        let s = &wrapper.style;
        let edge = row.edges.get(depth).copied().unwrap_or_default();
        inner = div()
            .w_full()
            .flex()
            .flex_col()
            // Padding, not margin: margins would add to the full width and overflow.
            .pl(px(wrapper.indent_left))
            .pr(px(wrapper.indent_right))
            .pt(px(edge.above))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .pb(px(edge.below))
                    .ml(len(s.margin.left))
                    .mr(len(s.margin.right))
                    .pl(definite(s.padding.left))
                    .pr(definite(s.padding.right))
                    .border_l(px(s.border_width.left))
                    .border_r(px(s.border_width.right))
                    .when(s.has_border(), |d| d.border_color(cx.palette.color(s.border_color.left)))
                    .when_some(s.painted_background(), |d, bg| d.bg(cx.palette.color(bg)))
                    .when(!s.width.is_auto(), |d| d.w(len(s.outer_width(s.width))))
                    .when(!s.min_width.is_auto(), |d| d.min_w(len(s.outer_width(s.min_width))))
                    .when(!s.max_width.is_auto(), |d| d.max_w(len(s.outer_width(s.max_width))))
                    .when(fills_with_auto_margins(s, true), |d| d.w_full())
                    .child(inner),
            )
            .into_any_element();
    }
    inner
}

/// In CSS block flow, an auto-width block fills its container even with auto
/// margins (they only center it once max-width bites). In flexbox, which we
/// lay blocks out with, auto margins shrink-wrap instead, so ask for full width.
/// Tables are the exception: they shrink-to-fit in CSS too.
fn fills_with_auto_margins(s: &ComputedStyle, in_flow: bool) -> bool {
    in_flow && s.width.is_auto() && (s.margin.left.is_auto() || s.margin.right.is_auto())
}

/// The box's own visuals: margin, padding, border, background, size, flex-item props.
fn apply_box(el: Div, s: &ComputedStyle, margin: Edges<CssLength>, cx: &RenderCx) -> Div {
    let palette = &cx.palette;
    let border_color = [s.border_color.top, s.border_color.left, s.border_color.bottom, s.border_color.right]
        .into_iter()
        .zip([s.border_width.top, s.border_width.left, s.border_width.bottom, s.border_width.right])
        .find(|&(_, w)| w > 0.0)
        .map(|(c, _)| palette.color(c));
    el.mt(len(margin.top))
        .mr(len(margin.right))
        .mb(len(margin.bottom))
        .ml(len(margin.left))
        .pt(definite(s.padding.top))
        .pr(definite(s.padding.right))
        .pb(definite(s.padding.bottom))
        .pl(definite(s.padding.left))
        .border_t(px(s.border_width.top))
        .border_r(px(s.border_width.right))
        .border_b(px(s.border_width.bottom))
        .border_l(px(s.border_width.left))
        .when_some(border_color, |d, c| d.border_color(c))
        .rounded_tl(radius(s.border_radius[0]))
        .rounded_tr(radius(s.border_radius[1]))
        .rounded_br(radius(s.border_radius[2]))
        .rounded_bl(radius(s.border_radius[3]))
        .when_some(s.painted_background(), |d, bg| d.bg(palette.color(bg)))
        // GPUI sizes border boxes; CSS's default content-box sizes exclude padding/border.
        .when(!s.width.is_auto(), |d| d.w(len(s.outer_width(s.width))))
        .when(!s.height.is_auto(), |d| d.h(len(s.outer_height(s.height))))
        .when(!s.min_width.is_auto(), |d| d.min_w(len(s.outer_width(s.min_width))))
        .when(!s.min_height.is_auto(), |d| d.min_h(len(s.outer_height(s.min_height))))
        .when(!s.max_width.is_auto(), |d| d.max_w(len(s.outer_width(s.max_width))))
        .when(!s.max_height.is_auto(), |d| d.max_h(len(s.outer_height(s.max_height))))
        .when(s.overflow != Overflow::Visible, |d| d.overflow_hidden())
        .when(s.opacity < 1.0, |d| d.opacity(s.opacity))
        .flex_grow(s.flex_grow)
        .flex_shrink(s.flex_shrink)
        .when(!s.flex_basis.is_auto(), |d| d.flex_basis(len(s.flex_basis)))
        .map(|d| match s.align_self {
            Some(Align::Start) => d.self_start(),
            Some(Align::Center) => d.self_center(),
            Some(Align::End) => d.self_end(),
            Some(Align::Stretch) => d.self_stretch(),
            Some(Align::Baseline) => d.self_baseline(),
            None => d,
        })
        .map(|d| match s.grid_column {
            Some(GridSpan::Span(n)) => d.col_span(n),
            Some(GridSpan::Full) => d.col_span_full(),
            None => d,
        })
        .map(|d| apply_position(d, s))
}

/// `position` + `top`/`right`/`bottom`/`left`. Absolute boxes are placed
/// relative to their parent's box (in CSS: the nearest positioned ancestor,
/// which usually *is* the parent); fixed ones relative to the viewport overlay.
fn apply_position(d: Div, s: &ComputedStyle) -> Div {
    let d = match s.position {
        style::Position::Absolute | style::Position::Fixed => d.absolute(),
        style::Position::Relative => d.relative(),
        style::Position::Static | style::Position::Sticky => return d,
    };
    let inset = |d: Div, side: style::Side| -> Div {
        let v = s.inset.get(side);
        if v.is_auto() {
            return d;
        }
        match side {
            style::Side::Top => d.top(len(v)),
            style::Side::Right => d.right(len(v)),
            style::Side::Bottom => d.bottom(len(v)),
            style::Side::Left => d.left(len(v)),
        }
    };
    style::Side::ALL.into_iter().fold(d, inset)
}

/// An inline-block's subtree (its margins are handled by the line it sits in).
pub(crate) fn render_block_box(block: &Block, cx: &RenderCx) -> AnyElement {
    render_block(block, cx, Edges::all(CssLength::ZERO), false)
}

/// A `position: fixed` box, for the viewport overlay layer.
pub fn render_fixed(b: &LayoutBox, cx: &RenderCx) -> AnyElement {
    render_box(b, cx, b.margin(), false)
}

fn is_out_of_flow(b: &LayoutBox) -> bool {
    matches!(b, LayoutBox::Block(b) if matches!(b.style.position, style::Position::Absolute | style::Position::Fixed))
}

/// Render a box with the margins its parent's layout decided on.
fn render_box(b: &LayoutBox, cx: &RenderCx, margin: Edges<CssLength>, in_flow: bool) -> AnyElement {
    match b {
        LayoutBox::Paragraph(p) => render_paragraph(p, cx),
        LayoutBox::Block(block) => render_block(block, cx, margin, in_flow),
        LayoutBox::Image(image) => render_image(image, cx, margin),
    }
}

/// Block flow: stack children vertically, collapsing adjacent vertical margins.
fn render_flow_children(children: &[LayoutBox], cx: &RenderCx) -> Vec<AnyElement> {
    let mut prev_bottom = 0.0f32;
    // Absolutely positioned children don't take part in margin collapsing.
    let last = children.iter().rposition(|c| !is_out_of_flow(c)).unwrap_or(0);
    children
        .iter()
        .enumerate()
        .map(|(ix, child)| {
            if is_out_of_flow(child) {
                // Positioned boxes paint above in-flow content (and their
                // parent's border, which GPUI paints after children).
                return deferred(render_box(child, cx, child.margin(), false)).into_any_element();
            }
            let m = child.margin();
            let top = m.top.px_or_zero();
            let space = if ix == 0 { top } else { prev_bottom.max(top) };
            prev_bottom = m.bottom.px_or_zero();
            let bottom = if ix == last { prev_bottom } else { 0.0 };
            render_box(child, cx, Edges { top: CssLength::Px(space), bottom: CssLength::Px(bottom), ..m }, true)
        })
        .collect()
}

/// Flex/grid items: margins apply as-is.
fn render_items(children: &[LayoutBox], cx: &RenderCx) -> Vec<AnyElement> {
    children
        .iter()
        .map(|child| {
            let el = render_box(child, cx, child.margin(), false);
            if is_out_of_flow(child) { deferred(el).into_any_element() } else { el }
        })
        .collect()
}

fn render_block(block: &Block, cx: &RenderCx, margin: Edges<CssLength>, in_flow: bool) -> AnyElement {
    let s = &*block.style;
    let fill = block.kind != BlockKind::Table && fills_with_auto_margins(s, in_flow);
    let base = apply_box(div(), s, margin, cx)
        .text_color(cx.palette.color(s.color))
        .when(fill, |d| d.w_full())
        .when(s.min_width.is_auto(), |d| d.min_w(px(block.min_content)));
    let scrolls = s.overflow == Overflow::Scroll && (!s.height.is_auto() || !s.max_height.is_auto());
    // Background image / mask icon, painted first (behind the content).
    let base = match image_layer(s, cx) {
        Some(layer) => base.relative().child(layer),
        None => base,
    };

    let el = match &block.kind {
        BlockKind::Normal | BlockKind::Table => {
            base.flex().flex_col().children(render_flow_children(&block.children, cx))
        }
        BlockKind::TableRow => base.flex().flex_row().children(render_items(&block.children, cx)),
        BlockKind::TableCell => base
            .flex()
            .flex_col()
            .map(|d| match block.cell {
                // Column-shared sizing (see `layout_table`) so rows line up.
                Some(cell) => d
                    .flex_basis(px(cell.basis))
                    .flex_grow(cell.grow)
                    .flex_shrink(1.0)
                    .min_w(px(cell.min)),
                None => d,
            })
            .children(render_flow_children(&block.children, cx)),
        BlockKind::ListItem { marker } => base
            .relative()
            .flex()
            .flex_col()
            .when_some(marker.clone(), |d, marker| {
                d.child(
                    div()
                        .absolute()
                        .top(definite(s.padding.top))
                        .left(px(-36.))
                        .w(px(30.))
                        .flex()
                        .justify_end()
                        .text_size(px(s.font_size))
                        .line_height(line_height(s.line_height))
                        .text_color(cx.palette.color(s.color))
                        .child(marker),
                )
            })
            .children(render_flow_children(&block.children, cx)),
        BlockKind::Flex => {
            let d = base.flex().map(|d| match s.flex_direction {
                FlexDirection::Row => d.flex_row(),
                FlexDirection::RowReverse => d.flex_row_reverse(),
                FlexDirection::Column => d.flex_col(),
                FlexDirection::ColumnReverse => d.flex_col_reverse(),
            });
            let d = apply_gap(apply_alignment(d.when(s.flex_wrap, |d| d.flex_wrap()), s), s);
            d.children(render_items(&block.children, cx))
        }
        BlockKind::Grid => {
            let d = base.grid().grid_cols(s.grid_columns.unwrap_or(1));
            apply_gap(apply_alignment(d, s), s).children(render_items(&block.children, cx))
        }
    };

    let el = match cursor_style(s.cursor) {
        Some(c) => el.cursor(c),
        None => el,
    };
    let hover = block.node.filter(|_| cx.track_hover);
    if !scrolls && hover.is_none() {
        return el.into_any_element();
    }
    let el = el.id(("block", block.id));
    let el = if scrolls { el.overflow_y_scroll() } else { el };
    match hover {
        Some(node) => {
            let report = cx.on_block_hover.clone();
            el.on_hover(move |hovered, window, cx| report(node, *hovered, window, cx)).into_any_element()
        }
        None => el.into_any_element(),
    }
}

/// An `<img>`: sized from CSS, then its attributes, then its intrinsic size,
/// keeping the aspect ratio when only one dimension is known.
fn render_image(i: &ImageBox, cx: &RenderCx, margin: Edges<CssLength>) -> AnyElement {
    let s = &*i.style;
    let state = cx.images.borrow().get(&*i.src).cloned();
    let intrinsic = match &state {
        Some(ImageState::Ready { width, height, .. }) => Some((width / i.density, height / i.density)),
        _ => None,
    };
    let ratio = match (i.attr_width, i.attr_height) {
        (Some(w), Some(h)) if h > 0.0 => Some(w / h),
        _ => intrinsic.map(|(w, h)| w / h),
    };
    let natural_width = i.attr_width.or_else(|| match (i.attr_height, ratio) {
        (Some(h), Some(r)) => Some(h * r),
        _ => intrinsic.map(|(w, _)| w),
    });
    let natural_height = i.attr_height.or(intrinsic.map(|(_, h)| h));

    let mut d = apply_box(div(), s, margin, cx);
    if s.width.is_auto() {
        let from_css_height = match (s.height, ratio) {
            (CssLength::Px(h), Some(r)) => Some(h * r),
            _ => None,
        };
        if let Some(w) = from_css_height.or(natural_width) {
            d = d.w(px(w));
        }
    }
    if s.height.is_auto() {
        match ratio {
            Some(r) => d.style().aspect_ratio = Some(r),
            None => {
                if let Some(h) = natural_height {
                    d = d.h(px(h));
                }
            }
        }
    }

    let content = match state {
        Some(ImageState::Ready { image, .. }) => img(image).size_full().object_fit(ObjectFit::Fill).into_any_element(),
        Some(ImageState::Failed) | None if natural_width.is_none() => match &i.alt {
            Some(alt) => div()
                .text_sm()
                .italic()
                .text_color(cx.palette.muted)
                .child(alt.clone())
                .into_any_element(),
            None => div().into_any_element(),
        },
        _ => div().into_any_element(),
    };

    match &i.link {
        Some(url) => {
            let url = url.clone();
            let on_click = cx.on_link_click.clone();
            d.id(("img", i.id))
                .cursor_pointer()
                .on_click(move |_, window, cx| on_click(&url, window, cx))
                .child(content)
                .into_any_element()
        }
        None => d.child(content).into_any_element(),
    }
}

/// A box's CSS background image, or its mask icon, as an absolutely
/// positioned layer filling the box.
fn image_layer(s: &ComputedStyle, cx: &RenderCx) -> Option<AnyElement> {
    let lookup = |url: &str| -> Option<(std::sync::Arc<Image>, f32, f32)> {
        let url = Url::parse(url).ok()?;
        match cx.images.borrow().get(&url)? {
            ImageState::Ready { image, width, height } => Some((image.clone(), *width, *height)),
            _ => None,
        }
    };
    let layer = |size: BgSize, pos: BgPos| {
        let d = div().absolute().top_0().left_0().size_full().flex().overflow_hidden();
        let d = if pos.x < 0.25 {
            d.justify_start()
        } else if pos.x > 0.75 {
            d.justify_end()
        } else {
            d.justify_center()
        };
        let d = if pos.y < 0.25 {
            d.items_start()
        } else if pos.y > 0.75 {
            d.items_end()
        } else {
            d.items_center()
        };
        (d, size)
    };
    let sized = |el: Div, size: BgSize, (w, h): (f32, f32)| -> Div {
        let ratio = if h > 0.0 { w / h } else { 1.0 };
        match size {
            BgSize::Contain | BgSize::Cover => el.size_full(),
            BgSize::Auto => el.w(px(w)).h(px(h)),
            BgSize::Explicit(cw, ch) => match (cw, ch) {
                (CssLength::Px(cw), CssLength::Px(ch)) => el.w(px(cw)).h(px(ch)),
                (CssLength::Px(cw), _) => el.w(px(cw)).h(px(cw / ratio)),
                (_, CssLength::Px(ch)) => el.w(px(ch * ratio)).h(px(ch)),
                (cw, ch) => el.w(len(cw)).h(len(ch)),
            },
        }
    };

    if s.masked {
        // A stencil filled with the background color: draw the SVG in that color.
        let (image, w, h) = lookup(s.mask_url.as_deref()?)?;
        if image.format != ImageFormat::Svg {
            return None;
        }
        let color = cx.palette.color(s.mask_color()?);
        let path = crate::images::svg_asset_path(&image);
        let (d, size) = layer(s.mask_size, s.mask_position);
        let icon = sized(div(), size, (w, h)).child(svg().path(path).size_full().text_color(color));
        return Some(d.child(icon).into_any_element());
    }
    let (image, w, h) = lookup(s.background_url.as_deref()?)?;
    let (d, size) = layer(s.background_size, s.background_position);
    let fit = match size {
        BgSize::Cover => ObjectFit::Cover,
        BgSize::Contain => ObjectFit::Contain,
        _ => ObjectFit::Fill,
    };
    let picture = sized(div(), size, (w, h)).child(img(image).size_full().object_fit(fit));
    Some(d.child(picture).into_any_element())
}

fn apply_gap(mut d: Div, s: &ComputedStyle) -> Div {
    let gap = &mut d.style().gap;
    gap.width = Some(definite(s.column_gap));
    gap.height = Some(definite(s.row_gap));
    d
}

fn apply_alignment(d: Div, s: &ComputedStyle) -> Div {
    let d = match s.justify_content {
        Some(Justify::Start) | None => d,
        Some(Justify::Center) => d.justify_center(),
        Some(Justify::End) => d.justify_end(),
        Some(Justify::SpaceBetween) => d.justify_between(),
        Some(Justify::SpaceAround) => d.justify_around(),
        Some(Justify::SpaceEvenly) => d.justify_evenly(),
    };
    match s.align_items {
        Some(Align::Stretch) | None => d,
        Some(Align::Start) => d.items_start(),
        Some(Align::Center) => d.items_center(),
        Some(Align::End) => d.items_end(),
        Some(Align::Baseline) => d.items_baseline(),
    }
}

pub(crate) fn text_run(span_len: usize, st: &InlineStyle, palette: &Palette) -> TextRun {
    let color = palette.color(st.color);
    TextRun {
        len: span_len,
        font: Font {
            family: palette.font(&st.font_names, st.font_family),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight(st.font_weight as f32),
            style: if st.italic { FontStyle::Italic } else { FontStyle::Normal },
        },
        color,
        background_color: st.background.map(|c| palette.color(c)),
        underline: st.underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(color),
            wavy: false,
        }),
        strikethrough: st.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(color),
        }),
    }
}

fn render_paragraph(p: &Arc<Paragraph>, cx: &RenderCx) -> AnyElement {
    let flow = crate::inline::InlineFlow::new(p.clone(), cx);
    if p.style.white_space.wraps() {
        flow.into_any_element()
    } else {
        // Unwrapped text (pre, nowrap) scrolls sideways instead of overflowing.
        div()
            .id(("nowrap", p.id))
            .overflow_x_scroll()
            .child(flow)
            .into_any_element()
    }
}
