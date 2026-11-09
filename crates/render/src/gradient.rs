//! CSS gradients. Two-stop linear ones are GPUI's own gradient quads; the
//! rest (more stops, hard stops, radial, repeating, patterns tiled by
//! `background-size`) are rasterized into images, cached by what they look
//! like and how big they are.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use gpui_kit::*;
use style::{BgRepeat, BgSize, ComputedStyle, Gradient, GradientShape, GradientStop, Length as CssLength, RadialSize, StopPos};

use crate::view::Palette;

/// The gradient layers of a box, filling its padding box behind the content.
pub struct GradientFill {
    layers: Arc<[Gradient]>,
    size: BgSize,
    repeat: BgRepeat,
    position: style::BgPos,
    radii: Corners<Pixels>,
    palette: Palette,
}

impl GradientFill {
    pub fn new(s: &ComputedStyle, palette: &Palette) -> Option<GradientFill> {
        let layers = s.gradients.clone().filter(|_| !s.masked)?;
        let radius = |i: usize, a: f32, b: f32| match s.border_radius[i] {
            CssLength::Px(r) => px((r - a.max(b)).max(0.0)),
            _ => px(0.0),
        };
        let bw = s.border_width;
        Some(GradientFill {
            layers,
            size: s.background_size,
            repeat: s.background_repeat,
            position: s.background_position,
            radii: Corners {
                top_left: radius(0, bw.left, bw.top),
                top_right: radius(1, bw.right, bw.top),
                bottom_right: radius(2, bw.right, bw.bottom),
                bottom_left: radius(3, bw.left, bw.bottom),
            },
            palette: palette.clone(),
        })
    }
}

impl IntoElement for GradientFill {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for GradientFill {
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
        let style = Style {
            position: Position::Absolute,
            inset: Edges::all(px(0.).into()),
            size: Size { width: relative(1.).into(), height: relative(1.).into() },
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) {
        if bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
            return;
        }
        // Bottom layer first.
        for layer in self.layers.iter().rev() {
            self.paint_layer(layer, bounds, window);
        }
    }
}

impl GradientFill {
    fn paint_layer(&self, g: &Gradient, bounds: Bounds<Pixels>, window: &mut Window) {
        let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let tile = match self.size {
            BgSize::Explicit(w, h) => {
                let w = match w {
                    CssLength::Px(v) => v,
                    CssLength::Percent(p) => bw * p / 100.0,
                    CssLength::Auto => bw,
                };
                let h = match h {
                    CssLength::Px(v) => v,
                    CssLength::Percent(p) => bh * p / 100.0,
                    CssLength::Auto => bh,
                };
                (w, h)
            }
            _ => (bw, bh),
        };
        if tile.0 < 0.5 || tile.1 < 0.5 {
            return;
        }
        let colors: Vec<Rgba> = g.stops.iter().map(|s| self.palette.color(s.color).to_rgb()).collect();
        let whole = (tile.0 - bw).abs() < 0.5 && (tile.1 - bh).abs() < 0.5;

        // GPUI draws plain two-color linear gradients itself.
        if whole && let Some(angle) = simple_linear(g, tile) {
            let to_hsla = |c: Rgba| -> Hsla { c.into() };
            let bg = linear_gradient(angle, linear_color_stop(to_hsla(colors[0]), 0.0), linear_color_stop(to_hsla(colors[1]), 1.0));
            window.paint_quad(fill(bounds, bg).corner_radii(self.radii));
            return;
        }

        // A pattern repeats: rasterize enough periods that a few hundred
        // pixels are covered per image, so big boxes take few sprites.
        let (rx, ry) = (self.repeat.x && tile.0 < bw, self.repeat.y && tile.1 < bh);
        let reps = |on: bool, t: f32| if on { (256.0 / t).ceil().clamp(1.0, 64.0) as u32 } else { 1 };
        let (nx, ny) = (reps(rx, tile.0), reps(ry, tile.1));
        let scale = window.scale_factor();
        let Some(image) = cached(g, &colors, tile, (nx, ny), scale, window) else { return };
        let block = size(px(tile.0 * nx as f32), px(tile.1 * ny as f32));

        // Where the first tile goes: `background-position` aligns the tile
        // in the free space (and the repeats extend both ways from there).
        let free = (bw - tile.0, bh - tile.1);
        let x0 = free.0 * self.position.x;
        let y0 = free.1 * self.position.y;
        let start = |offset: f32, step: f32, repeat: bool| if repeat { offset - (offset / step).ceil() * step } else { offset };
        let (sx, sy) = (start(x0, f32::from(block.width), rx), start(y0, f32::from(block.height), ry));
        let mut y = sy;
        while y < bh {
            let mut x = sx;
            while x < bw {
                let at = Bounds::new(bounds.origin + point(px(x), px(y)), block);
                window.paint_image(bounds, at, self.radii, image.clone(), 0, false).ok();
                if !rx {
                    break;
                }
                x += f32::from(block.width);
            }
            if !ry {
                break;
            }
            y += f32::from(block.height);
        }
    }
}

/// The angle of a linear gradient that's just "from one color to another".
fn simple_linear(g: &Gradient, (w, h): (f32, f32)) -> Option<f32> {
    if g.repeating || g.stops.len() != 2 {
        return None;
    }
    let ends = matches!(g.stops[0].pos, None | Some(StopPos::Fraction(0.0)))
        && matches!(g.stops[1].pos, None | Some(StopPos::Fraction(1.0)));
    if !ends {
        return None;
    }
    match g.shape {
        GradientShape::Linear(a) => Some(a.rem_euclid(360.0)),
        GradientShape::LinearCorner(sx, sy) => {
            let (dx, dy) = (sx * h, sy * w);
            Some(dx.atan2(-dy).to_degrees().rem_euclid(360.0))
        }
        GradientShape::Radial { .. } | GradientShape::Conic { .. } => None,
    }
}

/// Rasterized tiles by look and size: (image, last used), and the clock.
type Cache = (HashMap<u64, (Arc<RenderImage>, u64)>, u64);

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new((HashMap::new(), 0));
}

const CACHE_LIMIT: usize = 192;

fn cached(g: &Gradient, colors: &[Rgba], tile: (f32, f32), reps: (u32, u32), scale: f32, window: &mut Window) -> Option<Arc<RenderImage>> {
    // Pixels: the tile at device resolution, capped (gradients are smooth;
    // the GPU scales them up the rest of the way).
    let cap = 1024.0;
    let fit = |v: f32| (v * scale).ceil().clamp(1.0, cap);
    let (pw, ph) = (fit(tile.0), fit(tile.1));
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{g:?}").hash(&mut h);
        for c in colors {
            [c.r, c.g, c.b, c.a].map(f32::to_bits).hash(&mut h);
        }
        (tile.0.to_bits(), tile.1.to_bits(), reps, pw as u32, ph as u32).hash(&mut h);
        h.finish()
    };
    let (image, evicted) = CACHE.with_borrow_mut(|(map, clock)| {
        *clock += 1;
        let now = *clock;
        if let Some((image, used)) = map.get_mut(&key) {
            *used = now;
            return (Some(image.clone()), None);
        }
        let Some(image) = rasterize(g, colors, tile, (pw as u32, ph as u32), reps) else { return (None, None) };
        let image = Arc::new(image);
        map.insert(key, (image.clone(), now));
        let evicted = (map.len() > CACHE_LIMIT).then(|| {
            let oldest = map.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| *k)?;
            map.remove(&oldest).map(|(img, _)| img)
        });
        (Some(image), evicted.flatten())
    });
    if let Some(old) = evicted {
        window.drop_image(old).ok();
    }
    image
}

/// One tile (repeated `reps` times) as BGRA pixels.
fn rasterize(g: &Gradient, colors: &[Rgba], tile: (f32, f32), (pw, ph): (u32, u32), reps: (u32, u32)) -> Option<RenderImage> {
    let (tw, th) = tile;
    // The gradient line: its length and how to find a point's place on it.
    enum Line {
        Linear { dir: (f32, f32), len: f32 },
        Radial { center: (f32, f32), radii: (f32, f32) },
        /// Angle around `center` from `from` (radians), as a fraction of a
        /// turn.
        Conic { center: (f32, f32), from: f32 },
    }
    let line = match g.shape {
        GradientShape::Linear(deg) => {
            let r = deg.to_radians();
            let dir = (r.sin(), -r.cos());
            Line::Linear { dir, len: (tw * dir.0).abs() + (th * dir.1).abs() }
        }
        GradientShape::LinearCorner(sx, sy) => {
            let (dx, dy) = (sx * th, sy * tw);
            let n = (dx * dx + dy * dy).sqrt().max(1e-6);
            let dir = (dx / n, dy / n);
            Line::Linear { dir, len: (tw * dir.0).abs() + (th * dir.1).abs() }
        }
        GradientShape::Radial { circle, size, at } => {
            let (cx, cy) = (tw * at.x, th * at.y);
            let (left, right, top, bottom) = (cx, tw - cx, cy, th - cy);
            let side = |near: bool| {
                let pick = |a: f32, b: f32| if near { a.min(b) } else { a.max(b) };
                (pick(left, right), pick(top, bottom))
            };
            let radii = match size {
                RadialSize::Explicit(rx, ry) => (rx, ry),
                RadialSize::ClosestSide | RadialSize::FarthestSide => {
                    let (x, y) = side(size == RadialSize::ClosestSide);
                    if circle {
                        let r = if size == RadialSize::ClosestSide { x.min(y) } else { x.max(y) };
                        (r, r)
                    } else {
                        (x, y)
                    }
                }
                RadialSize::ClosestCorner | RadialSize::FarthestCorner => {
                    let (x, y) = side(size == RadialSize::ClosestCorner);
                    if circle {
                        let r = (x * x + y * y).sqrt();
                        (r, r)
                    } else {
                        // The ellipse through the corner with the sides' ratio.
                        (x * std::f32::consts::SQRT_2, y * std::f32::consts::SQRT_2)
                    }
                }
            };
            Line::Radial { center: (cx, cy), radii: (radii.0.max(1e-3), radii.1.max(1e-3)) }
        }
        GradientShape::Conic { from, at } => Line::Conic { center: (tw * at.x, th * at.y), from: from.to_radians() },
    };
    let length = match &line {
        Line::Linear { len, .. } => *len,
        Line::Radial { radii, .. } => radii.0,
        // Positions in degrees (`Px`) or fractions of a turn.
        Line::Conic { .. } => 360.0,
    }
    .max(1e-3);
    let stops = resolve_stops(&g.stops, colors, length);
    let (first, last) = (stops.first()?.0, stops.last()?.0);
    let period = last - first;

    let color_at = |t: f32| -> [f32; 4] {
        let t = if g.repeating && period > 1e-4 { first + (t - first).rem_euclid(period) } else { t };
        if t <= stops[0].0 {
            return stops[0].1;
        }
        for pair in stops.windows(2) {
            let ((a, ca), (b, cb)) = (pair[0], pair[1]);
            if t <= b {
                let f = if b > a { (t - a) / (b - a) } else { 1.0 };
                return std::array::from_fn(|i| ca[i] + (cb[i] - ca[i]) * f);
            }
        }
        stops[stops.len() - 1].1
    };

    let (out_w, out_h) = (pw * reps.0, ph * reps.1);
    if out_w as u64 * out_h as u64 > 4096 * 4096 {
        return None;
    }
    let mut tile_px = vec![0u8; (pw * ph * 4) as usize];
    for y in 0..ph {
        for x in 0..pw {
            // Pixel centers, in CSS px within the tile.
            let (px_, py) = ((x as f32 + 0.5) * tw / pw as f32, (y as f32 + 0.5) * th / ph as f32);
            let t = match &line {
                Line::Linear { dir, len } => ((px_ - tw / 2.0) * dir.0 + (py - th / 2.0) * dir.1) / len + 0.5,
                Line::Radial { center, radii } => {
                    let (dx, dy) = ((px_ - center.0) / radii.0, (py - center.1) / radii.1);
                    (dx * dx + dy * dy).sqrt()
                }
                Line::Conic { center, from } => {
                    // 0 at the top, clockwise.
                    let a = (px_ - center.0).atan2(-(py - center.1)) - from;
                    a.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
                }
            };
            let [r, g_, b, a] = color_at(t);
            // Stops are premultiplied (so fades to transparent don't go grey):
            // undo it for the image.
            let (r, g_, b) = if a > 1e-4 { (r / a, g_ / a, b / a) } else { (0.0, 0.0, 0.0) };
            let i = ((y * pw + x) * 4) as usize;
            let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            tile_px[i..i + 4].copy_from_slice(&[byte(b), byte(g_), byte(r), byte(a)]);
        }
    }
    let mut raw = vec![0u8; (out_w * out_h * 4) as usize];
    for (ty, row) in raw.chunks_exact_mut((out_w * 4) as usize).enumerate() {
        let src_row = &tile_px[((ty as u32 % ph) * pw * 4) as usize..][..(pw * 4) as usize];
        for chunk in row.chunks_exact_mut((pw * 4) as usize) {
            chunk.copy_from_slice(src_row);
        }
    }
    let out = image::RgbaImage::from_raw(out_w, out_h, raw)?;
    Some(RenderImage::new(vec![image::Frame::new(out)]))
}

/// Stop positions along the line (0..1), filled in and in order, with
/// premultiplied colors.
fn resolve_stops(stops: &[GradientStop], colors: &[Rgba], length: f32) -> Vec<(f32, [f32; 4])> {
    let n = stops.len();
    let mut pos: Vec<Option<f32>> = stops
        .iter()
        .map(|s| {
            s.pos.map(|p| match p {
                StopPos::Fraction(f) => f,
                StopPos::Px(v) => v / length,
            })
        })
        .collect();
    if pos[0].is_none() {
        pos[0] = Some(0.0);
    }
    if pos[n - 1].is_none() {
        pos[n - 1] = Some(1.0);
    }
    // Never backwards.
    let mut max = f32::MIN;
    for p in pos.iter_mut().flatten() {
        if *p < max {
            *p = max;
        }
        max = *p;
    }
    // Unpositioned stops spread evenly between their neighbours.
    let mut i = 0;
    while i < n {
        if pos[i].is_some() {
            i += 1;
            continue;
        }
        let start = i - 1;
        let mut end = i;
        while pos[end].is_none() {
            end += 1;
        }
        let (a, b) = (pos[start].unwrap_or(0.0), pos[end].unwrap_or(1.0));
        for (k, slot) in pos.iter_mut().enumerate().take(end).skip(i) {
            *slot = Some(a + (b - a) * (k - start) as f32 / (end - start) as f32);
        }
        i = end;
    }
    pos.into_iter()
        .zip(colors)
        .map(|(p, c)| (p.unwrap_or(0.0), [c.r * c.a, c.g * c.a, c.b * c.a, c.a]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{GradientStop, Rgba, StopPos, resolve_stops};

    fn stop(pos: Option<StopPos>) -> GradientStop {
        GradientStop { color: style::Color::TRANSPARENT, pos }
    }

    #[test]
    fn stops_fill_in() {
        let c = Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
        let s = resolve_stops(&[stop(None), stop(None), stop(Some(StopPos::Px(50.0))), stop(None)], &[c; 4], 100.0);
        let p: Vec<f32> = s.iter().map(|x| x.0).collect();
        assert_eq!(p, vec![0.0, 0.25, 0.5, 1.0]);
        // Out of order positions are clamped forward.
        let s = resolve_stops(&[stop(Some(StopPos::Fraction(0.6))), stop(Some(StopPos::Fraction(0.2)))], &[c; 2], 100.0);
        assert_eq!((s[0].0, s[1].0), (0.6, 0.6));
    }
}
