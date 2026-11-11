//! Layout tree → GPUI elements.

use std::cell::{Cell, RefCell};
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

use crate::boxes::{Block, BlockKind, Control, ImageBox, InlineStyle, LayoutBox, Paragraph, Row};
use crate::images::{ImageState, ImageStore};

/// The fonts installed on this system, for resolving CSS `font-family` lists.
#[derive(Default)]
pub struct FontBook {
    /// lowercase name → the system's spelling (or, for web fonts, the
    /// family the font file itself declares)
    installed: RefCell<HashMap<String, SharedString>>,
    /// Resolved lists, keyed by the names themselves. (Not by `Arc` address:
    /// after a restyle frees the old lists, new ones can reuse the addresses.)
    cache: RefCell<HashMap<style::FontNames, Option<SharedString>>>,
}

impl FontBook {
    pub fn new(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            installed: RefCell::new(names.into_iter().map(|n| (n.to_lowercase(), n.into())).collect()),
            cache: RefCell::default(),
        }
    }

    /// A web font arrived: pages naming `css_family` get `family` (the name
    /// inside the font file, which is what the text system knows it by).
    pub fn add(&self, css_family: &str, family: &str) {
        self.installed.borrow_mut().insert(css_family.to_lowercase(), family.to_owned().into());
        self.installed.borrow_mut().entry(family.to_lowercase()).or_insert_with(|| family.to_owned().into());
        self.cache.borrow_mut().clear();
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
            if let Some(installed) = self.installed.borrow().get(&lower) {
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
    /// CSS `filter` color functions the content being drawn is under
    /// (innermost element's first): every color goes through them.
    pub filter: Option<Arc<[style::Filter]>>,
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
            filter: None,
        }
    }

    pub fn color(&self, c: Color) -> Hsla {
        let color = match c {
            Color::Foreground | Color::CurrentColor => self.foreground,
            Color::Link => self.link,
            Color::Muted => self.muted,
            Color::Rgba(v) | Color::LightDark(v, _) => rgba(v).into(),
        };
        match &self.filter {
            Some(filters) => {
                let c = gpui_kit::Rgba::from(color);
                let [r, g, b, a] = filters.iter().fold([c.r, c.g, c.b, c.a], |c, f| f.color(c));
                gpui_kit::Rgba { r, g, b, a }.into()
            }
            None => color,
        }
    }

    /// This palette under `filters` too (a filtered element's own,
    /// applied before the ones it's inside).
    fn under(&self, filters: &[style::Filter]) -> Palette {
        let own = filters.iter().copied().filter(|f| f.is_color());
        let list: Vec<style::Filter> = own.chain(self.filter.iter().flat_map(|f| f.iter().copied())).collect();
        Palette { filter: (!list.is_empty()).then(|| list.into()), ..self.clone() }
    }

    /// The family to draw with: the page's first installed font, else our default for its kind.
    pub fn font(&self, names: &style::FontNames, generic: FontFamily) -> SharedString {
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
/// An element's painted box (window coordinates), for scripts measuring it.
pub type BoundsReport = Rc<dyn Fn(page::NodeId, Bounds<Pixels>, &mut Window, &mut App)>;
/// The inline element under the pointer within paragraph `id` changed.
pub type InlineHover = Rc<dyn Fn(usize, Option<page::NodeId>, &mut Window, &mut App)>;
/// Draws a form control's widget (inside the box the page styled for it).
pub type ControlRenderer = Rc<dyn Fn(&Control, &ComputedStyle, &mut Window, &mut App) -> AnyElement>;

/// Everything rendering needs besides the boxes themselves.
#[derive(Clone)]
pub struct RenderCx {
    pub palette: Palette,
    pub images: ImageStore,
    /// The picture each `<img>`/`<canvas>` last showed: drawn until a new
    /// `src` is ready (a canvas gets a new one every frame it changes, and
    /// blanking while each decodes made animations flicker).
    pub shown_images: Rc<RefCell<HashMap<page::NodeId, Arc<url::Url>>>>,
    /// Bumped whenever an image finishes loading (cached inline layouts with
    /// images in them must be redone: their sizes may have changed).
    pub images_generation: u64,
    /// The rounded clipping box text being built sits in (see
    /// `ROUND_CLIP`), for its inline images, painted after it's built.
    pub round_clip: Option<(f32, f32, f32)>,
    /// The stacking context text being built sits in (see `STACKING`):
    /// its inline-blocks are rendered later, when it's no longer current.
    pub stacking: (usize, u32),
    /// Bumped when web fonts arrive (every cached text layout is stale).
    pub fonts_generation: u64,
    pub on_link_click: LinkClick,
    pub on_link_hover: LinkHover,
    /// Report hover per element (only when the page has `:hover`-style rules).
    pub track_hover: bool,
    pub on_block_hover: BlockHover,
    /// Elements whose geometry scripts read: report where they're painted.
    pub wants_bounds: Rc<dyn Fn(page::NodeId) -> bool>,
    pub on_bounds: BoundsReport,
    pub on_inline_hover: InlineHover,
    pub controls: ControlRenderer,
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
        .child(in_flow(
            &row.content,
            cx,
            Edges { top: CssLength::ZERO, bottom: CssLength::ZERO, ..margin },
            true,
        ))
        .children(row.absolutes.iter().map(|b| positioned(b, cx, b.margin())))
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
                    .when(!s.width.is_auto(), |d| d.w(len(box_width(s, s.width))))
                    .when(!s.min_width.is_auto(), |d| d.min_w(len(box_width(s, s.min_width))))
                    .when(!s.max_width.is_auto(), |d| d.max_w(len(box_width(s, s.max_width))))
                    .when(content_box(s), |mut d| {
                        d.style().content_box = Some(true);
                        d
                    })
                    .when(fills_with_auto_margins(s, true), |d| d.w_full())
                    .child(inner)
                    // Scripts measuring the container get each slice (the
                    // tab unions the slices it draws).
                    .when_some(wrapper.node.filter(|&n| (cx.wants_bounds)(n)), |d, node| {
                        let report = cx.on_bounds.clone();
                        d.relative().child(
                            canvas(move |bounds, window, cx| report(node, bounds, window, cx), |_, _, _, _| {})
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                        )
                    }),
            )
            .into_any_element();
        // A raised split container (Stripe's `z-index: 2` header): each
        // slice paints at its place in the stacking order.
        let raised_wrapper = matches!(s.position, style::Position::Relative | style::Position::Sticky) && s.z_index.is_some_and(|z| z > 0);
        if std::env::var_os("FEATHER_DEBUG_Z").is_some() && s.z_index.is_some() {
            eprintln!("z wrapper: {:?} z={:?} pos={:?} raised={raised_wrapper}", wrapper.node, s.z_index, s.position);
        }
        if raised_wrapper && STACKING.with(|st| st.get().1) == 0 {
            let rank = 1 + s.z_index.unwrap_or(0).clamp(0, (1 << Z_BITS) - 2) as usize;
            let priority = rank << (Z_BITS * (CONTEXT_LEVELS - 1));
            inner = ClippedDeferred { child: Some(inner), priority }.into_any_element();
        }
    }
    inner
}

/// The shadows GPUI can draw faithfully for this box. CSS clips an outer
/// shadow to outside the box; GPUI draws the whole shape behind it, which a
/// box without a background shows (a `0 1px` underline became a grey slab).
fn visible_shadows(shadows: &[style::Shadow], s: &ComputedStyle, palette: &Palette) -> Vec<BoxShadow> {
    let opaque = s.painted_background().is_some();
    let kept: Vec<style::Shadow> = shadows.iter().copied().filter(|sh| sh.inset || opaque).collect();
    box_shadows(&kept, palette)
}

/// CSS `box-shadow` layers as GPUI shadows.
pub(crate) fn box_shadows(shadows: &[style::Shadow], palette: &Palette) -> Vec<BoxShadow> {
    shadows
        .iter()
        .map(|sh| BoxShadow {
            color: palette.color(sh.color),
            offset: point(px(sh.x), px(sh.y)),
            // GPUI's own shadow presets use CSS's numbers: same meaning.
            blur_radius: px(sh.blur),
            spread_radius: px(sh.spread),
            inset: sh.inset,
        })
        .collect()
}

/// In CSS block flow, an auto-width block fills its container even with auto
/// margins (they only center it once max-width bites). In flexbox, which we
/// lay blocks out with, auto margins shrink-wrap instead, so ask for full width.
/// Tables are the exception: they shrink-to-fit in CSS too.
/// Lay the box out with CSS content-box sizing (taffy adds padding and
/// border to its sizes). Pixel sizes are converted to border-box ones
/// instead (`ComputedStyle::outer_width`), which also covers the sizes the
/// renderer sets itself; a percentage can't be, so a content-box box with
/// one, and with padding or border, is laid out as content-box. Not when it
/// fills its container (auto width, auto margins: sized as a border box).
fn content_box(s: &ComputedStyle) -> bool {
    use CssLength::Percent as P;
    !s.border_box
        && !fills_with_auto_margins(s, true)
        && (s.horizontal_edges() > 0.0 || s.vertical_edges() > 0.0)
        && [s.width, s.height, s.min_width, s.min_height, s.max_width, s.max_height].iter().any(|l| matches!(l, P(_)))
}

/// `width`-type lengths as GPUI's layout takes them for this box (see
/// [`content_box`]).
fn box_width(s: &ComputedStyle, l: CssLength) -> CssLength {
    if content_box(s) { l } else { s.outer_width(l) }
}

fn box_height(s: &ComputedStyle, l: CssLength) -> CssLength {
    if content_box(s) { l } else { s.outer_height(l) }
}

fn fills_with_auto_margins(s: &ComputedStyle, in_flow: bool) -> bool {
    in_flow && s.width.is_auto() && !s.fit_width && (s.margin.left.is_auto() || s.margin.right.is_auto())
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
        .when_some(s.box_background(), |d, bg| d.bg(palette.color(bg)))
        // GPUI sizes border boxes; CSS's default content-box sizes exclude
        // padding/border (see `content_box`).
        .when(content_box(s), |mut d| {
            d.style().content_box = Some(true);
            d
        })
        .when(!s.width.is_auto(), |d| d.w(len(box_width(s, s.width))))
        .when(!s.height.is_auto(), |d| d.h(len(box_height(s, s.height))))
        .when(!s.min_width.is_auto(), |d| d.min_w(len(box_width(s, s.min_width))))
        .when(!s.min_height.is_auto(), |d| d.min_h(len(box_height(s, s.min_height))))
        .when(!s.max_width.is_auto(), |d| d.max_w(len(box_width(s, s.max_width))))
        .when(!s.max_height.is_auto(), |d| d.max_h(len(box_height(s, s.max_height))))
        .when(s.overflow != Overflow::Visible, |d| d.overflow_hidden())
        .when(s.opacity < 1.0, |d| d.opacity(s.opacity))
        // `aspect-ratio`: the missing side follows the given one.
        .when_some(s.aspect_ratio.filter(|_| s.width.is_auto() || s.height.is_auto()), |mut d, r| {
            d.style().aspect_ratio = Some(r);
            d
        })
        .map(|d| {
            // `filter: drop-shadow()`: drawn as the box's shadow.
            let mut shadows = s.shadows.as_ref().map(|sh| visible_shadows(sh, s, palette)).unwrap_or_default();
            if let Some(filters) = &s.filter {
                let drops: Vec<style::Shadow> =
                    filters.iter().filter_map(|f| if let style::Filter::DropShadow(sh) = f { Some(*sh) } else { None }).collect();
                shadows.extend(box_shadows(&drops, palette));
            }
            if shadows.is_empty() { d } else { d.shadow(shadows) }
        })
        .flex_grow(s.flex_grow)
        .flex_shrink(s.flex_shrink)
        .when(!s.flex_basis.is_auto(), |d| d.flex_basis(len(s.flex_basis)))
        // `width: fit-content` in a column: don't stretch across it (auto
        // margins still center it).
        .when(s.fit_width && s.align_self.is_none(), |d| d.self_start())
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
        .when_some(s.grid_row_start.filter(|n| *n > 0), |d, n| d.row_start(n))
        .when_some(s.grid_column_start.filter(|n| *n > 0), |d, n| d.col_start(n))
        .map(|d| apply_position(d, s))
}

/// `position` + `top`/`right`/`bottom`/`left`. Absolute boxes are placed
/// relative to their parent's box (in CSS: the nearest positioned ancestor,
/// which usually *is* the parent); fixed ones relative to the viewport overlay.
/// A grid container's template for GPUI's layout (taffy's CSS grid).
fn css_grid(s: &ComputedStyle) -> CssGrid {
    fn track(t: &style::TrackSize) -> CssTrackSize {
        match t {
            style::TrackSize::Px(v) => CssTrackSize::Px(*v),
            style::TrackSize::Percent(p) => CssTrackSize::Percent(*p),
            style::TrackSize::Fr(f) => CssTrackSize::Fr(*f),
            style::TrackSize::Auto => CssTrackSize::Auto,
            style::TrackSize::MinContent => CssTrackSize::MinContent,
            style::TrackSize::MaxContent => CssTrackSize::MaxContent,
            style::TrackSize::FitContent(v) => CssTrackSize::FitContent(*v),
            style::TrackSize::MinMax(a, b) => CssTrackSize::MinMax(Box::new(track(a)), Box::new(track(b))),
        }
    }
    let names = |n: &[Vec<Arc<str>>]| n.iter().map(|l| l.iter().map(|s| s.to_string()).collect()).collect::<Vec<Vec<String>>>();
    let entries = |t: &Option<Arc<style::GridTemplate>>| {
        t.iter()
            .flat_map(|t| t.entries.iter())
            .map(|e| match e {
                style::TemplateEntry::Track(t) => CssGridTemplateEntry::Track(track(t)),
                style::TemplateEntry::Repeat { count, tracks, names: n } => CssGridTemplateEntry::Repeat {
                    count: match count {
                        style::RepeatCount::Count(c) => CssRepeatCount::Count(*c),
                        style::RepeatCount::AutoFill => CssRepeatCount::AutoFill,
                        style::RepeatCount::AutoFit => CssRepeatCount::AutoFit,
                    },
                    tracks: tracks.iter().map(track).collect(),
                    names: names(n),
                },
            })
            .collect::<Vec<_>>()
    };
    let line_names = |t: &Option<Arc<style::GridTemplate>>| t.as_ref().map(|t| names(&t.names)).unwrap_or_default();
    // `grid-template-areas`: each name's rectangle, as 1-based lines.
    let mut areas: Vec<(String, u16, u16, u16, u16)> = Vec::new();
    let (mut area_rows, mut area_columns) = (0u16, 0u16);
    if let Some(rows) = &s.grid_areas {
        area_rows = rows.len() as u16;
        for (r, row) in rows.iter().enumerate() {
            area_columns = area_columns.max(row.len() as u16);
            for (c, cell) in row.iter().enumerate() {
                if &**cell == "." {
                    continue;
                }
                let (r, c) = (r as u16 + 1, c as u16 + 1);
                match areas.iter_mut().find(|a| a.0 == **cell) {
                    Some(a) => {
                        a.1 = a.1.min(r);
                        a.2 = a.2.max(r + 1);
                        a.3 = a.3.min(c);
                        a.4 = a.4.max(c + 1);
                    }
                    None => areas.push((cell.to_string(), r, r + 1, c, c + 1)),
                }
            }
        }
    }
    let align = |a: style::Align| match a {
        style::Align::Start => AlignItems::Start,
        style::Align::Center => AlignItems::Center,
        style::Align::End => AlignItems::End,
        style::Align::Stretch => AlignItems::Stretch,
        style::Align::Baseline => AlignItems::Baseline,
    };
    CssGrid(Arc::new(CssGridTemplate {
        columns: entries(&s.grid_template_columns),
        rows: entries(&s.grid_template_rows),
        column_names: line_names(&s.grid_template_columns),
        row_names: line_names(&s.grid_template_rows),
        auto_columns: s.grid_auto_columns.iter().map(track).collect(),
        auto_rows: s.grid_auto_rows.iter().map(track).collect(),
        auto_flow_column: s.grid_auto_flow.0,
        auto_flow_dense: s.grid_auto_flow.1,
        areas,
        area_rows,
        area_columns,
        justify_items: s.justify_items.map(align),
    }))
}

/// Where a grid item goes, if it says (GPUI's layout ignores it elsewhere).
fn css_grid_item(s: &ComputedStyle) -> Option<CssGridItem> {
    let (rs, re) = &s.grid_row_lines;
    let (cs, ce) = &s.grid_column_lines;
    if rs.is_auto() && re.is_auto() && cs.is_auto() && ce.is_auto() && s.justify_self.is_none() {
        return None;
    }
    let line = |l: &style::GridLine| match l {
        style::GridLine::Auto => CssGridLine::Auto,
        style::GridLine::Line(n) => CssGridLine::Line(*n),
        style::GridLine::Named(n, i) => CssGridLine::Named(n.to_string(), *i),
        style::GridLine::Span(n) => CssGridLine::Span(*n),
        style::GridLine::NamedSpan(n, i) => CssGridLine::NamedSpan(n.to_string(), *i),
    };
    let justify_self = s.justify_self.map(|a| match a {
        style::Align::Start => AlignSelf::Start,
        style::Align::Center => AlignSelf::Center,
        style::Align::End => AlignSelf::End,
        style::Align::Stretch => AlignSelf::Stretch,
        style::Align::Baseline => AlignSelf::Baseline,
    });
    Some(CssGridItem(Arc::new(CssGridPlacement {
        row_start: line(rs),
        row_end: line(re),
        column_start: line(cs),
        column_end: line(ce),
        justify_self,
    })))
}

fn apply_position(d: Div, s: &ComputedStyle) -> Div {
    let mut d = d;
    if let Some(item) = css_grid_item(s) {
        d.style().css_grid_item = Some(item);
    }
    // `transform: translate()`: shift the box without moving anything else,
    // which is what a relative offset does. Percentages are of its own size.
    // Of the border box: with `box-sizing: border-box` that's the width itself.
    let (ew, eh) = if s.border_box { (0.0, 0.0) } else { (s.horizontal_edges(), s.vertical_edges()) };
    let later = SHIFT_LATER.replace(false);
    let own = |o: style::Offset, size: style::Length, edges: f32| match size {
        style::Length::Px(w) => o.resolve(w + edges),
        // Moved after layout instead (see `Shifted`).
        _ if later => o.px,
        // Unknown size: an estimate (a line of text) beats ignoring it.
        _ if o.pct != 0.0 => o.resolve(s.font_size * 1.4 + edges),
        _ => o.px,
    };
    let tx = own(s.translate.0, s.width, ew);
    let ty = own(s.translate.1, s.height, eh);
    let shifted = |side: style::Side, v: style::Length| -> style::Length {
        let delta = match side {
            style::Side::Left => tx,
            style::Side::Right => -tx,
            style::Side::Top => ty,
            style::Side::Bottom => -ty,
        };
        match v {
            style::Length::Px(p) => style::Length::Px(p + delta),
            other => other,
        }
    };
    let d = match s.position {
        // Percentage insets can't take a pixel shift; a margin moves an
        // absolutely positioned box just the same.
        style::Position::Absolute | style::Position::Fixed if tx != 0.0 || ty != 0.0 => {
            let d = d.absolute();
            let d = if !matches!(s.inset.top, style::Length::Px(_)) && ty != 0.0 {
                d.mt(px(s.margin.top.px_or_zero() + ty))
            } else {
                d
            };
            if !matches!(s.inset.left, style::Length::Px(_)) && tx != 0.0 { d.ml(px(s.margin.left.px_or_zero() + tx)) } else { d }
        }
        style::Position::Absolute | style::Position::Fixed => d.absolute(),
        style::Position::Relative => d.relative(),
        style::Position::Static | style::Position::Sticky if tx != 0.0 || ty != 0.0 => {
            return d.relative().left(px(tx)).top(px(ty));
        }
        style::Position::Static | style::Position::Sticky => return d,
    };
    let inset = |d: Div, side: style::Side| -> Div {
        let mut v = s.inset.get(side);
        // Relative boxes with no offset on a side get the translation there.
        if v.is_auto() && s.position == style::Position::Relative && matches!(side, style::Side::Left | style::Side::Top) {
            v = style::Length::ZERO;
        }
        if !v.is_auto() {
            v = shifted(side, v);
        }
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
    // Laid out with its paragraph, after the boxes around it were rendered:
    // in the rounded clip the paragraph was in (an avatar frame around an
    // inline-block picture).
    let outer = ROUND_CLIP.replace(cx.round_clip);
    let outer_stacking = STACKING.replace(cx.stacking);
    // A relatively positioned one with a `z-index` paints with the
    // positioned boxes, in their order (a hovered badge over its neighbors).
    let is_raised = matches!(block.style.position, style::Position::Relative | style::Position::Sticky)
        && block.style.z_index.is_some_and(|z| z > 0);
    let el = is_raised
        .then(|| deferred(&LayoutBox::Block(block.clone()), true, || render_block(block, cx, Edges::all(CssLength::ZERO), false)))
        .flatten()
        .unwrap_or_else(|| render_block(block, cx, Edges::all(CssLength::ZERO), false));
    STACKING.set(outer_stacking);
    ROUND_CLIP.set(outer);
    el
}

/// The stacking context being rendered into (see `STACKING`).
pub(crate) fn current_stacking() -> (usize, u32) {
    STACKING.with(|s| s.get())
}

/// A box the tab pins over the page (`position: sticky`, scrolled past):
/// drawn as it is in its place.
pub fn render_pinned(b: &LayoutBox, cx: &RenderCx) -> AnyElement {
    // Drawn in the deferred layer, as fixed boxes are: the page's own
    // positioned boxes (BD's card chips) scroll under it, not over it.
    let zero = Edges::all(CssLength::ZERO);
    deferred(b, true, || render_box(b, cx, zero, false)).unwrap_or_else(|| render_box(b, cx, zero, false))
}

/// A `position: fixed` box, for the viewport overlay layer: drawn after
/// the page's own positioned boxes (a fixed header stays over thumbnails
/// scrolling under it), with what's inside it drawn in place.
pub fn render_fixed(b: &LayoutBox, cx: &RenderCx) -> AnyElement {
    // A fixed box always starts a stacking context.
    deferred(b, true, || render_box(b, cx, b.margin(), false)).unwrap_or_else(|| render_box(b, cx, b.margin(), false))
}

/// Does this box start a stacking context (its descendants' `z-index`
/// order among themselves, not with the page)?
fn stacking_context(b: &LayoutBox) -> bool {
    matches!(b, LayoutBox::Block(block) if block.style.z_index.is_some() || block.style.opacity < 1.0)
}

/// Draw `b` (built by `build`) in the deferred layer, in its place in the
/// stacking order: positioned boxes paint over in-flow content in
/// `z-index` order, within the stacking context they're in.
///
/// GPUI orders deferred draws by one number (stably: ties keep tree
/// order), so a box's priority spells out its path through nested
/// stacking contexts, 12 bits per level, outermost first: a context's
/// contents sort among themselves, and with everything outside it as the
/// context does. `None` (drawn in place) past `CONTEXT_LEVELS` contexts or
/// 8 nested deferrals (GPUI allows 10).
fn deferred(b: &LayoutBox, context: bool, build: impl FnOnce() -> AnyElement) -> Option<AnyElement> {
    let (base, level) = STACKING.with(|s| s.get());
    if level >= CONTEXT_LEVELS || DEFER_LEVELS.with(|d| d.get()) >= 8 {
        return None;
    }
    let priority = base + (z_rank(b) << (Z_BITS * (CONTEXT_LEVELS - 1 - level)));
    if std::env::var_os("FEATHER_DEBUG_Z").is_some()
        && let LayoutBox::Block(block) = b
    {
        eprintln!("z: {:?} z={:?} level={level} context={context} priority={priority:x}", block.node, block.style.z_index);
    }
    DEFER_LEVELS.with(|d| d.set(d.get() + 1));
    if context {
        STACKING.with(|s| s.set((priority, level + 1)));
    }
    let el = build();
    STACKING.with(|s| s.set((base, level)));
    DEFER_LEVELS.with(|d| d.set(d.get() - 1));
    Some(ClippedDeferred { child: Some(el), priority }.into_any_element())
}

const Z_BITS: u32 = 12;
const CONTEXT_LEVELS: u32 = 5;

/// A positioned box's place among its context's: `z-index` (auto: 0), 1
/// up (0 is the context's own content), at most 2^12 - 1.
fn z_rank(b: &LayoutBox) -> usize {
    let z = match b {
        LayoutBox::Block(block) => block.style.z_index.unwrap_or(0),
        _ => 0,
    };
    1 + z.clamp(0, (1 << Z_BITS) - 2) as usize
}

/// A relatively positioned (or sticky) box with a positive `z-index`: it
/// stays where it is in the flow, but paints with the positioned boxes, in
/// their order (NASA's hero text, z-index 400, under its z-index 200 shade).
/// Flex and grid items' `z-index` counts without a `position` too.
fn raised(b: &LayoutBox, item: bool) -> bool {
    matches!(b, LayoutBox::Block(block)
        if (item || matches!(block.style.position, style::Position::Relative | style::Position::Sticky))
            && block.style.z_index.is_some_and(|z| z > 0))
}

/// Render an in-flow box, deferring it if it's `raised` (and not already
/// inside a deferred box, where it's drawn in place).
fn in_flow(b: &LayoutBox, cx: &RenderCx, margin: Edges<CssLength>, flow: bool) -> AnyElement {
    // `flow`: in block flow; else a flex or grid item.
    if raised(b, !flow)
        && let Some(el) = deferred(b, true, || render_box(b, cx, margin, flow))
    {
        return el;
    }
    render_box(b, cx, margin, flow)
}

pub(crate) fn is_out_of_flow(b: &LayoutBox) -> bool {
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
    behind_first(render_flow_children_in_order(children, cx), children)
}

/// Negative-`z-index` boxes paint under their siblings: first, since GPUI
/// paints in order (absolute boxes don't care where they are in the list).
fn behind_first(mut elements: Vec<AnyElement>, children: &[LayoutBox]) -> Vec<AnyElement> {
    if !children.iter().any(behind) {
        return elements;
    }
    let mut under = Vec::new();
    let mut rest = Vec::new();
    for (el, child) in elements.drain(..).zip(children) {
        if behind(child) { under.push(el) } else { rest.push(el) }
    }
    under.extend(rest);
    under
}

fn render_flow_children_in_order(children: &[LayoutBox], cx: &RenderCx) -> Vec<AnyElement> {
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
                return positioned(child, cx, child.margin());
            }
            let m = child.margin();
            let top = m.top.px_or_zero();
            let space = if ix == 0 { top } else { prev_bottom.max(top) };
            prev_bottom = m.bottom.px_or_zero();
            let bottom = if ix == last { prev_bottom } else { 0.0 };
            in_flow(child, cx, Edges { top: CssLength::Px(space), bottom: CssLength::Px(bottom), ..m }, true)
        })
        .collect()
}

/// An element, laid out as it is, that says where it landed (its bounds
/// at prepaint): measuring without a box around it.
struct Measured {
    inner: AnyElement,
    report: Box<dyn Fn(Bounds<Pixels>)>,
}

impl Measured {
    fn new(inner: AnyElement, report: impl Fn(Bounds<Pixels>) + 'static) -> Self {
        Measured { inner, report: Box::new(report) }
    }
}

impl IntoElement for Measured {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Measured {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        (self.inner.request_layout(window, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        (self.report)(bounds);
        self.inner.prepaint(window, cx);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.inner.paint(window, cx);
    }
}

/// Where each float row split its items last frame (see `FloatSplit`).
#[derive(Default)]
struct Split {
    /// Items beside the float.
    beside: usize,
    /// Fewer than this left a gap under the float (never go back below).
    floor: usize,
    float_height: f32,
    column_height: f32,
    width: f32,
}

thread_local! {
    /// By (row, offset into its items): a row's later floats split too.
    static SPLITS: RefCell<HashMap<(usize, usize), Split>> = RefCell::new(HashMap::new());
}

/// A float and what follows it (see `FloatSplit`).
fn float_split_row(block: &Block, split: crate::boxes::FloatSplit, base: Div, cx: &RenderCx) -> Div {
    let (float_ix, column_ix) = if split.right { (1, 0) } else { (0, 1) };
    let (Some(float), Some(LayoutBox::Block(column))) = (block.children.get(float_ix), block.children.get(column_ix)) else {
        return base.flex().flex_row().items_start().children(render_items(&block.children, cx));
    };
    base.flex().flex_col().relative().child(float_flow((block.id, 0), split.right, float, &column.children, usize::from(split.initial), cx))
}

/// `float` and `items`: as many items beside it as cover its height (later
/// floats on its side among them stacked under it), the rest below at full
/// width, around the next float there if there is one. Each frame measures
/// both sides and moves the split (it settles in a frame or two).
fn float_flow(key: (usize, usize), right: bool, float: &LayoutBox, items: &[LayoutBox], initial: usize, cx: &RenderCx) -> AnyElement {
    let side = if right { style::Float::Right } else { style::Float::Left };
    let same_side = move |b: &LayoutBox| matches!(b, LayoutBox::Block(f) if f.style.float == side);
    let beside = SPLITS.with_borrow_mut(|m| {
        let s = m.entry(key).or_insert_with(|| Split { beside: initial, floor: 1, ..Default::default() });
        s.beside.clamp(1.min(items.len()), items.len())
    });
    let (next_to, below) = items.split_at(beside);
    // The float column: the float, and later ones that came up beside it.
    let floats: Vec<&LayoutBox> = std::iter::once(float).chain(next_to.iter().filter(|b| same_side(b))).collect();
    let text: Vec<LayoutBox> = next_to.iter().filter(|b| !same_side(b)).cloned().collect();
    let mut float_els = Vec::new();
    for f in floats {
        float_els.extend(render_items(std::slice::from_ref(f), cx));
    }
    let float_el = div().flex().flex_col().flex_none().map(|d| if right { d.items_end() } else { d.items_start() }).children(float_els).into_any_element();
    let float_el = Measured::new(float_el, move |b| {
        SPLITS.with_borrow_mut(|m| {
            if let Some(s) = m.get_mut(&key) {
                s.float_height = f32::from(b.size.height);
            }
        })
    });
    let column_el = div()
        .flex()
        .flex_col()
        .flex_grow(1.0)
        .flex_shrink(1.0)
        .flex_basis(px(0.))
        .min_w(px(0.))
        .children(render_flow_children(&text, cx))
        .into_any_element();
    let column_el = Measured::new(column_el, move |b| {
        SPLITS.with_borrow_mut(|m| {
            if let Some(s) = m.get_mut(&key) {
                s.column_height = f32::from(b.size.height);
            }
        })
    });
    let row = div().flex().flex_row().items_start().w_full();
    let row = if right { row.child(column_el).child(float_el) } else { row.child(float_el).child(column_el) };
    let total = items.len();
    // How many of the first k items are text (not floats), for each k.
    let mut text_counts = Vec::with_capacity(total + 1);
    text_counts.push(0usize);
    for b in items {
        text_counts.push(text_counts.last().copied().unwrap_or(0) + usize::from(!same_side(b)));
    }
    let text_items = move |k: usize| text_counts[k.min(total)].max(1);
    // After both sides are placed: move the split toward covering the float.
    let decide = canvas(
        move |bounds, window, _| {
            let changed = SPLITS.with_borrow_mut(|m| {
                let Some(s) = m.get_mut(&key) else { return false };
                let width = f32::from(bounds.size.width);
                if (width - s.width).abs() > 0.5 {
                    // A new width: what was too few may be enough now.
                    s.width = width;
                    s.floor = 1;
                }
                let (f, c, k) = (s.float_height, s.column_height, s.beside.min(total));
                if f <= 0.0 || k == 0 {
                    return false;
                }
                let average = (c / text_items(k) as f32).max(8.0);
                let next = if c + 1.0 < f && k < total {
                    // A gap under the column: more beside.
                    s.floor = s.floor.max(k + 1);
                    (k + ((f - c) / average).ceil() as usize).min(total)
                } else if c - average >= f + 1.0 && k > s.floor {
                    // Items beside it that start below its bottom: back down.
                    k.saturating_sub(((c - f) / average).floor() as usize).max(s.floor)
                } else {
                    k
                };
                if std::env::var_os("FEATHER_DEBUG_FLOATS").is_some() {
                    eprintln!("float row {key:?}: float {f:.0}px, beside {k}/{total} = {c:.0}px -> {next} (floor {})", s.floor);
                }
                let changed = next != s.beside;
                s.beside = next;
                changed
            });
            if changed {
                window.request_animation_frame();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_0();
    // Below: up to the next float on this side, then around that one.
    let next_float = below.iter().position(|b| same_side(b));
    let (plain, rest) = match next_float {
        Some(j) => (&below[..j], Some((j, &below[j], &below[j + 1..]))),
        None => (below, None),
    };
    let mut out = div().flex().flex_col().w_full().relative().child(row).children(render_flow_children(plain, cx));
    if let Some((j, next, after)) = rest {
        let nested = (key.0, key.1 + beside + j + 1);
        out = out.child(float_flow(nested, right, next, after, 1, cx));
    }
    out.child(decide).into_any_element()
}

/// Flex/grid items: margins apply as-is.
fn render_items(children: &[LayoutBox], cx: &RenderCx) -> Vec<AnyElement> {
    children
        .iter()
        .map(|child| {
            // Unwrapped text as a flex item can't shrink below its width
            // (its automatic minimum size), so it needs no scroller.
            if let LayoutBox::Paragraph(p) = child
                && !p.style.white_space.wraps()
            {
                return div().flex_none().child(crate::inline::InlineFlow::new(p.clone(), cx)).into_any_element();
            }
            if is_out_of_flow(child) {
                return positioned(child, cx, child.margin());
            }
            in_flow(child, cx, child.margin(), false)
        })
        .collect::<Vec<_>>()
        .pipe(|els| behind_first(els, children))
}

trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}

thread_local! {
    /// The nearest rounded box that clips its content (`overflow: hidden`
    /// with `border-radius`): (corner radius, width, height), px. GPUI clips
    /// to rectangles, so an image filling it rounds its own corners instead
    /// (avatars: a round frame around a square picture).
    static ROUND_CLIP: Cell<Option<(f32, f32, f32)>> = const { Cell::new(None) };
}

/// A rounded, clipping box of fixed size: its radius and size.
fn round_clip(s: &ComputedStyle) -> Option<(f32, f32, f32)> {
    if s.overflow == Overflow::Visible {
        return None;
    }
    let (CssLength::Px(w), CssLength::Px(h)) = (s.width, s.height) else { return None };
    let (w, h) = if s.border_box { (w - s.horizontal_edges(), h - s.vertical_edges()) } else { (w, h) };
    let r = match s.border_radius[0] {
        CssLength::Px(r) => r,
        CssLength::Percent(p) => p / 100.0 * w.min(h),
        _ => return None,
    };
    (r > 0.0).then_some((r, w, h))
}

fn render_block(block: &Block, cx: &RenderCx, margin: Edges<CssLength>, in_flow: bool) -> AnyElement {
    // `filter`: its colors change everything drawn in the box.
    if let Some(filters) = block.style.filter.as_ref().filter(|f| f.iter().any(|f| f.is_color())) {
        let inner = RenderCx { palette: cx.palette.under(filters), ..cx.clone() };
        return render_block_clipped(block, &inner, margin, in_flow);
    }
    render_block_clipped(block, cx, margin, in_flow)
}

fn render_block_clipped(block: &Block, cx: &RenderCx, margin: Edges<CssLength>, in_flow: bool) -> AnyElement {
    if let Some(clip) = round_clip(&block.style) {
        let outer = ROUND_CLIP.replace(Some(clip));
        let el = render_block_unclipped(block, cx, margin, in_flow);
        ROUND_CLIP.set(outer);
        return el;
    }
    render_block_unclipped(block, cx, margin, in_flow)
}

fn render_block_unclipped(block: &Block, cx: &RenderCx, margin: Edges<CssLength>, in_flow: bool) -> AnyElement {
    // `translateX(-130%)` on a box whose size only layout knows (a shimmer
    // stretched by `inset: 0`): shifted once it's laid out.
    let later = later_translate(&block.style);
    if later != (0.0, 0.0) {
        let outer = SHIFT_LATER.replace(true);
        let el = transform::within(block.style.rotate, || render_block_inner(block, cx, margin, in_flow));
        SHIFT_LATER.set(outer);
        return Shifted { child: Some(el), pct: later }.into_any_element();
    }
    // A rotated box rotates the icons inside it (all GPUI can turn).
    let el = transform::within(block.style.rotate, || render_block_inner(block, cx, margin, in_flow));
    // `transform: scale()`: everything in it drawn bigger or smaller about
    // its center (an empty decorated box is drawn scaled already, as SVG).
    match block.style.scale {
        (x, y) if (x, y) != (1.0, 1.0) && transform::box_shape_size(block).is_none() => {
            Scaled { child: el, scale: (x, y), origin: block.style.transform_origin }.into_any_element()
        }
        _ => el,
    }
}

/// A box drawn scaled about its center (CSS `transform: scale()`); it's laid
/// out, and takes the pointer, at its own size.
struct Scaled {
    child: AnyElement,
    scale: (f32, f32),
    /// `transform-origin`, of the box.
    origin: (CssLength, CssLength),
}

impl IntoElement for Scaled {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Scaled {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let at = |l: CssLength, size: Pixels| match l {
            CssLength::Px(v) => px(v),
            CssLength::Percent(p) => size * (p / 100.0),
            CssLength::Auto => size / 2.0,
        };
        let pivot = bounds.origin + point(at(self.origin.0, bounds.size.width), at(self.origin.1, bounds.size.height));
        let child = &mut self.child;
        window.with_paint_scale_xy(pivot, self.scale, |window| child.paint(window, cx));
    }
}

thread_local! {
    /// The block being rendered moves by its percentage translation after
    /// layout ([`Shifted`]); `apply_position` leaves that part out.
    static SHIFT_LATER: Cell<bool> = const { Cell::new(false) };
}

/// The percentages of a translation along axes whose size isn't fixed
/// (fractions of the laid-out width and height).
fn later_translate(s: &ComputedStyle) -> (f32, f32) {
    let unknown = |size: CssLength| !matches!(size, CssLength::Px(_));
    let x = if unknown(s.width) { s.translate.0.pct / 100.0 } else { 0.0 };
    let y = if unknown(s.height) { s.translate.1.pct / 100.0 } else { 0.0 };
    (x, y)
}

/// A box moved after layout by fractions of its own size: what
/// `transform: translate(%)` does.
struct Shifted {
    child: Option<AnyElement>,
    pct: (f32, f32),
}

impl IntoElement for Shifted {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Shifted {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let id = self.child.as_mut().expect("child").request_layout(window, cx);
        (id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let offset = point(bounds.size.width * self.pct.0, bounds.size.height * self.pct.1);
        let child = self.child.as_mut().expect("child");
        window.with_element_offset(offset, |window| child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = &mut self.child {
            child.paint(window, cx);
        }
    }
}

fn render_block_inner(block: &Block, cx: &RenderCx, margin: Edges<CssLength>, in_flow: bool) -> AnyElement {
    // A rotated/scaled box with nothing in it (arrows, notches, spinners)
    // is drawn as a transformed shape instead of a GPUI box.
    // `visibility: hidden`: the box keeps its place but paints nothing.
    let shape = if block.style.visible { transform::box_shape(block, cx) } else { None };
    let stripped;
    let s = match &shape {
        Some((plain, _)) => {
            stripped = plain.clone();
            &stripped
        }
        None if !block.style.visible => {
            stripped = invisible(&block.style);
            &stripped
        }
        None => &*block.style,
    };
    let fill = block.kind != BlockKind::Table && fills_with_auto_margins(s, in_flow);
    let base = apply_box(div(), s, margin, cx)
        .when_some(shape.map(|(_, layers)| layers), |d, layers| d.child(layers))
        .text_color(cx.palette.color(s.color))
        .when(fill, |d| d.w_full());
    // (A flex item's automatic minimum width is its min-content width:
    // taffy asks the text itself, shaped, not an estimate.)
    let scrolls = s.overflow == Overflow::Scroll && (!s.height.is_auto() || !s.max_height.is_auto());
    // Gradients, then a background image / mask icon, painted first
    // (behind the content).
    let base = match crate::gradient::GradientFill::new(s, &cx.palette) {
        Some(fill) if s.position == style::Position::Static => base.relative().child(fill),
        Some(fill) => base.child(fill),
        None => base,
    };
    let base = match image_layer(s, cx) {
        // The layer is positioned inside the box: make it a containing
        // block, unless it's positioned already (`.relative()` would undo that).
        Some(layer) if s.position == style::Position::Static => base.relative().child(layer),
        Some(layer) => base.child(layer),
        None => base,
    };

    let el = match &block.kind {
        BlockKind::Control(control) => base.child(ControlHost {
            control: control.clone(),
            style: block.style.clone(),
            render: cx.controls.clone(),
            child: None,
        }),
        BlockKind::Normal | BlockKind::Table => {
            base.flex().flex_col().children(render_flow_children(&block.children, cx))
        }
        BlockKind::TableRow => base.flex().flex_row().children(render_items(&block.children, cx)),
        BlockKind::TableCell => base
            .flex()
            .flex_col()
            // `vertical-align` places the content in the row's height
            // (middle by default, from the rows; baseline as top).
            .map(|d| match block.style.vertical_align {
                style::VerticalAlign::Middle => d.justify_center(),
                style::VerticalAlign::Bottom | style::VerticalAlign::TextBottom => d.justify_end(),
                _ => d,
            })
            .map(|d| match block.cell {
                // Column-shared sizing (see `layout_table`) so rows line up.
                Some(cell) => d
                    .flex_grow(cell.grow)
                    .flex_shrink(1.0)
                    .min_w(px(cell.min))
                    .map(|d| match cell.percent {
                        Some(p) => d.flex_basis(relative(p / 100.0)).flex_shrink_0(),
                        None => d.flex_basis(px(cell.basis)),
                    }),
                None => d,
            })
            .children(render_flow_children(&block.children, cx)),
        BlockKind::ListItem { marker } => base
            .relative()
            .flex()
            .flex_col()
            // A hidden item's marker is hidden with it.
            .when_some(marker.clone().filter(|_| s.visible), |d, marker| {
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
        BlockKind::Flex if block.float_split.is_some() => float_split_row(block, block.float_split.expect("checked"), base, cx),
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
        BlockKind::Grid if s.taffy_grid => {
            let mut d = base.grid();
            d.style().css_grid = Some(css_grid(s));
            apply_gap(apply_alignment(d, s), s).children(render_items(&block.children, cx))
        }
        BlockKind::Grid => {
            let n = s.grid_columns.unwrap_or(1);
            let d = if s.grid_content_min { base.grid().grid_cols_min_content(n) } else { base.grid().grid_cols(n) };
            apply_gap(apply_alignment(d, s), s).children(render_items(&block.children, cx))
        }
    };

    let el = match cursor_style(s.cursor) {
        Some(c) => el.cursor(c),
        None => el,
    };
    // Scripts measure this one: an invisible child reports where it lands.
    let el = match block.node.filter(|&n| (cx.wants_bounds)(n)) {
        Some(node) => {
            let report = cx.on_bounds.clone();
            el.child(
                canvas(move |bounds, window, cx| report(node, bounds, window, cx), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
        }
        None => el,
    };
    // `pointer-events: none` and hidden boxes aren't under the mouse.
    let hover = block.node.filter(|_| cx.track_hover && !s.pointer_events_none && s.visible);
    if !scrolls && hover.is_none() {
        return el.into_any_element();
    }
    let el = el.id(("block", block.id));
    let el = match (scrolls, block.node) {
        // Scripts read and set where it's scrolled (`scrollTop`).
        (true, Some(node)) => el.overflow_y_scroll().track_scroll(&scrolled::handle(node)),
        (true, None) => el.overflow_y_scroll(),
        _ => el,
    };
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
/// A style that paints nothing (for `visibility: hidden` boxes).
fn invisible(s: &ComputedStyle) -> ComputedStyle {
    let mut s = s.clone();
    s.background = None;
    s.background_image = None;
    s.background_url = None;
    s.gradients = None;
    s.shadows = None;
    s.masked = false;
    for side in style::Side::ALL {
        s.border_color.set(side, style::Color::TRANSPARENT);
    }
    s
}

/// The image's picture: its `src` once that's loaded, until then the one
/// the element showed before.
/// The rounded clipping box being rendered into, if any.
pub(crate) fn current_round_clip() -> Option<(f32, f32, f32)> {
    ROUND_CLIP.get()
}

/// Corners for an image `w`x`h` (px, when known) with style `s`: its own
/// radius, else that of a rounded clipping box it fills.
pub(crate) fn clipped_corners(s: &ComputedStyle, w: Option<f32>, h: Option<f32>, clip: Option<(f32, f32, f32)>, own: [Pixels; 4]) -> [Pixels; 4] {
    // As big as the box or bigger (a 22px picture in an 18px frame): the
    // box's corners cut it.
    let fills = |len: CssLength, clip: f32, size: Option<f32>| {
        matches!(len, CssLength::Percent(p) if p >= 99.0) || size.is_some_and(|v| v >= clip - 1.0)
    };
    match clip {
        Some((r, cw, ch)) if own.iter().all(|c| *c == px(0.)) && fills(s.width, cw, w) && fills(s.height, ch, h) => [px(r); 4],
        _ => own,
    }
}

pub(crate) fn shown_image(i: &ImageBox, cx: &RenderCx) -> Option<ImageState> {
    let state = cx.images.borrow().get(&*i.src).cloned();
    let Some(node) = i.node else { return state };
    if matches!(state, Some(ImageState::Ready { .. })) {
        cx.shown_images.borrow_mut().insert(node, i.src.clone());
        return state;
    }
    let before = cx.shown_images.borrow().get(&node).cloned()?;
    match cx.images.borrow().get(&*before) {
        Some(ready @ ImageState::Ready { .. }) => Some(ready.clone()),
        _ => state,
    }
}

fn render_image(i: &ImageBox, cx: &RenderCx, margin: Edges<CssLength>) -> AnyElement {
    let filtered;
    let cx = match i.style.filter.as_ref().filter(|f| f.iter().any(|f| f.is_color())) {
        Some(filters) => {
            filtered = RenderCx { palette: cx.palette.under(filters), ..cx.clone() };
            &filtered
        }
        None => cx,
    };
    let s = &*i.style;
    let state = shown_image(i, cx);
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
    // (A percentage height against a content-sized box was made `auto` when
    // the boxes were built.)
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
        _ if !s.visible => div().into_any_element(),
        Some(ImageState::Ready { image, padded, frames, width, height }) => {
            // `50%` makes a circle: a share of the image's own size.
            let box_w = match s.width { CssLength::Px(v) => Some(v), _ => natural_width };
            let box_h = match s.height { CssLength::Px(v) => Some(v), _ => natural_height.or(box_w) };
            let corners = s.border_radius.map(|r| match (r, box_w, box_h) {
                (CssLength::Percent(p), Some(w), Some(h)) => px(p / 100.0 * w.min(h)),
                _ => radius(r),
            });
            // Filling a rounded box that clips it: rounded like that box.
            let clip = ROUND_CLIP.get();
            let corners = clipped_corners(s, box_w, box_h, clip, corners);
            // The box's shape (for drawing a rounded `cover` picture).
            let ratio = match (s.width, s.height) {
                (CssLength::Px(w), CssLength::Px(h)) if h > 0.0 => Some(w / h),
                _ => clip.map(|(_, w, h)| w / h.max(1.0)),
            };
            transform::image(i, cx)
                .unwrap_or_else(|| {
                    let (picture, padded) = crate::images::filtered(&image, &padded, &frames, cx.palette.filter.as_deref());
                    crate::images::picture_fitted(picture, &padded, width, height, corners, s.object_fit, ratio)
                })
        }
        // Alt text stands in for a broken image (not one still loading:
        // that's blank, so it doesn't flash).
        Some(ImageState::Failed) if natural_width.is_none() => match &i.alt {
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

    // Scripts listening to it (a <canvas> app): where it is, and hover.
    let d = match i.node.filter(|&n| (cx.wants_bounds)(n)) {
        Some(node) => {
            let report = cx.on_bounds.clone();
            d.relative().child(
                canvas(move |bounds, window, cx| report(node, bounds, window, cx), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
        }
        None => d,
    };
    let d = match i.node.filter(|_| cx.track_hover && !s.pointer_events_none && s.visible && i.link.is_none()) {
        Some(node) => {
            let report = cx.on_block_hover.clone();
            return d
                .id(("img", i.id))
                .on_hover(move |hovered, window, cx| report(node, *hovered, window, cx))
                .child(content)
                .into_any_element();
        }
        None => d,
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
    let lookup = |url: &str| -> Option<(std::sync::Arc<Image>, Option<std::sync::Arc<RenderImage>>, f32, f32)> {
        let url = Url::parse(url).ok()?;
        match cx.images.borrow().get(&url)? {
            ImageState::Ready { image, frames, width, height, .. } => Some((image.clone(), frames.clone(), *width, *height)),
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
    // Pixel offsets (sprite sheets): moved from where alignment put it, and
    // kept at its size rather than squeezed by the box.
    let shifted = |el: Div, pos: BgPos| -> Div {
        if pos.dx == 0.0 && pos.dy == 0.0 {
            return el;
        }
        el.flex_shrink_0().relative().left(px(pos.dx)).top(px(pos.dy))
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
        let (image, _, w, h) = lookup(s.mask_url.as_deref()?)?;
        if image.format != ImageFormat::Svg {
            return None;
        }
        let color = cx.palette.color(s.mask_color()?);
        let path = crate::images::svg_asset_path(&image);
        let (d, size) = layer(s.mask_size, s.mask_position);
        let xf = transform::Xf::of_box(s);
        let icon = shifted(sized(div(), size, (w, h)), s.mask_position).child(
            svg()
                .path(path)
                .size_full()
                .text_color(color)
                .when(!xf.is_identity(), |d| d.with_transformation(xf.gpui())),
        );
        return Some(d.child(icon).into_any_element());
    }
    let (image, frames, w, h) = lookup(s.background_url.as_deref()?)?;
    // Decoded here (animations, AVIF): those frames.
    let image: ImageSource = match frames {
        Some(frames) => frames.into(),
        None => image.into(),
    };
    let (d, size) = layer(s.background_size, s.background_position);
    let fit = match size {
        BgSize::Cover => ObjectFit::Cover,
        BgSize::Contain => ObjectFit::Contain,
        _ => ObjectFit::Fill,
    };
    let picture = shifted(sized(div(), size, (w, h)), s.background_position).child(match &image {
        // An animation needs element state to play (see `picture_fitted`).
        ImageSource::Render(r) if r.frame_count() > 1 => {
            let id = ("animation", r.id.0);
            img(image).size_full().object_fit(fit).id(id).into_any_element()
        }
        _ => img(image).size_full().object_fit(fit).into_any_element(),
    });
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
        Some(Justify::SafeCenter) => {
            let mut d = d;
            d.style().justify_content = Some(gpui_kit::AlignContent::SafeCenter);
            d
        }
        Some(Justify::End) => d.justify_end(),
        Some(Justify::SpaceBetween) => d.justify_between(),
        Some(Justify::SpaceAround) => d.justify_around(),
        Some(Justify::SpaceEvenly) => d.justify_evenly(),
    };
    let d = match s.align_content {
        None => d,
        Some(Justify::Start) => d.content_start(),
        Some(Justify::Center) => d.content_center(),
        Some(Justify::SafeCenter) => {
            let mut d = d;
            d.style().align_content = Some(gpui_kit::AlignContent::SafeCenter);
            d
        }
        Some(Justify::End) => d.content_end(),
        Some(Justify::SpaceBetween) => d.content_between(),
        Some(Justify::SpaceAround) => d.content_around(),
        Some(Justify::SpaceEvenly) => d.content_evenly(),
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
    // A clipping box cuts unwrapped text off (or ends it in "…") instead.
    if p.style.white_space.wraps() || p.style.overflow != Overflow::Visible {
        flow.into_any_element()
    } else {
        // Unwrapped text (pre, nowrap) scrolls sideways instead of
        // overflowing: as wide as its longest line, inside a scroller.
        let handle = sideways::handle(p);
        let claim = handle.clone();
        div()
            .id(("nowrap", p.id))
            .flex()
            .flex_row()
            .overflow_x_scroll()
            // A vertical wheel scrolls the page, not this sideways.
            .restrict_scroll_to_axis()
            .track_scroll(&handle)
            .on_scroll(move |event, _, _| sideways::maybe_claim(&claim, event))
            .child(div().flex_none().min_w_full().child(flow))
            .into_any_element()
    }
}

/// Sideways scrolling inside the page (`<pre>` and nowrap text) wins over
/// Scrolling boxes' positions by element, for scripts (`el.scrollTop`,
/// `el.scrollTo()`): the tab answers and applies them.
pub mod scrolled {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use gpui_kit::{ScrollHandle, point, px};
    use page::NodeId;

    thread_local! {
        static HANDLES: RefCell<HashMap<NodeId, ScrollHandle>> = RefCell::new(HashMap::new());
        /// Positions scripts asked for that the box can't take yet (not
        /// rendered, or its content not laid out): (x, y, frames left).
        static PENDING: RefCell<HashMap<NodeId, (f32, f32, u8)>> = RefCell::new(HashMap::new());
    }

    /// How many frames a script's scroll waits for the box to grow into it.
    const PENDING_FRAMES: u8 = 30;

    /// `node`'s handle for this frame (applying a pending script scroll:
    /// GPUI clamps it to the content each layout).
    pub(crate) fn handle(node: NodeId) -> ScrollHandle {
        let h = HANDLES.with_borrow_mut(|h| h.entry(node).or_default().clone());
        PENDING.with_borrow_mut(|pending| {
            let Some((x, y, left)) = pending.get_mut(&node) else { return };
            let max = h.max_offset();
            if (f32::from(max.x) >= *x && f32::from(max.y) >= *y) || *left == 0 {
                pending.remove(&node);
                return;
            }
            *left -= 1;
            h.set_offset(point(-px(*x), -px(*y)));
        });
        h
    }

    /// A new page: the old one's positions go.
    pub fn forget() {
        HANDLES.with_borrow_mut(HashMap::clear);
        PENDING.with_borrow_mut(HashMap::clear);
    }

    /// Scroll `node`'s box to (x, y) px (clamped to what it has; kept for
    /// a few frames when it has less, as before its first layout): whether
    /// the tab should redraw.
    pub fn scroll_to(node: NodeId, x: f32, y: f32) -> bool {
        let h = HANDLES.with_borrow(|h| h.get(&node).cloned());
        let max = h.as_ref().map(|h| h.max_offset());
        let fits = max.is_some_and(|m| f32::from(m.x) >= x && f32::from(m.y) >= y);
        PENDING.with_borrow_mut(|p| {
            if fits { p.remove(&node) } else { p.insert(node, (x, y, PENDING_FRAMES)) }
        });
        if let (Some(h), Some(max)) = (h, max) {
            h.set_offset(point(-px(x.clamp(0.0, f32::from(max.x).max(0.0))), -px(y.clamp(0.0, f32::from(max.y).max(0.0)))));
        }
        true
    }

    /// `node`'s box: (scrollLeft, scrollTop, scrollWidth, scrollHeight).
    pub fn info(node: NodeId) -> Option<[f32; 4]> {
        let h = HANDLES.with_borrow(|h| h.get(&node).cloned())?;
        let (offset, max, bounds) = (h.offset(), h.max_offset(), h.bounds());
        Some([
            f32::from(-offset.x),
            f32::from(-offset.y),
            f32::from(bounds.size.width + max.x),
            f32::from(bounds.size.height + max.y),
        ])
    }
}

/// the shell's swipe-to-go-back while there's room to scroll that way.
pub mod sideways {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::sync::Arc;

    use gpui_kit::{ScrollEvent, ScrollHandle, px};

    use crate::boxes::Paragraph;

    thread_local! {
        /// Scroll positions per paragraph (by address: a new layout starts
        /// over at the left).
        static HANDLES: RefCell<HashMap<usize, ScrollHandle>> = RefCell::new(HashMap::new());
        static CLAIMED: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn handle(p: &Arc<Paragraph>) -> ScrollHandle {
        HANDLES.with_borrow_mut(|h| {
            if h.len() > 2000 {
                h.clear();
            }
            h.entry(Arc::as_ptr(p) as usize).or_default().clone()
        })
    }

    pub(crate) fn maybe_claim(handle: &ScrollHandle, event: &ScrollEvent) {
        let d = event.delta.pixel_delta(px(20.));
        if d.x.abs() <= d.y.abs() {
            return;
        }
        let (at, max) = (-handle.offset().x, handle.max_offset().x);
        // The element's own scrolling may have run already: where it was
        // before this event counts too. Positive x moves towards the start.
        let before = at + d.x;
        let room = if d.x > px(0.) { at > px(0.) || before > px(0.) } else { at < max || before < max };
        if room {
            CLAIMED.set(true);
        }
    }

    /// Whether the scroll event being dispatched was taken by page content
    /// (and clears it for the next one).
    pub fn take_claim() -> bool {
        CLAIMED.replace(false)
    }
}

/// Asks the shell for a control's widget while laying out (when it can
/// create the widget's state), then is that widget.
struct ControlHost {
    control: Arc<Control>,
    style: Arc<ComputedStyle>,
    render: ControlRenderer,
    child: Option<AnyElement>,
}

impl IntoElement for ControlHost {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for ControlHost {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut child = (self.render)(&self.control, &self.style, window, cx);
        let id = child.request_layout(window, cx);
        self.child = Some(child);
        (id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = &mut self.child {
            child.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = &mut self.child {
            child.paint(window, cx);
        }
    }
}

/// Like GPUI's `deferred()` (drawn after everything else, so positioned boxes
/// land on top of later rows), but clipped to where it was placed: the page.
/// Plain `deferred` draws unclipped, over the browser's own tab strip.
/// An out-of-flow box behind its siblings (negative `z-index`): painted in
/// tree order, not deferred above everything.
fn behind(b: &LayoutBox) -> bool {
    matches!(b, LayoutBox::Block(block) if block.style.z_index.is_some_and(|z| z < 0))
}

thread_local! {
    /// The deferred stacking context the box being built is in: its draw
    /// priority and how many contexts deep it is (see `deferred`).
    static STACKING: std::cell::Cell<(usize, u32)> = const { std::cell::Cell::new((0, 0)) };
    /// How many deferred boxes, of any kind, it's inside.
    static DEFER_LEVELS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// A positioned box: drawn after the in-flow content, through GPUI's
/// deferred layer. Only the outermost one is deferred; ones inside it are
/// drawn in place, in the z-order of their siblings (a deferral inside a
/// deferral would land above every other positioned box on the page).
fn positioned(b: &LayoutBox, cx: &RenderCx, margin: Edges<CssLength>) -> AnyElement {
    if behind(b) {
        return render_box(b, cx, margin, false);
    }
    // With `z-index: auto` it's no stacking context: what's inside with a
    // z-index of its own is ordered with the page (NASA's hero text, z-index
    // 400 in a z-auto box, over its z-index 200 shade).
    deferred(b, stacking_context(b), || render_box(b, cx, margin, false)).unwrap_or_else(|| render_box(b, cx, margin, false))
}

struct ClippedDeferred {
    child: Option<AnyElement>,
    priority: usize,
}

impl IntoElement for ClippedDeferred {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for ClippedDeferred {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let id = self.child.as_mut().expect("child").request_layout(window, cx);
        (id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) {
        let child = self.child.take().expect("child");
        let offset = window.element_offset();
        let mask = window.content_mask();
        window.defer_draw(child, offset, self.priority, Some(mask));
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}

/// CSS rotations and scales. GPUI can transform only monochrome SVGs, so
/// that's what transformed things become: mask icons, single-color inline
/// SVGs, and empty decorated boxes (drawn as SVG shapes). Text and photos
/// stay as they are.
pub(crate) mod transform {
    use std::cell::Cell;

    use gpui_kit::*;
    use style::{ComputedStyle, Length as CssLength, Side};

    use crate::RenderCx;
    use crate::boxes::{Block, BlockKind, ImageBox};

    thread_local! {
        /// Rotation of the boxes being rendered around this one (degrees).
        static INHERITED: Cell<f32> = const { Cell::new(0.0) };
    }

    /// Render `f` inside a box rotated by `deg`.
    pub(crate) fn within<R>(deg: f32, f: impl FnOnce() -> R) -> R {
        if deg == 0.0 {
            return f();
        }
        let outer = INHERITED.get();
        INHERITED.set(outer + deg);
        let out = f();
        INHERITED.set(outer);
        out
    }

    /// Run `f` with the surrounding rotation set to `deg` (for elements that
    /// draw later than they're built).
    pub(crate) fn at<R>(deg: f32, f: impl FnOnce() -> R) -> R {
        let outer = INHERITED.replace(deg);
        let out = f();
        INHERITED.set(outer);
        out
    }

    pub(crate) fn inherited() -> f32 {
        INHERITED.get()
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(crate) struct Xf {
        /// Degrees, clockwise.
        pub rotate: f32,
        pub scale: (f32, f32),
    }

    impl Xf {
        /// What applies to drawing `s`'s own content: its transform plus
        /// the rotations of the boxes around it.
        pub(crate) fn of(s: &ComputedStyle) -> Xf {
            Xf { rotate: s.rotate + inherited(), scale: s.scale }
        }

        /// For a box's own layers, drawn inside `within` its rotation
        /// (which the surrounding rotation already includes).
        pub(crate) fn of_box(s: &ComputedStyle) -> Xf {
            Xf { rotate: inherited(), scale: s.scale }
        }

        pub(crate) fn is_identity(self) -> bool {
            self.rotate.rem_euclid(360.0) == 0.0 && self.scale == (1.0, 1.0)
        }

        pub(crate) fn gpui(self) -> Transformation {
            Transformation::rotate(radians(self.rotate.to_radians())).with_scaling(size(self.scale.0, self.scale.1))
        }
    }

    /// A transformed single-color inline SVG, drawn as one (else `None`:
    /// draw the picture as usual).
    pub(crate) fn image(i: &ImageBox, cx: &RenderCx) -> Option<AnyElement> {
        let xf = Xf::of(&i.style);
        let mono = i.mono.as_ref().filter(|_| !xf.is_identity())?;
        let color = cx.palette.color(mono.color.unwrap_or(i.style.color));
        Some(svg().data(&mono.markup).size_full().text_color(color).with_transformation(xf.gpui()).into_any_element())
    }

    /// The border box size `box_shape` draws `block` at, when it does.
    pub(crate) fn box_shape_size(block: &Block) -> Option<(f32, f32)> {
        let s = &*block.style;
        if !block.children.is_empty() || !matches!(block.kind, BlockKind::Normal) {
            return None;
        }
        let xf = Xf::of_box(s);
        if xf.is_identity() || s.masked || s.background_url.is_some() {
            return None;
        }
        let bw = s.border_width;
        let has_border = Side::ALL.iter().any(|&side| bw.get(side) > 0.0);
        let fill = s.background.or(s.background_image);
        if fill.is_none() && !has_border {
            return None;
        }
        // The border box's size (only fixed sizes).
        // An empty box that shrinks to fit (inline-block, floated, out of
        // flow) is as wide as nothing when its width is `auto`: GitHub's
        // breadcrumb slash is a rotated right border on one.
        let shrinks = !s.display.is_block_level()
            || s.float != style::Float::None
            || matches!(s.position, style::Position::Absolute | style::Position::Fixed);
        let w = match s.width {
            CssLength::Px(w) => w,
            CssLength::Auto if shrinks => 0.0,
            _ => return None,
        };
        let CssLength::Px(h) = s.height else { return None };
        let (w, h) = if s.border_box { (w, h) } else { (w + s.horizontal_edges(), h + s.vertical_edges()) };
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        Some((w, h))
    }

    /// An empty box with a background or border and a transform: its style
    /// without the decoration (so the box keeps its size and place), and the
    /// decoration as transformed SVG layers covering its border box.
    pub(crate) fn box_shape(block: &Block, cx: &RenderCx) -> Option<(ComputedStyle, AnyElement)> {
        let (w, h) = box_shape_size(block)?;
        let s = &*block.style;
        let xf = Xf::of_box(s);
        let bw = s.border_width;
        let has_border = Side::ALL.iter().any(|&side| bw.get(side) > 0.0);
        let fill = s.background.or(s.background_image);
        let radii: [(f32, f32); 4] = std::array::from_fn(|i| match s.border_radius[i] {
            CssLength::Px(r) => (r, r),
            CssLength::Percent(p) => (w * p / 100.0, h * p / 100.0),
            _ => (0.0, 0.0),
        });
        let (t, r, b, l) = (bw.top, bw.right, bw.bottom, bw.left);
        let outer = rounded_rect(0.0, 0.0, w, h, radii);
        let mut layers: Vec<AnyElement> = Vec::new();
        let layer = |body: String, color: style::Color| {
            let markup = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">{body}</svg>"#);
            svg()
                .data(markup.as_bytes())
                .absolute()
                .top(px(-t))
                .left(px(-l))
                .w(px(w))
                .h(px(h))
                .text_color(cx.palette.color(color))
                .with_transformation(xf.gpui())
                .into_any_element()
        };
        if let Some(fill) = fill {
            layers.push(layer(format!(r#"<path d="{outer}"/>"#), fill));
        }
        if has_border {
            // The ring between the outer and inner edges; each side's color
            // owns the wedge from the box's center (how corners are split).
            let inner_radii: [(f32, f32); 4] = [
                ((radii[0].0 - l).max(0.0), (radii[0].1 - t).max(0.0)),
                ((radii[1].0 - r).max(0.0), (radii[1].1 - t).max(0.0)),
                ((radii[2].0 - r).max(0.0), (radii[2].1 - b).max(0.0)),
                ((radii[3].0 - l).max(0.0), (radii[3].1 - b).max(0.0)),
            ];
            let inner = rounded_rect(l, t, (w - l - r).max(0.0), (h - t - b).max(0.0), inner_radii);
            let (cx_, cy) = (w / 2.0, h / 2.0);
            let wedge = |side: Side| match side {
                Side::Top => format!("M0 0L{w} 0L{cx_} {cy}Z"),
                Side::Right => format!("M{w} 0L{w} {h}L{cx_} {cy}Z"),
                Side::Bottom => format!("M{w} {h}L0 {h}L{cx_} {cy}Z"),
                Side::Left => format!("M0 {h}L0 0L{cx_} {cy}Z"),
            };
            let mut colors: Vec<(style::Color, Vec<Side>)> = Vec::new();
            for side in Side::ALL {
                if bw.get(side) <= 0.0 {
                    continue;
                }
                let c = s.border_color.get(side);
                match colors.iter_mut().find(|(k, _)| *k == c) {
                    Some((_, sides)) => sides.push(side),
                    None => colors.push((c, vec![side])),
                }
            }
            for (i, (color, sides)) in colors.into_iter().enumerate() {
                let clip: String = sides.iter().map(|&side| wedge(side)).collect();
                layers.push(layer(
                    format!(
                        r#"<clipPath id="c{i}"><path d="{clip}"/></clipPath><path fill-rule="evenodd" clip-path="url(#c{i})" d="{outer}{inner}"/>"#
                    ),
                    color,
                ));
            }
        }
        let mut plain = s.clone();
        plain.background = None;
        plain.background_image = None;
        for side in Side::ALL {
            plain.border_color.set(side, style::Color::TRANSPARENT);
        }
        Some((plain, div().children(layers).into_any_element()))
    }

    /// An SVG path for a rectangle with elliptical corner radii (top-left,
    /// top-right, bottom-right, bottom-left).
    fn rounded_rect(x: f32, y: f32, w: f32, h: f32, radii: [(f32, f32); 4]) -> String {
        // Corners that don't fit are scaled down together, like CSS.
        let f = [w / (radii[0].0 + radii[1].0), w / (radii[3].0 + radii[2].0), h / (radii[0].1 + radii[3].1), h / (radii[1].1 + radii[2].1)]
            .into_iter()
            .filter(|f| f.is_finite())
            .fold(1.0f32, f32::min);
        let r = radii.map(|(a, b)| (a * f, b * f));
        let ((a0, b0), (a1, b1), (a2, b2), (a3, b3)) = (r[0], r[1], r[2], r[3]);
        format!(
            "M{x0} {y}H{x1}A{a1} {b1} 0 0 1 {xr} {y1}V{y2}A{a2} {b2} 0 0 1 {x2} {yb}H{x3}A{a3} {b3} 0 0 1 {x} {y3}V{y0}A{a0} {b0} 0 0 1 {x0} {y}Z",
            x0 = x + a0,
            x1 = x + w - a1,
            xr = x + w,
            y1 = y + b1,
            y2 = y + h - b2,
            x2 = x + w - a2,
            yb = y + h,
            x3 = x + a3,
            y3 = y + h - b3,
            y0 = y + b0,
        )
    }
}
