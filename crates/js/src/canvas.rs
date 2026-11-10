//! `<canvas>` 2D drawing: the page's context (`context2d.rs`) records
//! operations; before each snapshot they're replayed here onto a tiny-skia
//! pixmap, whose pixels the tab shows.
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

/// Where `drawImage` draws from.
#[derive(Debug, Clone)]
pub enum Source {
    /// An image, by address (decoded from the cache).
    Url(String),
    /// Another canvas (its key: `node_to_number`).
    Canvas(u64),
}

/// One recorded drawing operation.
#[derive(Clone)]
pub enum Op {
    /// The canvas was resized (or had its size set): blank, default state.
    Reset,
    Save,
    Restore,
    SetTransform([f32; 6]),
    Transform([f32; 6]),
    FillStyle(Fill),
    StrokeStyle(Fill),
    LineWidth(f32),
    Alpha(f32),
    Cap(LineCap),
    Join(LineJoin),
    Miter(f32),
    Dash(Vec<f32>),
    DashOffset(f32),
    Blend(BlendMode),
    BeginPath,
    MoveTo(f32, f32),
    LineTo(f32, f32),
    ClosePath,
    Quad([f32; 4]),
    Bezier([f32; 6]),
    /// x, y, rx, ry, rotation, start, end; anticlockwise.
    Ellipse([f32; 7], bool),
    Rect([f32; 4]),
    RoundRect([f32; 4], f32),
    Fill(FillRule),
    Stroke,
    Clip,
    FillRect([f32; 4]),
    StrokeRect([f32; 4]),
    ClearRect([f32; 4]),
    /// From `source`: (dx, dy), (dx, dy, dw, dh) or (sx, sy, sw, sh, dx, dy, dw, dh).
    DrawImage(Source, Vec<f32>),
    /// `putImageData`: unpremultiplied RGBA, `w` x `h`, at (x, y).
    PutPixels { x: i32, y: i32, w: u32, h: u32, rgba: Vec<u8> },
}

impl Op {
    /// A `globalCompositeOperation`.
    pub fn blend(name: &str) -> Option<BlendMode> {
        Some(match name {
            "source-over" => BlendMode::SourceOver,
            "destination-out" => BlendMode::DestinationOut,
            "destination-over" => BlendMode::DestinationOver,
            "source-in" => BlendMode::SourceIn,
            "source-out" => BlendMode::SourceOut,
            "source-atop" => BlendMode::SourceAtop,
            "destination-in" => BlendMode::DestinationIn,
            "destination-atop" => BlendMode::DestinationAtop,
            "lighter" => BlendMode::Plus,
            "multiply" => BlendMode::Multiply,
            "screen" => BlendMode::Screen,
            "overlay" => BlendMode::Overlay,
            "darken" => BlendMode::Darken,
            "lighten" => BlendMode::Lighten,
            "xor" => BlendMode::Xor,
            "copy" => BlendMode::Source,
            _ => return None,
        })
    }

    pub fn cap(name: &str) -> Option<LineCap> {
        Some(match name {
            "butt" => LineCap::Butt,
            "round" => LineCap::Round,
            "square" => LineCap::Square,
            _ => return None,
        })
    }

    pub fn join(name: &str) -> Option<LineJoin> {
        Some(match name {
            "miter" => LineJoin::Miter,
            "round" => LineJoin::Round,
            "bevel" => LineJoin::Bevel,
            _ => return None,
        })
    }
}

/// A fill or stroke style.
#[derive(Clone)]
pub enum Fill {
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

    /// The pixels, premultiplied RGBA.
    pub fn pixels(&self) -> &[u8] {
        self.pixmap.data()
    }

    /// The pixels as a PNG `data:` URL (`canvas.toDataURL()`).
    pub fn data_url(&mut self) -> Arc<str> {
        if let Some(url) = &self.url {
            return url.clone();
        }
        let png = self.pixmap.encode_png().unwrap_or_default();
        let url: Arc<str> = format!("data:image/png;base64,{}", base64(&png)).into();
        self.url = Some(url.clone());
        url
    }

    /// Run recorded operations. `others` holds the page's other
    /// canvases, for drawImage(canvas).
    pub fn apply(&mut self, ops: &[Op], others: &HashMap<u64, Pixmap>, images: &dyn Fn(&str) -> Option<Arc<Pixmap>>) {
        if !ops.is_empty() {
            self.url = None;
        }
        for op in ops {
            match op {
                Op::Reset => {
                    self.pixmap.fill(Color::TRANSPARENT);
                    self.state = State::default();
                    self.stack.clear();
                    self.path = PathBuilder::new();
                    self.started = false;
                }
                Op::Save => self.stack.push(self.state.clone()),
                Op::Restore => {
                    if let Some(s) = self.stack.pop() {
                        self.state = s;
                    }
                }
                &Op::SetTransform([a, b, c, d, e, f]) => self.state.transform = Transform::from_row(a, b, c, d, e, f),
                &Op::Transform([a, b, c, d, e, f]) => self.state.transform = self.state.transform.pre_concat(Transform::from_row(a, b, c, d, e, f)),
                Op::FillStyle(f) => self.state.fill = f.clone(),
                Op::StrokeStyle(f) => self.state.stroke = f.clone(),
                &Op::LineWidth(w) => self.state.line_width = w,
                &Op::Alpha(a) => self.state.alpha = a,
                &Op::Cap(c) => self.state.cap = c,
                &Op::Join(j) => self.state.join = j,
                &Op::Miter(m) => self.state.miter = m,
                Op::Dash(d) => self.state.dash = d.clone(),
                &Op::DashOffset(o) => self.state.dash_offset = o,
                &Op::Blend(b) => self.state.blend = b,
                Op::BeginPath => {
                    self.path = PathBuilder::new();
                    self.started = false;
                }
                &Op::MoveTo(x, y) => {
                    let p = self.pt(x, y);
                    self.path.move_to(p.x, p.y);
                    self.started = true;
                }
                &Op::LineTo(x, y) => {
                    let p = self.pt(x, y);
                    self.line_to(p);
                }
                Op::ClosePath => {
                    if self.started {
                        self.path.close();
                    }
                }
                &Op::Quad([cx, cy, x, y]) => {
                    let (c, p) = (self.pt(cx, cy), self.pt(x, y));
                    self.ensure_start(c);
                    self.path.quad_to(c.x, c.y, p.x, p.y);
                }
                &Op::Bezier([c1x, c1y, c2x, c2y, x, y]) => {
                    let (c1, c2, p) = (self.pt(c1x, c1y), self.pt(c2x, c2y), self.pt(x, y));
                    self.ensure_start(c1);
                    self.path.cubic_to(c1.x, c1.y, c2.x, c2.y, p.x, p.y);
                }
                &Op::Ellipse([x, y, rx, ry, rotation, start, end], ccw) => self.ellipse(x, y, rx, ry, rotation, start, end, ccw),
                &Op::Rect([x, y, w, h]) => self.rect(x, y, w, h),
                &Op::RoundRect([x, y, w, h], r) => self.round_rect(x, y, w, h, r.min(w.abs() / 2.0).min(h.abs() / 2.0)),
                &Op::Fill(rule) => {
                    if let Some(path) = self.path.clone().finish() {
                        self.fill_path(&path, rule);
                    }
                }
                Op::Stroke => {
                    if let Some(path) = self.path.clone().finish() {
                        self.stroke_path(&path);
                    }
                }
                Op::Clip => {
                    if let Some(path) = self.path.clone().finish() {
                        let mut mask = Mask::new(self.pixmap.width(), self.pixmap.height()).expect("mask");
                        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
                        if let Some(old) = &self.state.clip {
                            // Intersect: keep where both are set.
                            for (m, o) in mask.data_mut().iter_mut().zip(old.data()) {
                                *m = ((*m as u16 * *o as u16) / 255) as u8;
                            }
                        }
                        self.state.clip = Some(mask);
                    }
                }
                Op::FillRect(r) | Op::StrokeRect(r) | Op::ClearRect(r) => {
                    let [x, y, w, h] = *r;
                    let mut pb = PathBuilder::new();
                    for (i, (x, y)) in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)].into_iter().enumerate() {
                        let p = self.pt(x, y);
                        if i == 0 { pb.move_to(p.x, p.y) } else { pb.line_to(p.x, p.y) }
                    }
                    pb.close();
                    let Some(path) = pb.finish() else { continue };
                    match op {
                        Op::FillRect(_) => self.fill_path(&path, FillRule::Winding),
                        Op::StrokeRect(_) => self.stroke_path(&path),
                        _ => {
                            let paint = Paint { blend_mode: BlendMode::Clear, ..Paint::default() };
                            self.pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), self.state.clip.as_ref());
                        }
                    }
                }
                Op::DrawImage(source, rest) => {
                    let src: Option<Arc<Pixmap>> = match source {
                        Source::Canvas(key) => others.get(key).map(|p| Arc::new(p.clone())),
                        Source::Url(url) => images(url),
                    };
                    let Some(src) = src else { continue };
                    let (iw, ih) = (src.width() as f32, src.height() as f32);
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
                Op::PutPixels { x, y, w, h, rgba } => {
                    let Some(size) = tiny_skia::IntSize::from_wh(*w, *h) else { continue };
                    let Some(mut src) = Pixmap::from_vec(rgba.clone(), size) else { continue };
                    premultiply(src.data_mut());
                    // Replaces what's there, ignoring transform, alpha and clip.
                    let paint = tiny_skia::PixmapPaint { blend_mode: BlendMode::Source, ..Default::default() };
                    self.pixmap.draw_pixmap(*x, *y, src.as_ref(), &paint, Transform::identity(), None);
                }
            }
        }
    }

    /// `getImageData`: the pixels of a region, unpremultiplied RGBA (outside
    /// the canvas: transparent).
    pub fn image_data(&self, x: i32, y: i32, w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![0u8; w as usize * h as usize * 4];
        let (cw, ch) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let data = self.pixmap.data();
        for row in 0..h as i32 {
            let sy = y + row;
            if !(0..ch).contains(&sy) {
                continue;
            }
            for col in 0..w as i32 {
                let sx = x + col;
                if !(0..cw).contains(&sx) {
                    continue;
                }
                let s = ((sy * cw + sx) * 4) as usize;
                let d = ((row * w as i32 + col) * 4) as usize;
                let a = data[s + 3];
                let un = |c: u8| if a == 0 { 0 } else { ((c as u16 * 255 + a as u16 / 2) / a as u16).min(255) as u8 };
                out[d..d + 4].copy_from_slice(&[un(data[s]), un(data[s + 1]), un(data[s + 2]), a]);
            }
        }
        out
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
/// A CSS color as a fill (`None`: not a color, and ignored).
pub fn color_fill(text: &str) -> Option<Fill> {
    css_color(text).map(Fill::Color)
}

/// A canvas gradient as a fill: `linear` (x0, y0, x1, y1) or `radial` (x0,
/// y0, r0, x1, y1, r1), with its (offset, CSS color) stops.
pub fn gradient_fill(radial: bool, at: &[f32], stops: &[(f32, String)]) -> Fill {
    let n = |i: usize| at.get(i).copied().unwrap_or(0.0);
    let stops: Vec<(f32, Color)> = stops.iter().filter_map(|(o, c)| Some((o.clamp(0.0, 1.0), css_color(c)?))).collect();
    let fill = if radial {
        single_or(stops, |stops| Fill::Radial { from: Point::from_xy(n(0), n(1)), to: Point::from_xy(n(3), n(4)), radius: n(5).max(0.001), stops })
    } else {
        single_or(stops, |stops| Fill::Linear { from: Point::from_xy(n(0), n(1)), to: Point::from_xy(n(2), n(3)), stops })
    };
    fill.unwrap_or(Fill::Color(Color::TRANSPARENT))
}

/// Unpremultiplied RGBA to premultiplied, in place.
fn premultiply(data: &mut [u8]) {
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u16;
        for c in &mut px[..3] {
            *c = ((*c as u16 * a + 127) / 255) as u8;
        }
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
/// A picture to draw on a canvas: its frames (one, unless it's animated)
/// and how long each shows, ms.
pub struct Picture {
    frames: Vec<(Arc<Pixmap>, u32)>,
    total_ms: u32,
    started: std::time::Instant,
}

impl Picture {
    /// The frame showing now (animations run from when first decoded,
    /// as a browser's do from load).
    pub fn now(&self) -> Arc<Pixmap> {
        if self.frames.len() == 1 || self.total_ms == 0 {
            return self.frames[0].0.clone();
        }
        let mut t = (self.started.elapsed().as_millis() % self.total_ms as u128) as u32;
        for (frame, ms) in &self.frames {
            if t < *ms {
                return frame.clone();
            }
            t -= ms;
        }
        self.frames[0].0.clone()
    }
}

/// Decode a picture for drawing: every frame of a GIF or animated WebP.
pub fn decode_picture(bytes: &[u8]) -> Option<Picture> {
    use image::AnimationDecoder;
    use std::io::Cursor;
    let frames: Option<Vec<image::Frame>> = if is_avif(bytes) {
        // AVIF: through libavif (still or animated).
        avif::decode(bytes)
    } else {
        match image::guess_format(bytes).ok()? {
        image::ImageFormat::Gif => image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).ok()?.into_frames().collect_frames().ok(),
        image::ImageFormat::WebP => {
            let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).ok()?;
            if decoder.has_animation() { decoder.into_frames().collect_frames().ok() } else { None }
        }
        _ => None,
        }
    };
    let frames: Vec<(Arc<Pixmap>, u32)> = match frames.filter(|f| f.len() > 1 || is_avif(bytes)) {
        Some(frames) => frames
            .into_iter()
            .filter_map(|f| {
                let (num, den) = f.delay().numer_denom_ms();
                let ms = (num / den.max(1)).max(20);
                // (libavif's frames are BGRA, as GPUI wants them.)
                let mut buffer = f.into_buffer();
                if is_avif(bytes) {
                    for px in buffer.pixels_mut() {
                        px.0.swap(0, 2);
                    }
                }
                Some((Arc::new(premultiplied(buffer)?), ms))
            })
            .collect(),
        None => vec![(Arc::new(decode_image(bytes)?), 0)],
    };
    if frames.is_empty() {
        return None;
    }
    let total_ms = frames.iter().map(|(_, ms)| ms).sum();
    Some(Picture { frames, total_ms, started: std::time::Instant::now() })
}

/// An AVIF file (`ftyp` box with an `avif`/`avis` brand)?
fn is_avif(bytes: &[u8]) -> bool {
    bytes.len() > 12 && &bytes[4..8] == b"ftyp" && matches!(&bytes[8..12], b"avif" | b"avis")
}

pub fn decode_image(bytes: &[u8]) -> Option<Pixmap> {
    premultiplied(image::load_from_memory(bytes).ok()?.to_rgba8())
}

/// Straight RGBA as tiny-skia's premultiplied pixels.
fn premultiplied(img: image::RgbaImage) -> Option<Pixmap> {
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
        let red = || color_fill("red").unwrap();
        let ops = vec![
            Op::FillStyle(red()),
            Op::FillRect([0.0, 0.0, 10.0, 10.0]),
            Op::BeginPath,
            Op::FillStyle(color_fill("#00f").unwrap()),
            Op::Ellipse([15.0, 15.0, 4.0, 4.0, 0.0, 0.0, 6.3], false),
            Op::Fill(FillRule::Winding),
        ];
        c.apply(&ops, &HashMap::new(), &|_| None);
        let px = |x: u32, y: u32| c.pixmap().pixel(x, y).unwrap();
        assert_eq!((px(5, 5).red(), px(5, 5).blue(), px(5, 5).alpha()), (255, 0, 255));
        assert_eq!((px(15, 15).blue(), px(15, 15).red()), (255, 0));
        assert_eq!(px(19, 0).alpha(), 0);
        assert_eq!(c.image_data(4, 4, 1, 1), vec![255, 0, 0, 255]);
        assert!(c.data_url().starts_with("data:image/png;base64,iVBOR"));
        assert_eq!(base64(b"Man"), "TWFu");
        // putImageData replaces; Reset blanks.
        c.apply(&[Op::PutPixels { x: 0, y: 0, w: 1, h: 1, rgba: vec![0, 255, 0, 128] }], &HashMap::new(), &|_| None);
        assert_eq!(c.image_data(0, 0, 1, 1), vec![0, 255, 0, 128]);
        c.apply(&[Op::Reset], &HashMap::new(), &|_| None);
        assert_eq!(c.image_data(5, 5, 1, 1), vec![0, 0, 0, 0]);
        // drawImage, scaled into a clipped rounded rect.
        let mut src = Pixmap::new(2, 2).unwrap();
        src.fill(Color::from_rgba8(0, 255, 0, 255));
        let src = Arc::new(src);
        let mut d = Canvas::new(20, 20).unwrap();
        let ops = vec![
            Op::Save,
            Op::Transform([1.0, 0.0, 0.0, 1.0, 10.0, 10.0]),
            Op::Transform([2.0, 0.0, 0.0, 2.0, 0.0, 0.0]),
            Op::BeginPath,
            Op::RoundRect([-4.0, -4.0, 8.0, 8.0], 1.5),
            Op::Clip,
            Op::DrawImage(Source::Url("x.png".into()), vec![-4.0, -4.0, 8.0, 8.0]),
            Op::Restore,
        ];
        d.apply(&ops, &HashMap::new(), &|_| Some(src.clone()));
        let p = d.pixmap().pixel(10, 10).unwrap();
        assert_eq!((p.green(), p.alpha()), (255, 255), "image drawn");
        assert_eq!(d.pixmap().pixel(0, 0).unwrap().alpha(), 0, "outside the clip");
        assert_eq!(base64(b"Ma"), "TWE=");
    }
}
