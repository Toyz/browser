//! Inline layout: our own GPUI element for a paragraph's content.
//!
//! GPUI's `StyledText` lays out one font size per block of text, with nothing
//! but text in it. Web inline content mixes sizes (`<sup>`, `<small>`), raises
//! and lowers baselines (`vertical-align`), draws boxes behind spans (`<code>`
//! chips, `<mark>`) and puts images in the line. So this element:
//!
//! 1. measures each piece (word / space / object) with GPUI's text shaper
//!    (results are cached frame to frame by GPUI's line layout cache),
//! 2. breaks lines greedily at the allowed break points, sizing each line from
//!    the ascent/descent/line-height of everything on it (CSS's line box model),
//! 3. paints inline-box decorations, then text grouped into runs, then images,
//! 4. hit-tests links itself (hand cursor, click, hover).
//!
//! It answers min-content (longest word) and max-content (unwrapped) queries
//! for real, so flex items and table cells size like they do in CSS.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::*;
use style::{LineHeight, TextAlign as CssTextAlign, VerticalAlign};

use crate::boxes::{ImageBox, InlineStyle, Paragraph, PieceKind};
use crate::images::ImageState;
use crate::view::{RenderCx, text_run};

pub struct InlineFlow {
    id: ElementId,
    para: Arc<Paragraph>,
    cx: RenderCx,
    state: Rc<RefCell<FlowState>>,
}

impl InlineFlow {
    pub fn new(para: Arc<Paragraph>, cx: &RenderCx) -> Self {
        Self {
            id: ElementId::NamedInteger("inline".into(), para.id as u64),
            para,
            cx: cx.clone(),
            state: Rc::default(),
        }
    }
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
}

#[derive(Clone, Copy)]
struct Measured {
    width: Pixels,
    extent: Extent,
    /// The piece's own font ascent/descent (for decorations and painting).
    ascent: Pixels,
    descent: Pixels,
}

struct Line {
    /// (piece index, x offset)
    items: Vec<(usize, Pixels)>,
    /// Width without trailing spaces.
    width: Pixels,
    extent: Extent,
}

#[derive(Default)]
struct FlowState {
    pieces: Option<Rc<Vec<Measured>>>,
    strut: Extent,
    /// Lines for the last wrap width asked for.
    lines: Option<(Option<Pixels>, Rc<Vec<Line>>)>,
}

fn font_for(style: &InlineStyle, cx: &RenderCx) -> Font {
    Font {
        family: cx.palette.font(&style.font_names, style.font_family),
        features: FontFeatures::default(),
        fallbacks: None,
        weight: FontWeight(style.font_weight as f32),
        style: if style.italic { FontStyle::Italic } else { FontStyle::Normal },
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

/// An inline image's natural size, CSS px.
fn object_size(image: &ImageBox, cx: &RenderCx) -> (f32, f32) {
    let s = &image.style;
    let intrinsic = match cx.images.borrow().get(&*image.src) {
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
    let (cw, ch) = (css(s.width), css(s.height));
    let w = cw.or(image.attr_width).or(intrinsic.map(|(w, _)| w));
    let h = ch.or(image.attr_height).or(intrinsic.map(|(_, h)| h));
    match (w, h, ratio) {
        // Only one dimension set in CSS: the other follows the aspect ratio.
        (Some(w), Some(_), Some(r)) if cw.is_some() && ch.is_none() => (w, w / r),
        (Some(_), Some(h), Some(r)) if ch.is_some() && cw.is_none() => (h * r, h),
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some(r)) => (w, w / r),
        (None, Some(h), Some(r)) => (h * r, h),
        _ => (0.0, 0.0),
    }
}

impl FlowState {
    fn measure(&mut self, para: &Paragraph, cx: &RenderCx, window: &mut Window) -> Rc<Vec<Measured>> {
        if let Some(m) = &self.pieces {
            return m.clone();
        }
        let ts = window.text_system().clone();

        // The strut: every line is at least as tall as the block's own font.
        let block = InlineStyle::of(&para.style, false);
        let block_font = ts.resolve_font(&font_for(&block, cx));
        let size = px(block.font_size);
        self.strut = extent_for(
            &InlineStyle { baseline_shift: 0.0, ..block },
            ts.ascent(block_font, size),
            ts.descent(block_font, size),
        );

        let mut out = Vec::with_capacity(para.pieces.len());
        for piece in &para.pieces {
            let style = &para.spans[piece.span as usize].style;
            let m = match piece.kind {
                PieceKind::Object(i) => {
                    let image = &para.objects[i as usize];
                    let (w, h) = object_size(image, cx);
                    let (w, h) = (px(w), px(h));
                    let shift = px(style.baseline_shift);
                    let extent = if image.style.vertical_align == VerticalAlign::Middle {
                        // Centered on the middle of lowercase letters.
                        let mid = px(block.font_size * 0.3);
                        Extent { above: h / 2.0 + mid + shift, below: h / 2.0 - mid - shift }
                    } else {
                        Extent { above: h + shift, below: -shift }
                    };
                    Measured { width: w, extent, ascent: h, descent: px(0.) }
                }
                kind => {
                    let text = &para.text[piece.range.clone()];
                    let size = px(style.font_size);
                    let layout = if kind == PieceKind::Newline {
                        ts.layout_line(" ", size, &[text_run(1, style, &cx.palette)], None)
                    } else {
                        ts.layout_line(text, size, &[text_run(text.len(), style, &cx.palette)], None)
                    };
                    let width = if kind == PieceKind::Newline { px(0.) } else { layout.width };
                    Measured {
                        width,
                        extent: extent_for(style, layout.ascent, layout.descent),
                        ascent: layout.ascent,
                        descent: layout.descent,
                    }
                }
            };
            out.push(m);
        }
        let out = Rc::new(out);
        self.pieces = Some(out.clone());
        out
    }

    fn lines(&mut self, para: &Paragraph, cx: &RenderCx, wrap: Option<Pixels>, window: &mut Window) -> Rc<Vec<Line>> {
        if let Some((w, lines)) = &self.lines
            && *w == wrap
        {
            return lines.clone();
        }
        let measured = self.measure(para, cx, window);
        let lines = Rc::new(break_lines(para, &measured, self.strut, wrap));
        self.lines = Some((wrap, lines.clone()));
        lines
    }
}

/// Greedy line breaking at the paragraph's break opportunities.
fn break_lines(para: &Paragraph, measured: &[Measured], strut: Extent, wrap: Option<Pixels>) -> Vec<Line> {
    let collapse = para.style.white_space.collapses_spaces();
    let pieces = &para.pieces;
    let mut lines = Vec::new();
    let mut line = Line { items: Vec::new(), width: px(0.), extent: strut };
    let mut x = px(0.);
    let finish = |line: &mut Line, lines: &mut Vec<Line>, x: Pixels| {
        // Width excludes trailing spaces (they hang past the edge in CSS).
        let mut width = x;
        for &(ix, _) in line.items.iter().rev() {
            if pieces[ix].kind == PieceKind::Space {
                width -= measured[ix].width;
            } else {
                break;
            }
        }
        line.width = width.max(px(0.));
        lines.push(std::mem::replace(line, Line { items: Vec::new(), width: px(0.), extent: strut }));
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
        if let Some(wrap) = wrap
            && !line.items.is_empty()
            && x + group_width - trailing_space > wrap
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
    lines
}

pub struct Prepainted {
    lines: Rc<Vec<Line>>,
    measured: Rc<Vec<Measured>>,
    objects: Vec<AnyElement>,
    links: Rc<Vec<(Hitbox, usize)>>,
}

/// Per-element interaction state, kept across frames.
#[derive(Default, Clone)]
struct LinkState {
    pressed: Rc<Cell<Option<usize>>>,
    hovered: Rc<Cell<Option<usize>>>,
}

impl IntoElement for InlineFlow {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl InlineFlow {
    fn wrap_width(&self, width: Pixels) -> Option<Pixels> {
        self.para.style.white_space.wraps().then_some(width)
    }

    fn line_offset(&self, line: &Line, width: Pixels) -> Pixels {
        let free = (width - line.width).max(px(0.));
        match self.para.style.text_align {
            CssTextAlign::Left => px(0.),
            CssTextAlign::Center => free / 2.0,
            CssTextAlign::Right => free,
        }
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
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, ()) {
        let para = self.para.clone();
        let rcx = self.cx.clone();
        let state = self.state.clone();
        let wraps = para.style.white_space.wraps();
        let layout_id = window.request_measured_layout(Style::default(), move |known, available, window, _cx| {
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
            let height = lines.iter().map(|l| l.extent.above + l.extent.below).fold(px(0.), |a, b| a + b);
            size(known.width.unwrap_or(width), height)
        });
        (layout_id, ())
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
        let wrap = self.wrap_width(bounds.size.width);
        let lines = self.state.borrow_mut().lines(&self.para, &self.cx, wrap, window);
        let measured = self.state.borrow_mut().measure(&self.para, &self.cx, window);
        let para = &self.para;

        let link_of = |byte: usize| para.links.iter().position(|l| l.range.contains(&byte));
        let mut objects = Vec::new();
        let mut links = Vec::new();
        let mut y = bounds.origin.y;
        for line in lines.iter() {
            let baseline = y + line.extent.above;
            let dx = bounds.origin.x + self.line_offset(line, bounds.size.width);
            let mut current_link: Option<(usize, Pixels, Pixels)> = None; // (link, x0, x1)
            for &(ix, x) in &line.items {
                let piece = &para.pieces[ix];
                let m = measured[ix];
                if let PieceKind::Object(oi) = piece.kind {
                    let image = &para.objects[oi as usize];
                    let w = m.width;
                    let h = m.ascent;
                    let top = baseline - m.extent.above;
                    let mut el = match self.cx.images.borrow().get(&*image.src) {
                        Some(ImageState::Ready { image, .. }) => {
                            img(image.clone()).w(w).h(h).object_fit(ObjectFit::Fill).into_any_element()
                        }
                        _ => div().w(w).h(h).into_any_element(),
                    };
                    el.layout_as_root(size(AvailableSpace::Definite(w), AvailableSpace::Definite(h)), window, cx);
                    el.prepaint_at(point(dx + x, top), window, cx);
                    objects.push(el);
                }
                // Link hit areas: merge neighbouring pieces of the same link.
                match (link_of(piece.range.start), &mut current_link) {
                    (Some(l), Some((cl, _, x1))) if *cl == l => *x1 = x + m.width,
                    (link, current) => {
                        if let Some((cl, x0, x1)) = current.take() {
                            let b = Bounds::new(point(dx + x0, y), size(x1 - x0, line.extent.above + line.extent.below));
                            links.push((window.insert_hitbox(b, HitboxBehavior::Normal), cl));
                        }
                        *current = link.map(|l| (l, x, x + m.width));
                    }
                }
            }
            if let Some((cl, x0, x1)) = current_link {
                let b = Bounds::new(point(dx + x0, y), size(x1 - x0, line.extent.above + line.extent.below));
                links.push((window.insert_hitbox(b, HitboxBehavior::Normal), cl));
            }
            y += line.extent.above + line.extent.below;
        }
        Prepainted { lines, measured, objects, links: Rc::new(links) }
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
        let para = &self.para;
        let palette = &self.cx.palette;
        let measured = &prepainted.measured;
        let ts = window.text_system().clone();

        let mut y = bounds.origin.y;
        for line in prepainted.lines.iter() {
            let baseline = y + line.extent.above;
            let dx = bounds.origin.x + self.line_offset(line, bounds.size.width);

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
                window.paint_quad(quad(
                    Bounds::from_corners(point(dx + x0, top), point(dx + x1, bottom)),
                    px(b.radius),
                    bg,
                    px(b.border_width),
                    palette.color(b.border_color),
                    BorderStyle::Solid,
                ));
            }

            // 2. Text, in runs of consecutive same-span pieces.
            let mut k = 0;
            let items = &line.items;
            while k < items.len() {
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
                if !text.trim().is_empty() || para.boxes.is_empty() {
                    let style = &para.spans[piece.span as usize].style;
                    let run = text_run(text.len(), style, palette);
                    let shaped = ts.shape_line(text, px(style.font_size), &[run], None);
                    let m = measured[ix];
                    let origin = point(dx + x, baseline - px(style.baseline_shift) - m.ascent);
                    shaped
                        .paint(origin, m.ascent + m.descent, TextAlign::Left, None, window, cx)
                        .ok();
                }
                k = end;
            }
            y += line.extent.above + line.extent.below;
        }

        // 3. Inline images.
        for object in &mut prepainted.objects {
            object.paint(window, cx);
        }

        // 4. Links: cursor, click, hover.
        if prepainted.links.is_empty() {
            return;
        }
        for (hitbox, _) in prepainted.links.iter() {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
        let Some(id) = id else { return };
        let state = window.with_element_state(id, |state: Option<LinkState>, _| {
            let state = state.unwrap_or_default();
            (state.clone(), state)
        });
        let hit = |links: &[(Hitbox, usize)], window: &Window| {
            links.iter().find(|(h, _)| h.is_hovered(window)).map(|(_, l)| *l)
        };
        {
            let links = prepainted.links.clone();
            let state = state.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, _| {
                if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                    state.pressed.set(hit(&links, window));
                }
            });
        }
        {
            let links = prepainted.links.clone();
            let state = state.clone();
            let urls = para.links.clone();
            let on_click = self.cx.on_link_click.clone();
            window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                    let pressed = state.pressed.take();
                    if let Some(over) = hit(&links, window)
                        && pressed == Some(over)
                    {
                        on_click(&urls[over].url, window, cx);
                    }
                }
            });
        }
        {
            let links = prepainted.links.clone();
            let urls = para.links.clone();
            let on_hover = self.cx.on_link_hover.clone();
            window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble {
                    let over = hit(&links, window);
                    if over != state.hovered.get() {
                        state.hovered.set(over);
                        on_hover(over.map(|l| &*urls[l].url), window, cx);
                    }
                }
            });
        }
    }
}
