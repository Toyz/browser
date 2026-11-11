//! Inline layout: our own GPUI element for a paragraph's content.
//!
//! GPUI's `StyledText` lays out one font size per block of text, with nothing
//! but text in it. Web inline content mixes sizes (`<sup>`, `<small>`), raises
//! and lowers baselines (`vertical-align`), draws boxes behind spans (`<code>`
//! chips, `<mark>`), and puts images and inline-blocks (buttons, badges) in
//! the line. So this element:
//!
//! 1. measures each piece (word / space / object) with GPUI's text shaper
//!    (results are cached frame to frame by GPUI's line layout cache),
//! 2. breaks lines greedily at the allowed break points, sizing each line from
//!    the ascent/descent/line-height of everything on it (CSS's line box model),
//! 3. paints inline-box decorations, then text grouped into runs, then
//!    inline-blocks (recursively: their content is a nested flow), then images,
//! 4. hit-tests links itself (hand cursor, click, hover).
//!
//! It answers min-content (longest word) and max-content (unwrapped) queries
//! for real, so flex items and table cells size like they do in CSS.
//!
//! Inline-blocks can't be GPUI subtrees here: GPUI's layout engine isn't
//! available re-entrantly inside a measure callback. So only inline-blocks
//! with inline content are atomic objects (laid out by this same engine);
//! the box builder turns the rest into block boxes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use style::{LineHeight, TextAlign as CssTextAlign, VerticalAlign};
use url::Url;

use crate::boxes::{AtomicBox, ImageBox, InlineObject, InlineStyle, Paragraph, PieceKind};
use crate::images::ImageState;
use crate::view::{RenderCx, cursor_style, text_run};

pub struct InlineFlow {
    id: ElementId,
    para: Arc<Paragraph>,
    cx: RenderCx,
    state: Rc<RefCell<FlowState>>,
    /// Inline-blocks containing blocks: real GPUI subtrees, laid out as
    /// absolutely positioned children, then moved into their line slot.
    blocks: Vec<(u32, AnyElement, LayoutId)>,
    /// The text's own layout node (when there are block children).
    leaf: Option<LayoutId>,
    /// Rotation of the boxes around this text (for its icons).
    rotation: f32,
}

impl InlineFlow {
    pub fn new(para: Arc<Paragraph>, cx: &RenderCx) -> Self {
        Self {
            id: ElementId::NamedInteger("inline".into(), stable_key(&para)),
            para,
            cx: RenderCx { round_clip: crate::view::current_round_clip(), stacking: crate::view::current_stacking(), ..cx.clone() },
            state: Rc::default(),
            blocks: Vec::new(),
            leaf: None,
            rotation: crate::view::transform::inherited(),
        }
    }
}

/// What identifies a paragraph from one layout to the next: its first DOM
/// node (and how many inline objects it has), not its id in this build,
/// which shifts whenever the DOM before it changes. What it learned about
/// its inline-blocks (their sizes, kept in its element state) then carries
/// over to the next layout: keyed by the build's id, every new layout lost
/// it and its first frame was laid out with guesses, then corrected (pills
/// and icons jumping on each DOM update, YouTube's controls blinking).
fn stable_key(para: &Paragraph) -> u64 {
    use std::hash::{Hash, Hasher};
    let object_node = |o: &InlineObject| match o {
        InlineObject::Block(b) => b.node,
        InlineObject::BlockBox(b) => b.node,
        InlineObject::Image(i) => i.node,
    };
    // The inline-blocks' own elements (the sizes are theirs); else its
    // first text's element.
    let nodes: Vec<_> = para.objects.iter().filter_map(object_node).collect();
    let node = para.spans.iter().find_map(|s| s.style.node);
    if nodes.is_empty() && node.is_none() {
        return para.id as u64;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if nodes.is_empty() { node.hash(&mut h) } else { nodes.hash(&mut h) }
    para.objects.len().hash(&mut h);
    h.finish() | 1 << 63
}

/// Sizes inline-block subtrees got last frame (they can't be laid out from
/// inside text measurement, so lines are broken with these and corrected).
/// Also where the first line of text sits in each (for baseline alignment).
type BlockSizes = Rc<RefCell<HashMap<u32, BlockSize>>>;

#[derive(Clone, Copy, PartialEq)]
struct BlockSize {
    width: Pixels,
    height: Pixels,
    /// From the top of the box to its first baseline, if it has text.
    baseline: Option<Pixels>,
}

thread_local! {
    /// What each paragraph learned about its inline-blocks' sizes (see
    /// `stable_key`): outside GPUI's element state, whose keys include the
    /// ancestors' ids in this build.
    static LEARNED: RefCell<HashMap<u64, BlockSizes>> = RefCell::default();
    /// Where each paragraph's first baseline was placed last (window y), so
    /// an inline-block can find its own baseline after its subtree is laid out.
    static FIRST_BASELINES: RefCell<HashMap<usize, Pixels>> = RefCell::default();
}

/// The paragraph whose first line is a block's baseline: its first in-flow
/// text, looking inside nested blocks.
fn first_paragraph(block: &crate::boxes::Block) -> Option<usize> {
    use crate::boxes::LayoutBox;
    block.children.iter().find_map(|child| match child {
        LayoutBox::Paragraph(p) if !p.text.trim().is_empty() => Some(p.id),
        LayoutBox::Block(b) if !crate::view::is_out_of_flow(child) => first_paragraph(b),
        _ => None,
    })
}

/// Vertical extent of something on a line, relative to the baseline.
#[derive(Clone, Copy, Default)]
struct Extent {
    above: Pixels,
    below: Pixels,
}

impl Extent {
    fn max(self, o: Extent) -> Extent {
        Extent { above: self.above.max(o.above), below: self.below.max(o.below) }
    }

    fn height(self) -> Pixels {
        self.above + self.below
    }
}

#[derive(Clone, Copy)]
struct Measured {
    width: Pixels,
    extent: Extent,
    /// Text: the font's ascent/descent. Objects: the box height / 0.
    ascent: Pixels,
    descent: Pixels,
    /// Objects: the top margin, above the box but inside the line.
    margin_top: Pixels,
}

struct Line {
    /// (piece index, x offset)
    items: Vec<(usize, Pixels)>,
    /// Width without trailing spaces.
    width: Pixels,
    extent: Extent,
    /// The last line `line-clamp` shows, with more text cut after it.
    clamped: bool,
}

/// (wrap width, lines, wrap-adjusted piece measurements)
type LaidOut = (Option<Pixels>, Rc<Vec<Line>>, Rc<Vec<Measured>>);

/// Layout state for one paragraph (and, nested, its inline-blocks' content).
#[derive(Default)]
struct FlowState {
    measured: Option<Rc<Vec<Measured>>>,
    strut: Extent,
    /// Lines for the last wrap width asked for, with piece measurements
    /// adjusted for it (inline-blocks shrink to fit the available width).
    lines: Option<LaidOut>,
    /// Inline-block content flows, by object index, with their content width.
    nested: HashMap<u32, (Rc<RefCell<FlowState>>, Pixels)>,
    block_sizes: BlockSizes,
    /// Shaped text runs from painting, keyed by (wrap width, run start byte).
    /// Text shaping is the expensive part of a frame; lines don't change
    /// between frames unless the layout does.
    shaped: HashMap<(Option<Pixels>, usize), Rc<ShapedLine>>,
    /// Frames in a row its inline-blocks' sizes changed (see `prepaint`).
    relayouts: u8,
}

/// An inline layout kept across frames (in GPUI element state): valid while
/// it's for the same paragraph and no image has loaded since.
type FlowCache = (Arc<Paragraph>, u64, Rc<RefCell<FlowState>>);

/// A shaped span for measuring: its layout and each glyph's (byte index, x).
type SpanShape = (Arc<LineLayout>, Rc<[(usize, Pixels)]>);

/// Text shaping results shared by every paragraph, page and tab, keyed by
/// content rather than by element. A site's header, nav and footer shape
/// once and are reused on its next page; a restyle that only changes colors
/// (hover) re-measures for free. GPUI's own line cache only lives a frame.
mod shapes {
    use super::*;
    use std::hash::{Hash, Hasher};

    /// Clear everything past this much cached text (crude, but a page's worth
    /// of text is a few hundred KB).
    const MAX_BYTES: usize = 8 << 20;

    #[derive(PartialEq, Eq, Hash)]
    struct MeasureKey {
        text: SharedString,
        font: Font,
        size: u32,
    }

    /// Painting also depends on color and decorations.
    #[derive(PartialEq, Eq)]
    struct PaintKey {
        measure: MeasureKey,
        color: [u32; 4],
        background: Option<[u32; 4]>,
        underline: bool,
        strikethrough: bool,
    }

    impl Hash for PaintKey {
        fn hash<H: Hasher>(&self, h: &mut H) {
            self.measure.hash(h);
            self.color.hash(h);
            self.background.hash(h);
            (self.underline, self.strikethrough).hash(h);
        }
    }

    fn bits(c: Hsla) -> [u32; 4] {
        [c.h.to_bits(), c.s.to_bits(), c.l.to_bits(), c.a.to_bits()]
    }

    #[derive(Default)]
    struct Caches {
        measure: HashMap<MeasureKey, SpanShape>,
        paint: HashMap<PaintKey, Rc<ShapedLine>>,
        bytes: usize,
    }

    thread_local! {
        static CACHES: RefCell<Caches> = RefCell::default();
    }

    fn charge(c: &mut Caches, len: usize) {
        c.bytes += len;
        if c.bytes > MAX_BYTES {
            c.measure.clear();
            c.paint.clear();
            c.bytes = len;
        }
    }

    pub(super) fn measure(ts: &WindowTextSystem, text: SharedString, size: Pixels, run: &TextRun) -> SpanShape {
        let key = MeasureKey { text, font: run.font.clone(), size: f32::from(size).to_bits() };
        if let Some(hit) = CACHES.with_borrow(|c| c.measure.get(&key).cloned()) {
            return hit;
        }
        let layout = ts.layout_line(&key.text, size, std::slice::from_ref(run), None);
        let mut xs: Vec<(usize, Pixels)> =
            layout.runs.iter().flat_map(|run| run.glyphs.iter().map(|g| (g.index, g.position.x))).collect();
        xs.sort_by_key(|&(i, _)| i);
        let shape: SpanShape = (layout, xs.into());
        CACHES.with_borrow_mut(|c| {
            charge(c, key.text.len());
            c.measure.insert(key, shape.clone());
        });
        shape
    }

    pub(super) fn paint(ts: &WindowTextSystem, text: SharedString, size: Pixels, run: &TextRun) -> Rc<ShapedLine> {
        let key = PaintKey {
            measure: MeasureKey { text, font: run.font.clone(), size: f32::from(size).to_bits() },
            color: bits(run.color),
            background: run.background_color.map(bits),
            underline: run.underline.is_some(),
            strikethrough: run.strikethrough.is_some(),
        };
        if let Some(hit) = CACHES.with_borrow(|c| c.paint.get(&key).cloned()) {
            return hit;
        }
        // Behind an Rc: a ShapedLine is big (inline decoration runs), and
        // copying it per run per frame showed up in profiles.
        let line = Rc::new(ts.shape_line(key.measure.text.clone(), size, std::slice::from_ref(run), None));
        CACHES.with_borrow_mut(|c| {
            charge(c, key.measure.text.len());
            c.paint.insert(key, line.clone());
        });
        line
    }
}

/// CSS line-height around a glyph box: half the leading above, half below.
fn extent_for(style: &InlineStyle, ascent: Pixels, descent: Pixels) -> Extent {
    let content = ascent + descent;
    let line_height = match style.line_height {
        LineHeight::Normal => content,
        LineHeight::Number(n) => px(n * style.font_size),
        LineHeight::Px(v) => px(v),
    };
    let half_leading = (line_height - content) / 2.0;
    let shift = px(style.baseline_shift);
    Extent {
        above: ascent + half_leading + shift,
        below: descent + half_leading - shift,
    }
}

/// An inline image's size, CSS px: from CSS, its attributes, its picture.
/// `avail`: the line width, for percentage widths (`None`: not known yet;
/// they count as `auto`).
fn image_size(image: &ImageBox, cx: &RenderCx, avail: Option<f32>) -> (f32, f32) {
    let s = &image.style;
    let intrinsic = match crate::view::shown_image(image, cx) {
        Some(ImageState::Ready { width, height, .. }) => Some((width / image.density, height / image.density)),
        _ => None,
    };
    let ratio = match (image.attr_width, image.attr_height) {
        (Some(w), Some(h)) if h > 0.0 => Some(w / h),
        _ => intrinsic.map(|(w, h)| w / h.max(0.01)),
    };
    let css = |l: style::Length| match l {
        style::Length::Px(v) => Some(v),
        _ => None,
    };
    let horizontal = |s: &style::ComputedStyle| s.padding.left.px_or_zero() + s.padding.right.px_or_zero() + s.border_width.left + s.border_width.right;
    let of_avail = |l: style::Length| match (l, avail) {
        (style::Length::Percent(p), Some(a)) => Some((a * p / 100.0 - if s.border_box { horizontal(s) } else { 0.0 }).max(0.0)),
        _ => None,
    };
    let (cw, ch) = (css(s.width).or(of_avail(s.width)), css(s.height));
    let w = cw.or(image.attr_width).or(intrinsic.map(|(w, _)| w));
    let h = ch.or(image.attr_height).or(intrinsic.map(|(_, h)| h));
    let (w, h) = match (w, h, ratio) {
        // Only one dimension set in CSS: the other follows the aspect ratio.
        (Some(w), Some(_), Some(r)) if cw.is_some() && ch.is_none() => (w, w / r),
        (Some(_), Some(h), Some(r)) if ch.is_some() && cw.is_none() => (h * r, h),
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some(r)) => (w, w / r),
        (None, Some(h), Some(r)) => (h * r, h),
        _ => (0.0, 0.0),
    };
    // `max-width: 100%` (against the line): scaled down, keeping the ratio
    // unless the height was set too.
    match css(s.max_width).or(of_avail(s.max_width)) {
        Some(max) if w > max => (max, if ch.is_some() || w <= 0.0 { h } else { h * max / w }),
        _ => (w, h),
    }
}

/// Baseline-relative extent of an inline object `h` tall, per `vertical-align`.
/// `top`/`bottom` align with the line's strut (the block's own font box),
/// which is right whenever the object is the tallest thing on the line.
fn object_extent(valign: VerticalAlign, h: Pixels, strut: Extent, font_size: f32, shift: Pixels) -> Extent {
    let above = match valign {
        VerticalAlign::Middle => h / 2.0 + px(font_size * 0.3),
        VerticalAlign::Top | VerticalAlign::TextTop => strut.above,
        VerticalAlign::Bottom | VerticalAlign::TextBottom => h - strut.below,
        _ => h,
    } + shift;
    Extent { above, below: h - above }
}

/// The parts of `shadow` outside `inner`: up to four rectangles.
fn outside(shadow: Bounds<Pixels>, inner: Bounds<Pixels>) -> Vec<Bounds<Pixels>> {
    let (sl, st, sr, sb) = (shadow.left(), shadow.top(), shadow.right(), shadow.bottom());
    let (il, it, ir, ib) = (inner.left(), inner.top(), inner.right(), inner.bottom());
    let rect = |l: Pixels, t: Pixels, r: Pixels, b: Pixels| (r > l && b > t).then(|| Bounds::new(point(l, t), size(r - l, b - t)));
    let (mt, mb) = (st.max(it), sb.min(ib));
    [
        rect(sl, st, sr, sb.min(it)),
        rect(sl, st.max(ib), sr, sb),
        rect(sl, mt, sr.min(il), mb),
        rect(sl.max(ir), mt, sr, mb),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Padding + border on each side of an inline-block, px.
fn edges(s: &style::ComputedStyle) -> (Pixels, Pixels, Pixels, Pixels) {
    let p = |l: style::Length| l.px_or_zero();
    (
        px(p(s.padding.top) + s.border_width.top),
        px(p(s.padding.right) + s.border_width.right),
        px(p(s.padding.bottom) + s.border_width.bottom),
        px(p(s.padding.left) + s.border_width.left),
    )
}

impl FlowState {
    fn nested(&mut self, ix: u32) -> Rc<RefCell<FlowState>> {
        self.nested.entry(ix).or_insert_with(|| (Rc::default(), px(0.))).0.clone()
    }

    fn measure(&mut self, para: &Paragraph, cx: &RenderCx, window: &mut Window) -> Rc<Vec<Measured>> {
        if let Some(m) = &self.measured {
            return m.clone();
        }
        let ts = window.text_system().clone();

        // The strut: every line is at least as tall as the block's own font.
        let block = InlineStyle::of(&para.style, false);
        // Same measuring path as the text itself, so the metrics agree (mixing
        // sources inflates every line by their difference).
        let strut_style = InlineStyle { baseline_shift: 0.0, ..block.clone() };
        let strut = ts.layout_line(" ", px(block.font_size), &[text_run(1, &strut_style, &cx.palette)], None);
        self.strut = extent_for(&strut_style, strut.ascent, strut.descent);

        // Shape each span once; a piece's width is the distance between its
        // glyph positions. (Shaping word by word was most of a frame's cost.)
        let mut span_starts = Vec::with_capacity(para.spans.len());
        let mut offset = 0;
        for span in &para.spans {
            span_starts.push(offset);
            offset += span.len;
        }
        let mut shaped_spans: HashMap<u32, SpanShape> = HashMap::new();
        let mut span_layout = |span_ix: u32| {
            shaped_spans
                .entry(span_ix)
                .or_insert_with(|| {
                    let start = span_starts[span_ix as usize];
                    let span = &para.spans[span_ix as usize];
                    // Newlines can't be shaped; a space keeps every byte offset.
                    let text: SharedString = para.text[start..start + span.len].replace('\n', " ").into();
                    let run = text_run(text.len(), &span.style, &cx.palette);
                    shapes::measure(&ts, text, px(span.style.font_size), &run)
                })
                .clone()
        };
        let x_at = |layout: &LineLayout, xs: &[(usize, Pixels)], index: usize| -> Pixels {
            match xs.binary_search_by_key(&index, |&(i, _)| i) {
                Ok(i) => xs[i].1,
                Err(i) if i < xs.len() => xs[i].1,
                Err(_) => layout.width,
            }
        };

        let mut out = Vec::with_capacity(para.pieces.len());
        for piece in &para.pieces {
            let style = &para.spans[piece.span as usize].style;
            let shift = px(style.baseline_shift);
            let m = match piece.kind {
                PieceKind::Object(i) => match &para.objects[i as usize] {
                    InlineObject::Image(image) => self.measure_image(image, shift, block.font_size, None, cx),
                    InlineObject::Block(atomic) => self.measure_atomic(i, atomic, shift, None, cx, window),
                    InlineObject::BlockBox(block) => {
                        let s = &block.style;
                        let (ml, mr) = atomic_margins(s);
                        let known = self.block_sizes.borrow().get(&i).copied();
                        let (w, h) = known.map_or_else(
                            // First frame: a guess, corrected next frame.
                            || (px(block.max_content), px(block.style.font_size * 1.4 + s.vertical_edges())),
                            |k| (k.width, k.height),
                        );
                        // The line holds the margin box; baseline-aligned
                        // boxes sit on their first line of text.
                        let (mt, mb) = vertical_margins(s);
                        let baseline = known
                            .and_then(|k| k.baseline)
                            .filter(|_| s.overflow == style::Overflow::Visible)
                            .filter(|_| {
                                matches!(
                                    s.vertical_align,
                                    VerticalAlign::Baseline | VerticalAlign::Sub | VerticalAlign::Super | VerticalAlign::Shift(_)
                                )
                            });
                        let extent = match baseline {
                            Some(b) => {
                                let above = mt + b + shift;
                                Extent { above, below: h + mt + mb - above }
                            }
                            None => object_extent(s.vertical_align, h + mt + mb, self.strut, s.font_size, shift),
                        };
                        Measured { width: w + ml + mr, extent, ascent: h, descent: px(0.), margin_top: mt }
                    }
                },
                kind => {
                    let (layout, xs) = span_layout(piece.span);
                    let start = span_starts[piece.span as usize];
                    let width = if kind == PieceKind::Newline {
                        px(0.)
                    } else {
                        x_at(&layout, &xs, piece.range.end - start) - x_at(&layout, &xs, piece.range.start - start)
                    };
                    Measured {
                        width,
                        extent: extent_for(style, layout.ascent, layout.descent),
                        ascent: layout.ascent,
                        descent: layout.descent,
                        margin_top: px(0.),
                    }
                }
            };
            out.push(m);
        }
        let out = Rc::new(out);
        self.measured = Some(out.clone());
        out
    }

    /// An inline-block: shrink-to-fit (or its CSS size) around a nested flow,
    /// sitting on the baseline of its first line.
    /// An image's place on the line (`avail`: the line width, for
    /// percentage sizes).
    fn measure_image(&self, image: &ImageBox, shift: Pixels, font_size: f32, avail: Option<Pixels>, cx: &RenderCx) -> Measured {
        let (w, h) = image_size(image, cx, avail.map(f32::from));
        // Padding and borders around the picture.
        let (t, r, b, l) = edges(&image.style);
        let (ml, mr) = atomic_margins(&image.style);
        let (w, h) = (px(w) + l + r, px(h) + t + b);
        let extent = object_extent(image.style.vertical_align, h, self.strut, font_size, shift);
        // The line holds the margin box (icons with `mr-2`).
        Measured { width: w + ml + mr, extent, ascent: h, descent: px(0.), margin_top: px(0.) }
    }

    /// `available`: the line width it must fit in (`None`: unconstrained).
    fn measure_atomic(
        &mut self,
        ix: u32,
        atomic: &AtomicBox,
        shift: Pixels,
        available: Option<Pixels>,
        cx: &RenderCx,
        window: &mut Window,
    ) -> Measured {
        let s = &atomic.style;
        let (top, right, bottom, left) = edges(s);
        let (ml, mr) = atomic_margins(s);
        let css_width = match s.width {
            style::Length::Px(w) if s.border_box => Some(px(w) - left - right),
            style::Length::Px(w) => Some(px(w)),
            _ => None,
        };
        let (inner_w, inner_h, first_above) = match &atomic.content {
            Some(content) => {
                let nested = self.nested(ix);
                let mut st = nested.borrow_mut();
                let width_of = |lines: &[Line]| lines.iter().map(|l| l.width).fold(px(0.), Pixels::max);
                let natural = width_of(&st.lines(content, cx, None, window));
                // Shrink-to-fit: min(max(min-content, available), max-content).
                let w = css_width.unwrap_or_else(|| match available {
                    Some(avail) => {
                        let min = width_of(&st.lines(content, cx, Some(px(0.)), window));
                        natural.min((avail - left - right - ml - mr).max(min))
                    }
                    None => natural,
                });
                let w = w.max(px(0.));
                let lines = st.lines(content, cx, Some(w), window);
                let h = lines.iter().map(|l| l.extent.height()).fold(px(0.), |a, b| a + b);
                let first = lines.first().map(|l| l.extent.above).unwrap_or_default();
                (w, h, first)
            }
            None => (css_width.unwrap_or_default(), px(0.), px(0.)),
        };
        if let Some(entry) = self.nested.get_mut(&ix) {
            entry.1 = inner_w;
        }
        let outer = |l: style::Length| match l {
            style::Length::Px(h) if s.border_box => Some(px(h)),
            style::Length::Px(h) => Some(px(h) + top + bottom),
            _ => None,
        };
        let mut height = outer(s.height).unwrap_or(inner_h + top + bottom);
        if let Some(min) = outer(s.min_height) {
            height = height.max(min);
        }
        if let Some(max) = outer(s.max_height) {
            height = height.min(max);
        }
        let (ml, mr) = atomic_margins(s);
        let (mt, mb) = vertical_margins(s);
        let above = match s.vertical_align {
            VerticalAlign::Baseline | VerticalAlign::Sub | VerticalAlign::Super | VerticalAlign::Shift(_)
                if atomic.content.as_ref().is_some_and(|c| !c.text.is_empty()) =>
            {
                // Baseline-aligned on its own first line.
                mt + top + first_above + shift
            }
            valign => object_extent(valign, height + mt + mb, self.strut, s.font_size, shift).above,
        };
        Measured {
            width: inner_w + left + right + ml + mr,
            extent: Extent { above, below: height + mt + mb - above },
            ascent: height,
            descent: px(0.),
            margin_top: mt,
        }
    }

    fn lines(&mut self, para: &Paragraph, cx: &RenderCx, wrap: Option<Pixels>, window: &mut Window) -> Rc<Vec<Line>> {
        self.layout(para, cx, wrap, window).0
    }

    /// Lines and the (wrap-adjusted) piece measurements they were built from.
    fn layout(
        &mut self,
        para: &Paragraph,
        cx: &RenderCx,
        wrap: Option<Pixels>,
        window: &mut Window,
    ) -> (Rc<Vec<Line>>, Rc<Vec<Measured>>) {
        if let Some((w, lines, measured)) = &self.lines
            && *w == wrap
        {
            return (lines.clone(), measured.clone());
        }
        let mut measured = self.measure(para, cx, window);
        // Inline-blocks without a CSS width shrink to fit the line width.
        if wrap.is_some() {
            let mut adjusted: Option<Vec<Measured>> = None;
            let font_size = InlineStyle::of(&para.style, false).font_size;
            for (ix, piece) in para.pieces.iter().enumerate() {
                let PieceKind::Object(oi) = piece.kind else { continue };
                let shift = px(para.spans[piece.span as usize].style.baseline_shift);
                let m = match &para.objects[oi as usize] {
                    InlineObject::Block(atomic) if atomic.style.width.is_auto() => self.measure_atomic(oi, atomic, shift, wrap, cx, window),
                    // Percentage widths (`width: 100%`, `max-width: 100%`)
                    // are of the line.
                    InlineObject::Image(image)
                        if matches!(image.style.width, style::Length::Percent(_)) || matches!(image.style.max_width, style::Length::Percent(_)) =>
                    {
                        self.measure_image(image, shift, font_size, wrap, cx)
                    }
                    _ => continue,
                };
                adjusted.get_or_insert_with(|| measured.to_vec())[ix] = m;
            }
            if let Some(adjusted) = adjusted {
                measured = Rc::new(adjusted);
            }
        }
        let lines = Rc::new(break_lines(para, &measured, self.strut, wrap));
        self.lines = Some((wrap, lines.clone(), measured.clone()));
        (lines, measured)
    }
}

/// Greedy line breaking at the paragraph's break opportunities.
fn break_lines(para: &Paragraph, measured: &[Measured], strut: Extent, wrap: Option<Pixels>) -> Vec<Line> {
    let collapse = para.style.white_space.collapses_spaces();
    let pieces = &para.pieces;
    let mut lines = Vec::new();
    let mut line = Line { items: Vec::new(), width: px(0.), extent: strut, clamped: false };
    let mut x = px(0.);
    let finish = |line: &mut Line, lines: &mut Vec<Line>, x: Pixels| {
        // Width excludes trailing spaces that collapse (they hang past the
        // edge in CSS); preserved ones take room (GitHub's search box: a
        // flex row of spans, the space one `white-space: pre`).
        let mut width = x;
        for &(ix, _) in line.items.iter().rev().take_while(|_| collapse) {
            if pieces[ix].kind == PieceKind::Space {
                width -= measured[ix].width;
            } else {
                break;
            }
        }
        line.width = width.max(px(0.));
        lines.push(std::mem::replace(line, Line { items: Vec::new(), width: px(0.), extent: strut, clamped: false }));
    };

    let mut i = 0;
    while i < pieces.len() {
        if pieces[i].kind == PieceKind::Newline {
            line.extent = line.extent.max(measured[i].extent);
            finish(&mut line, &mut lines, x);
            x = px(0.);
            i += 1;
            continue;
        }
        // The unbreakable group starting here.
        let mut j = i + 1;
        while j < pieces.len() && !pieces[j].break_before && pieces[j].kind != PieceKind::Newline {
            j += 1;
        }
        let mut group_width = px(0.);
        let mut trailing_space = px(0.);
        for k in i..j {
            group_width += measured[k].width;
            if pieces[k].kind == PieceKind::Space {
                trailing_space += measured[k].width;
            } else {
                trailing_space = px(0.);
            }
        }
        // Half a pixel's grace: layout rounds sizes to whole pixels, so a
        // box sized to its text (a flex item at its max-content width) can
        // come back a fraction narrower than the text it was measured from.
        if let Some(wrap) = wrap
            && !line.items.is_empty()
            && x + group_width - trailing_space > wrap + px(0.5)
        {
            finish(&mut line, &mut lines, x);
            x = px(0.);
        }
        for k in i..j {
            // Collapsible spaces at the start of a wrapped line disappear.
            if collapse && line.items.is_empty() && !lines.is_empty() && pieces[k].kind == PieceKind::Space {
                continue;
            }
            line.items.push((k, x));
            line.extent = line.extent.max(measured[k].extent);
            x += measured[k].width;
        }
        i = j;
    }
    if !line.items.is_empty() || lines.is_empty() {
        finish(&mut line, &mut lines, x);
    }
    if let Some(n) = para.style.line_clamp.map(|n| n as usize)
        && lines.len() > n
    {
        lines.truncate(n);
        if let Some(last) = lines.last_mut() {
            last.clamped = true;
        }
    }
    lines
}

/// How far down a button's content sits: centered when the box is taller.
fn content_offset(atomic: &AtomicBox, nested: &RefCell<FlowState>, content: &Paragraph, inner_w: Pixels, box_h: Pixels, cx: &RenderCx, window: &mut Window) -> Pixels {
    if !atomic.center {
        return px(0.);
    }
    let (t, _, b, _) = edges(&atomic.style);
    let wrap = content.style.white_space.wraps().then_some(inner_w);
    let lines = nested.borrow_mut().lines(content, cx, wrap, window);
    let content_h = lines.iter().map(|l| l.extent.height()).fold(px(0.), |a, b| a + b);
    ((box_h - t - b - content_h) / 2.0).max(px(0.))
}

/// Horizontal margins of an inline-block (they sit outside its box, in the line).
fn atomic_margins(s: &style::ComputedStyle) -> (Pixels, Pixels) {
    (px(s.margin.left.px_or_zero()), px(s.margin.right.px_or_zero()))
}

/// An inline-block's top and bottom margins (inside its line box).
fn vertical_margins(s: &style::ComputedStyle) -> (Pixels, Pixels) {
    (px(s.margin.top.px_or_zero()), px(s.margin.bottom.px_or_zero()))
}

/// CSS shrinks oversized corner radii to fit; GPUI draws nothing for them.
fn clamp_radius(r: Pixels, bounds: Bounds<Pixels>) -> Pixels {
    r.min(bounds.size.width.min(bounds.size.height) / 2.0).max(px(0.))
}

fn line_offset(para: &Paragraph, line: &Line, width: Pixels) -> Pixels {
    let free = (width - line.width).max(px(0.));
    match para.style.text_align {
        CssTextAlign::Left => px(0.),
        CssTextAlign::Center => free / 2.0,
        CssTextAlign::Right => free,
    }
}

/// Each laid-out piece's area and the element its text belongs to.
type PieceAreas = Vec<(Bounds<Pixels>, Option<page::NodeId>)>;

/// What prepaint collects while walking the flow (and nested flows).
#[derive(Default)]
struct Placement {
    /// Where each block object's box goes: (paragraph id, object index) → origin.
    blocks: HashMap<(usize, u32), Point<Pixels>>,
    objects: Vec<AnyElement>,
    links: Vec<(Hitbox, Arc<Url>)>,
    areas: PieceAreas,
    cursors: Vec<(Hitbox, CursorStyle)>,
}

/// Lay out a flow at `origin` / `width`: images become child elements, links
/// and cursors get hitboxes, pieces get hover areas. Recurses into inline-blocks.
#[allow(clippy::too_many_arguments)]
fn place(
    para: &Paragraph,
    state: &RefCell<FlowState>,
    origin: Point<Pixels>,
    width: Pixels,
    cx: &RenderCx,
    out: &mut Placement,
    window: &mut Window,
    app: &mut App,
) {
    let wrap = para.style.white_space.wraps().then_some(width);
    let (lines, measured) = state.borrow_mut().layout(para, cx, wrap, window);
    let link_of = |byte: usize| para.links.iter().find(|l| l.range.contains(&byte)).map(|l| l.url.clone());

    let mut y = origin.y;
    for line in lines.iter() {
        let baseline = y + line.extent.above;
        let dx = origin.x + line_offset(para, line, width);
        let mut run: Option<(Arc<Url>, Pixels, Pixels)> = None;
        let flush_run = |run: &mut Option<(Arc<Url>, Pixels, Pixels)>, out: &mut Placement, window: &mut Window| {
            if let Some((url, x0, x1)) = run.take() {
                let b = Bounds::new(point(dx + x0, y), size(x1 - x0, line.extent.height()));
                out.links.push((window.insert_hitbox(b, HitboxBehavior::Normal), url));
            }
        };
        for &(ix, x) in &line.items {
            let piece = &para.pieces[ix];
            let m = measured[ix];
            let area = Bounds::new(point(dx + x, y), size(m.width, line.extent.height()));
            let span_style = &para.spans[piece.span as usize].style;
            // `pointer-events: none` text isn't under the mouse.
            if cx.track_hover && !span_style.inert {
                out.areas.push((area, span_style.node));
            }
            if !span_style.inert
                && let Some(c) = cursor_style(span_style.cursor)
            {
                out.cursors.push((window.insert_hitbox(area, HitboxBehavior::Normal), c));
            }
            if let PieceKind::Object(oi) = piece.kind {
                let top = baseline - m.extent.above + m.margin_top;
                match &para.objects[oi as usize] {
                    InlineObject::Image(image) => {
                        let (ml, mr) = atomic_margins(&image.style);
                        let (w, h) = (m.width - ml - mr, m.ascent);
                        let (t, r, b, l) = edges(&image.style);
                        let picture = match &crate::view::shown_image(image, cx) {
                            Some(ImageState::Ready { image: picture, padded, frames, width, height }) => div()
                                .w(w - l - r)
                                .h(h - t - b)
                                .child(
                                    crate::view::transform::image(image, cx)
                                        .unwrap_or_else(|| {
                                            // Its own `border-radius` (GitHub's inline avatars
                                            // are 50% circles), else a rounded frame's.
                                            let bounds = Bounds::new(point(px(0.), px(0.)), size(w, h));
                                            let own = image.style.border_radius.map(|l| {
                                                let r = match l {
                                                    style::Length::Percent(p) => w.min(h) * (p / 100.0),
                                                    other => px(other.px_or_zero()),
                                                };
                                                clamp_radius(r, bounds)
                                            });
                                            let corners = crate::view::clipped_corners(
                                                &image.style,
                                                Some(f32::from(w - l - r)),
                                                Some(f32::from(h - t - b)),
                                                cx.round_clip,
                                                own,
                                            );
                                            let ratio = f32::from(w - l - r) / f32::from(h - t - b).max(1.0);
                                            // Its own `filter` first, then those it's inside.
                                            let own: Vec<style::Filter> = image.style.filter.iter().flat_map(|f| f.iter().copied()).chain(cx.palette.filter.iter().flat_map(|f| f.iter().copied())).collect();
                                            let (picture, padded) = crate::images::filtered(picture, padded, frames, (!own.is_empty()).then_some(&own[..]));
                                            crate::images::picture_fitted(picture, &padded, *width, *height, corners, image.style.object_fit, Some(ratio))
                                        }),
                                )
                                .into_any_element(),
                            _ => div().into_any_element(),
                        };
                        let mut el = div().w(w).h(h).pt(t).pr(r).pb(b).pl(l).child(picture).into_any_element();
                        el.layout_as_root(size(AvailableSpace::Definite(w), AvailableSpace::Definite(h)), window, app);
                        // Whole pixels: images drawn at fractional positions get
                        // a blurred edge row.
                        let at = point((dx + x + ml).round(), top.round());
                        el.prepaint_at(at, window, app);
                        out.objects.push(el);
                    }
                    InlineObject::Block(atomic) => {
                        if let (Some(content), Some((nested, inner_w))) =
                            (&atomic.content, state.borrow().nested.get(&oi).cloned())
                        {
                            let (t, _, _, l) = edges(&atomic.style);
                            let (ml, _) = atomic_margins(&atomic.style);
                            let dy = content_offset(atomic, &nested, content, inner_w, m.ascent, cx, window);
                            place(content, &nested, point(dx + x + ml + l, top + t + dy), inner_w, cx, out, window, app);
                        }
                    }
                    InlineObject::BlockBox(block) => {
                        let (ml, _) = atomic_margins(&block.style);
                        out.blocks.insert((para.id, oi), point(dx + x + ml, top));
                    }
                }
            }
            // Link hit areas: merge neighbouring pieces of the same link.
            match (link_of(piece.range.start), &mut run) {
                (Some(url), Some((current, _, x1))) if Arc::ptr_eq(&url, current) => *x1 = x + m.width,
                (url, current) => {
                    flush_run(current, out, window);
                    *current = url.map(|u| (u, x, x + m.width));
                }
            }
        }
        flush_run(&mut run, out, window);
        y += line.extent.height();
    }
}

/// Paint a flow's decorations, text and inline-block boxes. Recurses into
/// inline-blocks. (Images are painted separately, as child elements.)
#[allow(clippy::too_many_arguments)]
fn paint_flow(
    para: &Paragraph,
    state: &RefCell<FlowState>,
    origin: Point<Pixels>,
    width: Pixels,
    cx: &RenderCx,
    window: &mut Window,
    app: &mut App,
) {
    let wrap = para.style.white_space.wraps().then_some(width);
    let (lines, measured) = state.borrow_mut().layout(para, cx, wrap, window);
    let palette = &cx.palette;
    let ts = window.text_system().clone();

    let mut y = origin.y;
    for line in lines.iter() {
        let baseline = y + line.extent.above;
        let dx = origin.x + line_offset(para, line, width);

        // 1. Inline box decorations, behind the text.
        for b in &para.boxes {
            let mut span = None::<(Pixels, Pixels, usize)>;
            for &(ix, x) in &line.items {
                let r = &para.pieces[ix].range;
                if r.start >= b.range.start && r.end <= b.range.end {
                    let x1 = x + measured[ix].width;
                    span = Some(match span {
                        Some((x0, _, first)) => (x0, x1, first),
                        None => (x, x1, ix),
                    });
                }
            }
            let Some((x0, x1, first)) = span else { continue };
            let m = measured[first];
            let shift = px(para.spans[para.pieces[first].span as usize].style.baseline_shift);
            let top = baseline - shift - m.ascent - px(b.pad_top);
            let bottom = baseline - shift + m.descent + px(b.pad_bottom);
            let bg = b.background.map(|c| palette.color(c)).unwrap_or_default();
            let bounds = Bounds::from_corners(point(dx + x0, top), point(dx + x1, bottom));
            window.paint_quad(quad(
                bounds,
                clamp_radius(px(b.radius), bounds),
                bg,
                Edges {
                    top: px(b.border_width.top),
                    right: px(b.border_width.right),
                    bottom: px(b.border_width.bottom),
                    left: px(b.border_width.left),
                },
                palette.color(b.border_color),
                BorderStyle::Solid,
            ));
        }

        // 2. Inline-blocks: their box, then their content.
        for &(ix, x) in &line.items {
            let PieceKind::Object(oi) = para.pieces[ix].kind else { continue };
            let InlineObject::Block(atomic) = &para.objects[oi as usize] else { continue };
            let m = measured[ix];
            let top = baseline - m.extent.above + m.margin_top;
            let s = &atomic.style;
            let (ml, mr) = atomic_margins(s);
            let x = x + ml;
            let bounds = Bounds::new(point(dx + x, top), size(m.width - ml - mr, m.ascent));
            let border = s.border_width;
            if let Some(shadows) = &s.shadows {
                let r = |l: style::Length| clamp_radius(px(l.px_or_zero()), bounds);
                let corners = Corners {
                    top_left: r(s.border_radius[0]),
                    top_right: r(s.border_radius[1]),
                    bottom_right: r(s.border_radius[2]),
                    bottom_left: r(s.border_radius[3]),
                };
                if s.painted_background().is_some() {
                    window.paint_drop_shadows(bounds, corners, &crate::view::box_shadows(shadows, palette));
                } else {
                    // CSS paints an outer shadow only outside the box; with
                    // nothing over it, GPUI's whole shape showed (MDN's
                    // contents links, ruled by `box-shadow: -2px 0 0`, became
                    // grey slabs). Hard ones are painted as what's outside;
                    // blurred ones are left out, as on blocks.
                    for sh in shadows.iter().filter(|sh| !sh.inset && sh.blur <= 0.0) {
                        let shadow = Bounds::new(
                            point(bounds.origin.x + px(sh.x - sh.spread), bounds.origin.y + px(sh.y - sh.spread)),
                            size(bounds.size.width + px(2.0 * sh.spread), bounds.size.height + px(2.0 * sh.spread)),
                        );
                        for strip in outside(shadow, bounds) {
                            window.paint_quad(fill(strip, palette.color(sh.color)));
                        }
                    }
                }
            }
            if s.painted_background().is_some() || s.has_border() {
                let corner = |l: style::Length| {
                    let r = match l {
                        style::Length::Percent(p) => bounds.size.width.min(bounds.size.height) * (p / 100.0),
                        other => px(other.px_or_zero()),
                    };
                    clamp_radius(r, bounds)
                };
                window.paint_quad(quad(
                    bounds,
                    Corners {
                        top_left: corner(s.border_radius[0]),
                        top_right: corner(s.border_radius[1]),
                        bottom_right: corner(s.border_radius[2]),
                        bottom_left: corner(s.border_radius[3]),
                    },
                    s.painted_background().map(|c| palette.color(c)).unwrap_or_default(),
                    Edges {
                        top: px(border.top),
                        right: px(border.right),
                        bottom: px(border.bottom),
                        left: px(border.left),
                    },
                    palette.color(s.border_color.top),
                    BorderStyle::Solid,
                ));
            }
            if let (Some(content), Some((nested, inner_w))) = (&atomic.content, state.borrow().nested.get(&oi).cloned()) {
                let (t, _, _, l) = edges(s);
                let dy = content_offset(atomic, &nested, content, inner_w, m.ascent, cx, window);
                paint_flow(content, &nested, point(dx + x + l, top + t + dy), inner_w, cx, window, app);
            }
        }

        // 3. Text, in runs of consecutive same-span pieces. A line cut short
        // (`text-overflow: ellipsis`, `line-clamp`) stops at `limit` and
        // ends in "…".
        let overflows = para.style.text_ellipsis && line.width > width + px(0.5);
        let ellipsize = line.clamped || overflows;
        let mut limit = None;
        let mut ended = false;
        let mut last_style = None;
        let items = &line.items;
        let mut k = 0;
        while k < items.len() && !ended {
            let (ix, x) = items[k];
            let piece = &para.pieces[ix];
            if matches!(piece.kind, PieceKind::Object(_) | PieceKind::Newline) {
                k += 1;
                continue;
            }
            let mut end = k + 1;
            while end < items.len() {
                let next = &para.pieces[items[end].0];
                let prev = &para.pieces[items[end - 1].0];
                if next.span != piece.span
                    || next.range.start != prev.range.end
                    || matches!(next.kind, PieceKind::Object(_) | PieceKind::Newline)
                {
                    break;
                }
                end += 1;
            }
            // Don't paint (underline) trailing spaces at the end of a line.
            let mut last = end;
            while last > k + 1 && para.pieces[items[last - 1].0].kind == PieceKind::Space && end == items.len() {
                last -= 1;
            }
            let start = piece.range.start;
            let stop = para.pieces[items[last - 1].0].range.end;
            let text: SharedString = para.text[start..stop].to_owned().into();
            let style = &para.spans[piece.span as usize].style;
            let shaped = match &style.gradient {
                // Gradient text: a run per character, colored by where it
                // falls in its span (not cached: it may be animating).
                Some(gradient) => {
                    let span_start: usize = para.spans[..piece.span as usize].iter().map(|s| s.len).sum();
                    let span_len = para.spans[piece.span as usize].len.max(1) as f32;
                    let base = text_run(1, style, palette);
                    let runs: Vec<TextRun> = text
                        .char_indices()
                        .map(|(i, ch)| {
                            let frac = (start + i - span_start) as f32 / span_len;
                            let color = palette.color(gradient.at(frac.clamp(0.0, 1.0)));
                            TextRun { len: ch.len_utf8(), color, ..base.clone() }
                        })
                        .collect();
                    Rc::new(ts.shape_line(text.clone(), px(style.font_size), &runs, None))
                }
                None => state
                    .borrow_mut()
                    .shaped
                    .entry((wrap, start))
                    .or_insert_with(|| {
                        let run = text_run(text.len(), style, palette);
                        shapes::paint(&ts, text, px(style.font_size), &run)
                    })
                    .clone(),
            };
            let m = measured[ix];
            let origin = point(dx + x, baseline - px(style.baseline_shift) - m.ascent);
            last_style = Some((style, m, x + shaped.width));
            let room = ellipsize.then(|| {
                *limit.get_or_insert_with(|| {
                    let run = text_run('…'.len_utf8(), style, palette);
                    width - shapes::paint(&ts, "…".into(), px(style.font_size), &run).width
                })
            });
            // Where the room runs out, if that leaves something to read: a
            // short label our font draws a little wider than the page's
            // just gets clipped instead of turning into "…".
            let cut_at = room.filter(|&room| x + shaped.width > room).and_then(|room| {
                let full = &para.text[start..stop];
                let cut = shaped.index_for_x(room - x).unwrap_or(full.len());
                let cut = (0..=cut.min(full.len())).rev().find(|&i| full.is_char_boundary(i)).unwrap_or(0);
                (full[..cut].trim_end().chars().count() >= 2 || line.clamped).then_some(cut)
            });
            match cut_at {
                Some(cut) => {
                    // Cut where the room runs out; the rest of the line goes.
                    let full = &para.text[start..stop];
                    let kept = format!("{}…", full[..cut].trim_end());
                    let run = text_run(kept.len(), style, palette);
                    let cut_line = shapes::paint(&ts, kept.into(), px(style.font_size), &run);
                    cut_line.paint(origin, m.ascent + m.descent, TextAlign::Left, None, window, app).ok();
                    ended = true;
                }
                _ => {
                    shaped.paint(origin, m.ascent + m.descent, TextAlign::Left, None, window, app).ok();
                }
            }
            k = end;
        }
        // A clamped line that fit still says there's more.
        if line.clamped
            && !ended
            && let Some((style, m, x_end)) = last_style
        {
            let run = text_run('…'.len_utf8(), style, palette);
            let dots = shapes::paint(&ts, "…".into(), px(style.font_size), &run);
            let origin = point(dx + x_end, baseline - px(style.baseline_shift) - m.ascent);
            dots.paint(origin, m.ascent + m.descent, TextAlign::Left, None, window, app).ok();
        }
        y += line.extent.height();
    }
}

pub struct Prepainted {
    placement: Placement,
    placed_blocks: Vec<u32>,
    /// Where the text flow is (the whole element, unless it has block children).
    text: Bounds<Pixels>,
    /// The whole paragraph, for `:hover` reporting.
    hover: Option<Hitbox>,
}

/// Per-element interaction state, kept across frames.
#[derive(Default, Clone)]
struct LinkState {
    pressed: Rc<RefCell<Option<Arc<Url>>>>,
    hovered: Rc<RefCell<Option<Arc<Url>>>>,
    hovered_node: Rc<RefCell<Option<page::NodeId>>>,
}

impl IntoElement for InlineFlow {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for InlineFlow {
    type RequestLayoutState = ();
    type PrepaintState = Prepainted;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        // Reuse last frame's layout for this paragraph if it's still valid.
        if let Some(id) = id {
            let para = self.para.clone();
            let images = if para.objects.is_empty() { 0 } else { self.cx.images_generation };
            let generation = images.wrapping_mul(1 << 20) ^ self.cx.fonts_generation;
            self.state = window.with_element_state(id, |cache: Option<FlowCache>, _| {
                let state = match cache {
                    Some((p, g, state)) if Arc::ptr_eq(&p, &para) && g == generation => state,
                    _ => Rc::default(),
                };
                (state.clone(), (para, generation, state))
            });
        }

        // Block subtrees, as absolutely positioned children: GPUI lays them out
        // (shrink-to-fit, flex, everything) without affecting our size.
        self.blocks.clear();
        for (oi, object) in self.para.objects.iter().enumerate() {
            if let InlineObject::BlockBox(block) = object {
                let content = crate::view::render_block_box(block, &self.cx);
                // A percentage width is of the line's containing block: give
                // the box the flow's full width to take it from.
                let percent = matches!(block.style.width, style::Length::Percent(_));
                let mut el = div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .when(percent, |d| d.w_full())
                    .child(content)
                    .into_any_element();
                let layout_id = el.request_layout(window, cx);
                self.blocks.push((oi as u32, el, layout_id));
            }
        }
        if !self.blocks.is_empty()
            && let Some(id) = id
        {
            let _ = id;
            let key = stable_key(&self.para);
            let sizes = LEARNED.with_borrow_mut(|m| {
                if m.len() > 4096 {
                    m.clear();
                }
                m.entry(key).or_default().clone()
            });
            self.state.borrow_mut().block_sizes = sizes;
        }

        let para = self.para.clone();
        let rcx = self.cx.clone();
        let state = self.state.clone();
        let wraps = para.style.white_space.wraps();
        // In a clipping box the text may be narrower than its unwrapped
        // line (cut off, or ending in "…").
        let mut leaf_style = Style::default();
        if para.style.overflow != style::Overflow::Visible {
            leaf_style.min_size.width = px(0.).into();
        }
        let leaf = window.request_measured_layout(leaf_style, move |known, available, window, _cx| {
            let wrap = if !wraps {
                None
            } else {
                match (known.width, available.width) {
                    (Some(w), _) => Some(w),
                    (None, AvailableSpace::Definite(w)) => Some(w),
                    (None, AvailableSpace::MinContent) => Some(px(0.)),
                    (None, AvailableSpace::MaxContent) => None,
                }
            };
            let lines = state.borrow_mut().lines(&para, &rcx, wrap, window);
            let width = lines.iter().map(|l| l.width).fold(px(0.), Pixels::max);
            let height = lines.iter().map(|l| l.extent.height()).fold(px(0.), |a, b| a + b);
            size(known.width.unwrap_or(width), height)
        });
        if self.blocks.is_empty() {
            self.leaf = None;
            return (leaf, ());
        }
        self.leaf = Some(leaf);
        let children = std::iter::once(leaf).chain(self.blocks.iter().map(|(_, _, id)| *id)).collect::<Vec<_>>();
        (window.request_layout(Style::default(), children, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepainted {
        let text = match self.leaf {
            Some(leaf) => window.layout_bounds(leaf),
            None => bounds,
        };
        let mut placement = Placement::default();
        crate::view::transform::at(self.rotation, || {
            place(&self.para, &self.state, text.origin, text.size.width, &self.cx, &mut placement, window, cx)
        });

        // Move each block subtree into its slot in the line. If its real size
        // differs from what the lines were broken with, try again next frame.
        // Where this paragraph's first line sits, for an inline-block
        // around it.
        if let Some((_, lines, _)) = &self.state.borrow().lines
            && let Some(first) = lines.first()
        {
            FIRST_BASELINES.with_borrow_mut(|b| b.insert(self.para.id, text.origin.y + first.extent.above));
        }
        let sizes = self.state.borrow().block_sizes.clone();
        let mut changed = false;
        let mut placed = Vec::new();
        for (oi, el, layout_id) in &mut self.blocks {
            let actual = window.layout_bounds(*layout_id);
            let mut baseline = None;
            if let Some(target) = placement.blocks.get(&(self.para.id, *oi)) {
                let delta = *target - actual.origin;
                window.with_element_offset(delta, |window| el.prepaint(window, cx));
                placed.push(*oi);
                // Its text was just placed: measure its baseline from its top.
                let InlineObject::BlockBox(block) = &self.para.objects[*oi as usize] else { continue };
                baseline = first_paragraph(block)
                    .and_then(|p| FIRST_BASELINES.with_borrow(|b| b.get(&p).copied()))
                    .map(|y| y - target.y)
                    .filter(|&b| b >= px(0.) && b <= actual.size.height);
            }
            // A box sized by its line (a percentage or `auto` width) can't
            // be wider than the window: in a box that shrinks to fit, its
            // width is of a line made from its own last width, and grew
            // every frame (doubling, with two such boxes in a line).
            let mut width = actual.size.width;
            if let InlineObject::BlockBox(block) = &self.para.objects[*oi as usize]
                && !matches!(block.style.width, style::Length::Px(_))
            {
                width = width.min(window.viewport_size().width);
            }
            let size = BlockSize { width, height: actual.size.height, baseline };
            let old = sizes.borrow().get(oi).copied();
            // Not placed this frame: keep what we knew of its baseline.
            let size = BlockSize { baseline: baseline.or(old.and_then(|o| o.baseline)), ..size };
            if old.is_none_or(|o| o.width != size.width || o.height != size.height || (o.baseline.unwrap_or_default() - size.baseline.unwrap_or_default()).abs() > px(0.5)) {
                if std::env::var_os("FEATHER_DEBUG_INLINE").is_some() {
                    let o = old.map(|o| (f32::from(o.width), f32::from(o.height), o.baseline.map(f32::from)));
                    eprintln!("inline {} obj {oi}: {o:?} -> {:?}", self.para.id, (f32::from(size.width), f32::from(size.height), size.baseline.map(f32::from)));
                }
                sizes.borrow_mut().insert(*oi, size);
                changed = true;
            }
        }
        // Sizes that keep changing frame after frame (layout feeding back on
        // itself) are taken as they are after a few: a page that never
        // settles is laid out again every frame, and its lines jump about.
        let settled = {
            let mut state = self.state.borrow_mut();
            state.relayouts = if changed { state.relayouts.saturating_add(1) } else { 0 };
            state.relayouts > 4
        };
        if changed && !settled {
            // Re-break lines with the real sizes next frame.
            let mut state = self.state.borrow_mut();
            state.measured = None;
            state.lines = None;
            state.shaped.clear();
            drop(state);
            window.request_animation_frame();
        }
        let hover = self.cx.track_hover.then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal));
        Prepainted { placement, hover, placed_blocks: placed, text }
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepainted: &mut Prepainted,
        window: &mut Window,
        cx: &mut App,
    ) {
        let text = prepainted.text;
        let _ = bounds;
        paint_flow(&self.para, &self.state, text.origin, text.size.width, &self.cx, window, cx);
        for object in &mut prepainted.placement.objects {
            object.paint(window, cx);
        }
        for (oi, el, _) in &mut self.blocks {
            if prepainted.placed_blocks.contains(oi) {
                el.paint(window, cx);
            }
        }

        // Cursors, links (click/hover), and `:hover` reporting.
        for (hitbox, cursor) in &prepainted.placement.cursors {
            window.set_cursor_style(*cursor, hitbox);
        }
        for (hitbox, _) in &prepainted.placement.links {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
        let Some(id) = id else { return };
        let state = window.with_element_state(id, |state: Option<LinkState>, _| {
            let state = state.unwrap_or_default();
            (state.clone(), state)
        });

        if let Some(hitbox) = prepainted.hover.clone() {
            let areas = Rc::new(std::mem::take(&mut prepainted.placement.areas));
            let state = state.clone();
            let report = self.cx.on_inline_hover.clone();
            let para_id = self.para.id;
            window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let node = if hitbox.is_hovered(window) {
                    let mouse = window.mouse_position();
                    // Innermost first: nested (inline-block) areas come after their parent's.
                    areas.iter().rev().find(|(b, _)| b.contains(&mouse)).and_then(|(_, n)| *n)
                } else {
                    None
                };
                if node != *state.hovered_node.borrow() {
                    *state.hovered_node.borrow_mut() = node;
                    report(para_id, node, window, cx);
                }
            });
        }

        if prepainted.placement.links.is_empty() {
            return;
        }
        let links: Rc<Vec<(Hitbox, Arc<Url>)>> = Rc::new(std::mem::take(&mut prepainted.placement.links));
        let hit = |links: &[(Hitbox, Arc<Url>)], window: &Window| {
            links.iter().find(|(h, _)| h.is_hovered(window)).map(|(_, u)| u.clone())
        };
        {
            let links = links.clone();
            let state = state.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, _| {
                if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                    *state.pressed.borrow_mut() = hit(&links, window);
                }
            });
        }
        {
            let links = links.clone();
            let state = state.clone();
            let on_click = self.cx.on_link_click.clone();
            window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                    let pressed = state.pressed.borrow_mut().take();
                    if let Some(over) = hit(&links, window)
                        && pressed.as_deref() == Some(&*over)
                    {
                        on_click(&over, window, cx);
                    }
                }
            });
        }
        {
            let on_hover = self.cx.on_link_hover.clone();
            window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble {
                    let over = hit(&links, window);
                    if over.as_deref() != state.hovered.borrow().as_deref() {
                        *state.hovered.borrow_mut() = over.clone();
                        on_hover(over.as_deref(), window, cx);
                    }
                }
            });
        }
    }
}
