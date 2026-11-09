//! `<canvas>` 2D drawing: the page's context records operations (in JS);
//! before each snapshot they're replayed here onto a tiny-skia pixmap, and
//! the snapshot shows the canvas as a PNG.
//!
//! Supported: paths (lines, curves, arcs, ellipses, rects), fill and
//! stroke with colors and linear/radial gradients, transforms,
//! save/restore, globalAlpha, clip, clearRect and drawImage (images from
//! the cache, other canvases). Text isn't drawn yet.

use std::collections::HashMap;
use std::sync::Arc;

use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Paint, Path, PathBuilder,
    Pattern, Pixmap, Point, RadialGradient, Rect, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};

/// One argument of a recorded operation.
#[derive(Debug, Clone)]
pub enum Arg {
    N(f64),
    S(String),
    A(Vec<Arg>),
}

impl Arg {
    fn num(&self) -> f32 {
        match self {
            Arg::N(n) => *n as f32,
            Arg::S(s) => s.parse().unwrap_or(0.0),
            Arg::A(_) => 0.0,
        }
    }
    fn str(&self) -> &str {
        match self {
            Arg::S(s) => s,
            _ => "",
        }
    }
}

#[derive(Clone)]
enum Fill {
    Color(Color),
    Linear { from: Point, to: Point, stops: Vec<(f32, Color)> },
    Radial { from: Point, to: Point, radius: f32, stops: Vec<(f32, Color)> },
}

#[derive(Clone)]
struct State {
    transform: Transform,
    fill: Fill,
    stroke: Fill,
    line_width: f32,
    alpha: f32,
    cap: LineCap,
    join: LineJoin,
    miter: f32,
    dash: Vec<f32>,
    dash_offset: f32,
    clip: Option<Mask>,
    blend: BlendMode,
}

impl Default for State {
    fn default() -> Self {
        State {
            transform: Transform::identity(),
            fill: Fill::Color(Color::BLACK),
            stroke: Fill::Color(Color::BLACK),
            line_width: 1.0,
            alpha: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter: 10.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            clip: None,
            blend: BlendMode::SourceOver,
        }
    }
}

pub struct Canvas {
    pixmap: Pixmap,
    state: State,
    stack: Vec<State>,
    path: PathBuilder,
    /// The current point has been set (moveTo before lineTo...).
    started: bool,
    /// The PNG `data:` URL of the current pixels, made on demand.
    url: Option<Arc<str>>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Option<Canvas> {
        Some(Canvas {
            pixmap: Pixmap::new(width.clamp(1, 8192), height.clamp(1, 8192))?,
            state: State::default(),
            stack: Vec::new(),
            path: PathBuilder::new(),
            started: false,
            url: None,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.pixmap.width(), self.pixmap.height())
    }

    /// The pixels as a PNG `data:` URL.
    pub fn data_url(&mut self) -> Arc<str> {
        if let Some(url) = &self.url {
            return url.clone();
        }
        let png = self.pixmap.encode_png().unwrap_or_default();
        let url: Arc<str> = format!("data:image/png;base64,{}", base64(&png)).into();
        self.url = Some(url.clone());
        url
    }

    /// Run recorded operations (`[name, args...]`). `others` holds the
    /// page's other canvases, for drawImage(canvas).
    pub fn apply(&mut self, ops: &[Vec<Arg>], others: &HashMap<u64, Pixmap>, images: &dyn Fn(&str) -> Option<Arc<Pixmap>>) {
        if !ops.is_empty() {
            self.url = None;
        }
        for op in ops {
            let Some((name, args)) = op.split_first() else { continue };
            let n = |i: usize| args.get(i).map(Arg::num).unwrap_or(0.0);
            match name.str() {
                "save" => self.stack.push(self.state.clone()),
                "restore" => {
                    if let Some(s) = self.stack.pop() {
                        self.state = s;
                    }
                }
                "setTransform" => self.state.transform = Transform::from_row(n(0), n(1), n(2), n(3), n(4), n(5)),
                "transform" => {
                    self.state.transform = self.state.transform.pre_concat(Transform::from_row(n(0), n(1), n(2), n(3), n(4), n(5)))
                }
                "translate" => self.state.transform = self.state.transform.pre_translate(n(0), n(1)),
                "scale" => self.state.transform = self.state.transform.pre_scale(n(0), n(1)),
                "rotate" => {
                    self.state.transform = self.state.transform.pre_concat(Transform::from_rotate(n(0).to_degrees()))
                }
                "fillStyle" | "strokeStyle" => {
                    let fill = args.first().and_then(parse_fill);
                    if let Some(fill) = fill {
                        if name.str() == "fillStyle" {
                            self.state.fill = fill;
                        } else {
                            self.state.stroke = fill;
                        }
                    }
                }
                "lineWidth" => {
                    if n(0) > 0.0 {
                        self.state.line_width = n(0);
                    }
                }
                "globalAlpha" => {
                    if (0.0..=1.0).contains(&n(0)) {
                        self.state.alpha = n(0);
                    }
                }
                "lineCap" => {
                    self.state.cap = match args.first().map(Arg::str) {
                        Some("round") => LineCap::Round,
                        Some("square") => LineCap::Square,
                        _ => LineCap::Butt,
                    }
                }
                "lineJoin" => {
                    self.state.join = match args.first().map(Arg::str) {
                        Some("round") => LineJoin::Round,
                        Some("bevel") => LineJoin::Bevel,
                        _ => LineJoin::Miter,
                    }
                }
                "miterLimit" => self.state.miter = n(0),
                "setLineDash" => {
                    self.state.dash = match args.first() {
                        Some(Arg::A(v)) => v.iter().map(Arg::num).collect(),
                        _ => Vec::new(),
                    }
                }
                "lineDashOffset" => self.state.dash_offset = n(0),
                "globalCompositeOperation" => {
                    self.state.blend = match args.first().map(Arg::str) {
                        Some("destination-out") => BlendMode::DestinationOut,
                        Some("destination-over") => BlendMode::DestinationOver,
                        Some("source-in") => BlendMode::SourceIn,
                        Some("source-atop") => BlendMode::SourceAtop,
                        Some("destination-in") => BlendMode::DestinationIn,
                        Some("lighter") => BlendMode::Plus,
                        Some("multiply") => BlendMode::Multiply,
                        Some("screen") => BlendMode::Screen,
                        Some("xor") => BlendMode::Xor,
                        Some("copy") => BlendMode::Source,
                        _ => BlendMode::SourceOver,
                    }
                }
                "beginPath" => {
                    self.path = PathBuilder::new();
                    self.started = false;
                }
                "moveTo" => {
                    let p = self.pt(n(0), n(1));
                    self.path.move_to(p.x, p.y);
                    self.started = true;
                }
                "lineTo" => {
                    let p = self.pt(n(0), n(1));
                    self.line_to(p);
                }
                "closePath" => {
                    if self.started {
                        self.path.close();
                    }
                }
                "quadraticCurveTo" => {
                    let (c, p) = (self.pt(n(0), n(1)), self.pt(n(2), n(3)));
                    self.ensure_start(c);
                    self.path.quad_to(c.x, c.y, p.x, p.y);
                }
                "bezierCurveTo" => {
                    let (c1, c2, p) = (self.pt(n(0), n(1)), self.pt(n(2), n(3)), self.pt(n(4), n(5)));
                    self.ensure_start(c1);
                    self.path.cubic_to(c1.x, c1.y, c2.x, c2.y, p.x, p.y);
                }
                "arc" => self.ellipse(n(0), n(1), n(2), n(2), 0.0, n(3), n(4), args.get(5).is_some_and(|a| a.num() != 0.0)),
                "ellipse" => self.ellipse(n(0), n(1), n(2), n(3), n(4), n(5), n(6), args.get(7).is_some_and(|a| a.num() != 0.0)),
                "rect" => self.rect(n(0), n(1), n(2), n(3)),
                "roundRect" => {
                    let r = args.get(4).map(Arg::num).unwrap_or(0.0).min(n(2).abs() / 2.0).min(n(3).abs() / 2.0);
                    self.round_rect(n(0), n(1), n(2), n(3), r);
                }
                "fill" => {
                    let rule = if args.first().map(Arg::str) == Some("evenodd") { FillRule::EvenOdd } else { FillRule::Winding };
                    if let Some(path) = self.path.clone().finish() {
                        self.fill_path(&path, rule);
                    }
                }
                "stroke" => {
                    if let Some(path) = self.path.clone().finish() {
                        self.stroke_path(&path);
                    }
                }
                "clip" => {
                    if let Some(path) = self.path.clone().finish() {
                        let mut mask = Mask::new(self.pixmap.width(), self.pixmap.height()).expect("mask");
                        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
                        if let Some(old) = &self.state.clip {
                            // Intersect: keep where both are set.
                            let data: Vec<u8> = old.data().iter().zip(mask.data()).map(|(a, b)| ((*a as u16 * *b as u16) / 255) as u8).collect();
                            mask.data_mut().copy_from_slice(&data);
                        }
                        self.state.clip = Some(mask);
                    }
                }
                "fillRect" | "strokeRect" | "clearRect" => {
                    let mut pb = PathBuilder::new();
                    let corners = [(n(0), n(1)), (n(0) + n(2), n(1)), (n(0) + n(2), n(1) + n(3)), (n(0), n(1) + n(3))];
                    for (i, (x, y)) in corners.into_iter().enumerate() {
                        let p = self.pt(x, y);
                        if i == 0 { pb.move_to(p.x, p.y) } else { pb.line_to(p.x, p.y) }
                    }
                    pb.close();
                    let Some(path) = pb.finish() else { continue };
                    match name.str() {
                        "fillRect" => self.fill_path(&path, FillRule::Winding),
                        "strokeRect" => self.stroke_path(&path),
                        _ => {
                            let mut paint = Paint::default();
                            paint.blend_mode = BlendMode::Clear;
                            self.pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), self.state.clip.as_ref());
                        }
                    }
                }
                "drawImage" | "drawCanvas" => {
                    let source: Option<Arc<Pixmap>> = if name.str() == "drawCanvas" {
                        others.get(&(n(0) as u64)).map(|p| Arc::new(p.clone()))
                    } else {
                        args.first().and_then(|a| images(a.str()))
                    };
                    let Some(src) = source else { continue };
                    let (iw, ih) = (src.width() as f32, src.height() as f32);
                    let rest: Vec<f32> = args[1..].iter().map(Arg::num).collect();
                    let (sx, sy, sw, sh, dx, dy, dw, dh) = match rest.len() {
                        2 => (0.0, 0.0, iw, ih, rest[0], rest[1], iw, ih),
                        4 => (0.0, 0.0, iw, ih, rest[0], rest[1], rest[2], rest[3]),
                        8.. => (rest[0], rest[1], rest[2], rest[3], rest[4], rest[5], rest[6], rest[7]),
                        _ => continue,
                    };
                    if sw == 0.0 || sh == 0.0 {
                        continue;
                    }
                    self.draw_image(&src, (sx, sy, sw, sh), (dx, dy, dw, dh));
                }
                _ => {}
            }
        }
    }

    fn pt(&self, x: f32, y: f32) -> Point {
        let mut p = [Point::from_xy(x, y)];
        self.state.transform.map_points(&mut p);
        p[0]
    }

    fn ensure_start(&mut self, p: Point) {
        if !self.started {
            self.path.move_to(p.x, p.y);
            self.started = true;
        }
    }

    fn line_to(&mut self, p: Point) {
        if self.started {
            self.path.line_to(p.x, p.y);
        } else {
            self.path.move_to(p.x, p.y);
            self.started = true;
        }
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let pts = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
        for (i, (px, py)) in pts.into_iter().enumerate() {
            let p = self.pt(px, py);
            if i == 0 { self.path.move_to(p.x, p.y) } else { self.path.line_to(p.x, p.y) }
        }
        self.path.close();
        let start = self.pt(x, y);
        self.path.move_to(start.x, start.y);
        self.started = true;
    }

    fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        if r <= 0.0 {
            return self.rect(x, y, w, h);
        }
        let first = self.pt(x + r, y);
        self.path.move_to(first.x, first.y);
        self.started = true;
        let p = self.pt(x + w - r, y);
        self.path.line_to(p.x, p.y);
        self.ellipse(x + w - r, y + r, r, r, 0.0, -std::f32::consts::FRAC_PI_2, 0.0, false);
        let p = self.pt(x + w, y + h - r);
        self.path.line_to(p.x, p.y);
        self.ellipse(x + w - r, y + h - r, r, r, 0.0, 0.0, std::f32::consts::FRAC_PI_2, false);
        let p = self.pt(x + r, y + h);
        self.path.line_to(p.x, p.y);
        self.ellipse(x + r, y + h - r, r, r, 0.0, std::f32::consts::FRAC_PI_2, std::f32::consts::PI, false);
        let p = self.pt(x, y + r);
        self.path.line_to(p.x, p.y);
        self.ellipse(x + r, y + r, r, r, 0.0, std::f32::consts::PI, 1.5 * std::f32::consts::PI, false);
        self.path.close();
    }

    /// An elliptical arc as cubic Béziers (at most a quarter turn each),
    /// joined to the current point by a line, like canvas does.
    #[allow(clippy::too_many_arguments)]
    fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, rotation: f32, start: f32, end: f32, ccw: bool) {
        use std::f32::consts::TAU;
        if rx < 0.0 || ry < 0.0 {
            return;
        }
        let mut sweep = end - start;
        if !ccw && sweep >= TAU || ccw && -sweep >= TAU {
            sweep = if ccw { -TAU } else { TAU };
        } else {
            if !ccw && sweep < 0.0 {
                sweep = sweep.rem_euclid(TAU);
            }
            if ccw && sweep > 0.0 {
                sweep = -((-sweep).rem_euclid(TAU));
                if sweep == 0.0 {
                    sweep = -TAU;
                }
            }
        }
        let (cos_r, sin_r) = (rotation.cos(), rotation.sin());
        let at = |a: f32| -> (f32, f32) {
            let (x, y) = (rx * a.cos(), ry * a.sin());
            (cx + x * cos_r - y * sin_r, cy + x * sin_r + y * cos_r)
        };
        let (sx, sy) = at(start);
        let s = self.pt(sx, sy);
        self.line_to(s);
        let segments = (sweep.abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.0) as usize;
        let step = sweep / segments as f32;
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        let mut a = start;
        for _ in 0..segments {
            let b = a + step;
            let d = |t: f32| -> (f32, f32) {
                let (x, y) = (-rx * t.sin(), ry * t.cos());
                (x * cos_r - y * sin_r, x * sin_r + y * cos_r)
            };
            let (p0, p3) = (at(a), at(b));
            let (d0, d3) = (d(a), d(b));
            let c1 = self.pt(p0.0 + k * d0.0, p0.1 + k * d0.1);
            let c2 = self.pt(p3.0 - k * d3.0, p3.1 - k * d3.1);
            let e = self.pt(p3.0, p3.1);
            self.path.cubic_to(c1.x, c1.y, c2.x, c2.y, e.x, e.y);
            a = b;
        }
    }

    fn paint(&self, fill: &Fill) -> Paint<'static> {
        let alpha = self.state.alpha;
        let fade = |c: Color| {
            let mut c = c;
            c.apply_opacity(alpha);
            c
        };
        let stops = |s: &[(f32, Color)]| s.iter().map(|(o, c)| GradientStop::new(*o, fade(*c))).collect::<Vec<_>>();
        let t = self.state.transform;
        let shader = match fill {
            Fill::Color(c) => Shader::SolidColor(fade(*c)),
            Fill::Linear { from, to, stops: s } => {
                LinearGradient::new(*from, *to, stops(s), SpreadMode::Pad, t).unwrap_or(Shader::SolidColor(Color::TRANSPARENT))
            }
            Fill::Radial { from, to, radius, stops: s } => RadialGradient::new(*from, *to, *radius, stops(s), SpreadMode::Pad, t)
                .unwrap_or(Shader::SolidColor(Color::TRANSPARENT)),
        };
        Paint { shader, anti_alias: true, blend_mode: self.state.blend, ..Paint::default() }
    }

    fn fill_path(&mut self, path: &Path, rule: FillRule) {
        let paint = self.paint(&self.state.fill.clone());
        self.pixmap.fill_path(path, &paint, rule, Transform::identity(), self.state.clip.as_ref());
    }

    fn stroke_path(&mut self, path: &Path) {
        let paint = self.paint(&self.state.stroke.clone());
        let t = self.state.transform;
        let scale = (t.sx * t.sy - t.kx * t.ky).abs().sqrt().max(0.001);
        let mut stroke = Stroke {
            width: self.state.line_width * scale,
            line_cap: self.state.cap,
            line_join: self.state.join,
            miter_limit: self.state.miter,
            ..Stroke::default()
        };
        if !self.state.dash.is_empty() {
            let mut d: Vec<f32> = self.state.dash.iter().map(|v| v * scale).collect();
            if d.len() % 2 == 1 {
                d.extend(d.clone());
            }
            stroke.dash = StrokeDash::new(d, self.state.dash_offset * scale);
        }
        self.pixmap.stroke_path(path, &paint, &stroke, Transform::identity(), self.state.clip.as_ref());
    }

    fn draw_image(&mut self, src: &Pixmap, (sx, sy, sw, sh): (f32, f32, f32, f32), (dx, dy, dw, dh): (f32, f32, f32, f32)) {
        let Some(rect) = Rect::from_xywh(dx.min(dx + dw), dy.min(dy + dh), dw.abs(), dh.abs()) else { return };
        // Image space → canvas user space → device.
        let to_dest = Transform::from_row(dw / sw, 0.0, 0.0, dh / sh, dx - sx * dw / sw, dy - sy * dh / sh);
        // (fill_path applies the canvas transform to the shader too.)
        let pattern = Pattern::new(src.as_ref(), SpreadMode::Pad, FilterQuality::Bilinear, self.state.alpha, to_dest);
        let paint = Paint { shader: pattern, anti_alias: true, blend_mode: self.state.blend, ..Paint::default() };
        let path = PathBuilder::from_rect(rect);
        self.pixmap.fill_path(&path, &paint, FillRule::Winding, self.state.transform, self.state.clip.as_ref());
    }

    pub fn pixmap(&self) -> &Pixmap {
        &self.pixmap
    }
}

fn css_color(text: &str) -> Option<Color> {
    let v = style::props::parse_color_str(text)?;
    Some(Color::from_rgba8((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// A fillStyle/strokeStyle: a color string, or a gradient
/// `["linear", x0, y0, x1, y1, [[offset, color]...]]` /
/// `["radial", x0, y0, r0, x1, y1, r1, stops]`.
fn parse_fill(arg: &Arg) -> Option<Fill> {
    match arg {
        Arg::S(s) => css_color(s).map(Fill::Color),
        Arg::A(v) => {
            let kind = v.first()?.str();
            let n = |i: usize| v.get(i).map(Arg::num).unwrap_or(0.0);
            let stops_at = |i: usize| -> Vec<(f32, Color)> {
                match v.get(i) {
                    Some(Arg::A(stops)) => stops
                        .iter()
                        .filter_map(|s| match s {
                            Arg::A(pair) => Some((pair.first()?.num().clamp(0.0, 1.0), css_color(pair.get(1)?.str())?)),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                }
            };
            match kind {
                "linear" => {
                    let stops = stops_at(5);
                    single_or(stops, |stops| Fill::Linear { from: Point::from_xy(n(1), n(2)), to: Point::from_xy(n(3), n(4)), stops })
                }
                "radial" => {
                    let stops = stops_at(7);
                    single_or(stops, |stops| Fill::Radial { from: Point::from_xy(n(1), n(2)), to: Point::from_xy(n(4), n(5)), radius: n(6).max(0.001), stops })
                }
                _ => None,
            }
        }
        Arg::N(_) => None,
    }
}

/// tiny-skia wants two stops at least.
fn single_or(stops: Vec<(f32, Color)>, f: impl FnOnce(Vec<(f32, Color)>) -> Fill) -> Option<Fill> {
    match stops.len() {
        0 => Some(Fill::Color(Color::TRANSPARENT)),
        1 => Some(Fill::Color(stops[0].1)),
        _ => Some(f(stops)),
    }
}

/// Decode an image (for drawImage), cached by URL.
pub fn decode_image(bytes: &[u8]) -> Option<Pixmap> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut pixmap = Pixmap::new(w, h)?;
    // tiny-skia stores premultiplied RGBA.
    for (dst, src) in pixmap.data_mut().chunks_exact_mut(4).zip(img.as_raw().chunks_exact(4)) {
        let a = src[3] as u16;
        dst[0] = ((src[0] as u16 * a) / 255) as u8;
        dst[1] = ((src[1] as u16 * a) / 255) as u8;
        dst[2] = ((src[2] as u16 * a) / 255) as u8;
        dst[3] = src[3];
    }
    Some(pixmap)
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_shapes() {
        let mut c = Canvas::new(20, 20).unwrap();
        let s = |v: &str| Arg::S(v.into());
        let ops = vec![
            vec![s("fillStyle"), s("red")],
            vec![s("fillRect"), Arg::N(0.0), Arg::N(0.0), Arg::N(10.0), Arg::N(10.0)],
            vec![s("beginPath")],
            vec![s("fillStyle"), s("#00f")],
            vec![s("arc"), Arg::N(15.0), Arg::N(15.0), Arg::N(4.0), Arg::N(0.0), Arg::N(6.3)],
            vec![s("fill")],
        ];
        c.apply(&ops, &HashMap::new(), &|_| None);
        let px = |x: u32, y: u32| c.pixmap().pixel(x, y).unwrap();
        assert_eq!((px(5, 5).red(), px(5, 5).blue(), px(5, 5).alpha()), (255, 0, 255));
        assert_eq!((px(15, 15).blue(), px(15, 15).red()), (255, 0));
        assert_eq!(px(19, 0).alpha(), 0);
        assert!(c.data_url().starts_with("data:image/png;base64,iVBOR"));
        assert_eq!(base64(b"Man"), "TWFu");
        // drawImage, scaled into a clipped rounded rect.
        let mut src = Pixmap::new(2, 2).unwrap();
        src.fill(Color::from_rgba8(0, 255, 0, 255));
        let src = Arc::new(src);
        let mut d = Canvas::new(20, 20).unwrap();
        let ops = vec![
            vec![s("save")],
            vec![s("translate"), Arg::N(10.0), Arg::N(10.0)],
            vec![s("scale"), Arg::N(2.0), Arg::N(2.0)],
            vec![s("beginPath")],
            vec![s("roundRect"), Arg::N(-4.0), Arg::N(-4.0), Arg::N(8.0), Arg::N(8.0), Arg::N(1.5)],
            vec![s("clip")],
            vec![s("drawImage"), s("x.png"), Arg::N(-4.0), Arg::N(-4.0), Arg::N(8.0), Arg::N(8.0)],
            vec![s("restore")],
        ];
        d.apply(&ops, &HashMap::new(), &|_| Some(src.clone()));
        let p = d.pixmap().pixel(10, 10).unwrap();
        assert_eq!((p.green(), p.alpha()), (255, 255), "image drawn");
        assert_eq!(d.pixmap().pixel(0, 0).unwrap().alpha(), 0, "outside the clip");
        assert_eq!(base64(b"Ma"), "TWE=");
    }
}
