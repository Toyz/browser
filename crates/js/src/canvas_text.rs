//! Text on a 2D canvas (`fillText`, `strokeText`, `measureText`): the
//! context's `font` resolved to a system face, each character's outline laid
//! along the baseline by its advance. Characters a face lacks come from a
//! fallback face. No shaping beyond that (no ligatures, kerning or
//! right-to-left reordering).

use std::cell::RefCell;
use std::sync::Arc;

use fontdb::{Database, Family, Query, Stretch, Style, Weight};

/// A parsed CSS `font` shorthand.
#[derive(Debug, Clone, PartialEq)]
pub struct FontSpec {
    pub size: f32,
    pub weight: u16,
    pub italic: bool,
    pub families: Vec<String>,
}

impl FontSpec {
    /// `"italic bold 12px/30px Georgia, serif"`; `None` if it isn't a font.
    pub fn parse(font: &str) -> Option<FontSpec> {
        let font = font.trim();
        let (mut weight, mut italic) = (400u16, false);
        let mut rest = font;
        let mut size = None;
        // Leading keywords up to the size.
        while !rest.is_empty() {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let word = &rest[..end];
            let lower = word.to_ascii_lowercase();
            match lower.as_str() {
                "italic" | "oblique" => italic = true,
                "bold" | "bolder" => weight = 700,
                "lighter" => weight = 300,
                "normal" | "small-caps" | "condensed" | "expanded" | "semi-condensed" | "semi-expanded" => {}
                w if w.len() == 3 && w.chars().all(|c| c.is_ascii_digit()) => weight = w.parse().unwrap_or(400),
                _ => {
                    // The size (and an optional `/line-height`).
                    let size_part = word.split('/').next().unwrap_or(word);
                    size = parse_size(size_part);
                    rest = rest[end..].trim_start();
                    break;
                }
            }
            rest = rest[end..].trim_start();
        }
        let size = size?;
        // (A line height given apart: `12px / 30px`.)
        if let Some(after) = rest.strip_prefix('/') {
            let after = after.trim_start();
            rest = after[after.find(char::is_whitespace).unwrap_or(after.len())..].trim_start();
        }
        let families: Vec<String> = rest.split(',').map(|f| f.trim().trim_matches(|c| c == '"' || c == '\'').to_owned()).filter(|f| !f.is_empty()).collect();
        if families.is_empty() {
            return None;
        }
        Some(FontSpec { size, weight, italic, families })
    }
}

fn parse_size(s: &str) -> Option<f32> {
    let s = s.to_ascii_lowercase();
    let (num, factor) = if let Some(n) = s.strip_suffix("px") {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix("pt") {
        (n, 4.0 / 3.0)
    } else if let Some(n) = s.strip_suffix("rem") {
        (n, 16.0)
    } else if let Some(n) = s.strip_suffix("em") {
        (n, 10.0)
    } else if let Some(n) = s.strip_suffix('%') {
        (n, 0.1)
    } else {
        return None;
    };
    num.parse::<f32>().ok().filter(|v| v.is_finite() && *v >= 0.0).map(|v| v * factor)
}

thread_local! {
    /// The system's fonts (read the first time a canvas draws text).
    static FONTS: RefCell<Option<Database>> = const { RefCell::new(None) };
    /// Loaded font files, by face.
    static FACES: RefCell<std::collections::HashMap<fontdb::ID, Option<(Arc<Vec<u8>>, u32)>>> = RefCell::default();
}

fn with_db<T>(f: impl FnOnce(&Database) -> T) -> T {
    FONTS.with_borrow_mut(|db| {
        let db = db.get_or_insert_with(|| {
            let mut db = Database::new();
            db.load_system_fonts();
            db.set_sans_serif_family("Helvetica");
            db.set_serif_family("Times");
            db.set_monospace_family("Menlo");
            db.set_cursive_family("Apple Chancery");
            db.set_fantasy_family("Papyrus");
            db
        });
        f(db)
    })
}

/// The bytes of a face (and its index in a collection).
fn face_data(id: fontdb::ID) -> Option<(Arc<Vec<u8>>, u32)> {
    if let Some(hit) = FACES.with_borrow(|m| m.get(&id).cloned()) {
        return hit;
    }
    let data = with_db(|db| db.with_face_data(id, |data, index| (Arc::new(data.to_vec()), index)));
    FACES.with_borrow_mut(|m| m.insert(id, data.clone()));
    data
}

/// The face for `spec` (its first family the system has, else sans-serif).
fn face_for(spec: &FontSpec) -> Option<fontdb::ID> {
    let style = if spec.italic { Style::Italic } else { Style::Normal };
    with_db(|db| {
        let lookup = |family: Family| db.query(&Query { families: &[family], weight: Weight(spec.weight), stretch: Stretch::Normal, style });
        for name in &spec.families {
            let family = match name.to_ascii_lowercase().as_str() {
                "sans-serif" | "system-ui" | "-apple-system" | "blinkmacsystemfont" | "ui-sans-serif" => Family::SansSerif,
                "serif" | "ui-serif" => Family::Serif,
                "monospace" | "ui-monospace" => Family::Monospace,
                "cursive" => Family::Cursive,
                "fantasy" => Family::Fantasy,
                _ => Family::Name(name),
            };
            if let Some(id) = lookup(family) {
                return Some(id);
            }
        }
        lookup(Family::SansSerif)
    })
}

/// Faces for characters the first lacks (CJK, symbols).
const FALLBACKS: &[&str] = &["Arial Unicode MS", "PingFang SC", "Hiragino Sans", "Apple Symbols", "Helvetica"];

/// One laid-out glyph: its face, glyph id, and pen position (px, from the
/// start of the run).
struct Placed {
    face: fontdb::ID,
    glyph: ttf_parser::GlyphId,
    x: f32,
}

/// Text laid out in a font: glyphs, total advance, and the primary face's
/// ascent and descent (px, descent negative).
pub struct Run {
    glyphs: Vec<Placed>,
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    size: f32,
}

/// Lay `text` out in `spec`.
pub fn layout(spec: &FontSpec, text: &str) -> Option<Run> {
    let primary = face_for(spec)?;
    let (data, index) = face_data(primary)?;
    let face = ttf_parser::Face::parse(&data, index).ok()?;
    let scale = spec.size / face.units_per_em() as f32;
    let (ascent, descent) = (face.ascender() as f32 * scale, face.descender() as f32 * scale);
    let mut fallback: Vec<(fontdb::ID, Arc<Vec<u8>>, u32)> = Vec::new();
    let mut glyphs = Vec::new();
    let mut x = 0.0f32;
    for ch in text.chars() {
        // (Line breaks and tabs are spaces on a canvas.)
        let ch = if matches!(ch, '\n' | '\r' | '\t' | '\x0c') { ' ' } else { ch };
        if let Some(g) = face.glyph_index(ch) {
            glyphs.push(Placed { face: primary, glyph: g, x });
            x += face.glyph_hor_advance(g).unwrap_or(0) as f32 * scale;
            continue;
        }
        if fallback.is_empty() {
            for name in FALLBACKS {
                let spec = FontSpec { families: vec![(*name).to_owned()], ..spec.clone() };
                if let Some(id) = face_for(&spec).filter(|id| *id != primary)
                    && let Some((d, i)) = face_data(id)
                {
                    fallback.push((id, d, i));
                }
            }
        }
        let found = fallback.iter().find_map(|(id, d, i)| {
            let f = ttf_parser::Face::parse(d, *i).ok()?;
            let g = f.glyph_index(ch)?;
            Some((*id, g, f.glyph_hor_advance(g).unwrap_or(0) as f32 * spec.size / f.units_per_em() as f32))
        });
        match found {
            Some((id, g, advance)) => {
                glyphs.push(Placed { face: id, glyph: g, x });
                x += advance;
            }
            // Nothing has it: the primary face's missing glyph.
            None => {
                let g = ttf_parser::GlyphId(0);
                glyphs.push(Placed { face: primary, glyph: g, x });
                x += face.glyph_hor_advance(g).unwrap_or(0) as f32 * scale;
            }
        }
    }
    Some(Run { glyphs, width: x, ascent, descent, size: spec.size })
}

/// Outlines of a run, each point given in run space (x right from the pen
/// start, y down from the baseline) to `emit`.
pub fn outline(run: &Run, h_scale: f32, mut sink: impl OutlineSink) {
    let mut faces: std::collections::HashMap<fontdb::ID, (Arc<Vec<u8>>, u32)> = std::collections::HashMap::new();
    for g in &run.glyphs {
        let entry = match faces.get(&g.face) {
            Some(e) => e.clone(),
            None => {
                let Some(e) = face_data(g.face) else { continue };
                faces.insert(g.face, e.clone());
                e
            }
        };
        let Ok(face) = ttf_parser::Face::parse(&entry.0, entry.1) else { continue };
        let scale = run.size / face.units_per_em() as f32;
        let mut builder = Builder { sink: &mut sink, ox: g.x * h_scale, sx: scale * h_scale, sy: scale };
        face.outline_glyph(g.glyph, &mut builder);
    }
}

/// Where outline points go (already in run space).
pub trait OutlineSink {
    fn move_to(&mut self, x: f32, y: f32);
    fn line_to(&mut self, x: f32, y: f32);
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32);
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32);
    fn close(&mut self);
}

struct Builder<'a, S: OutlineSink> {
    sink: &'a mut S,
    ox: f32,
    sx: f32,
    sy: f32,
}

impl<S: OutlineSink> Builder<'_, S> {
    fn p(&self, x: f32, y: f32) -> (f32, f32) {
        (self.ox + x * self.sx, -y * self.sy)
    }
}

impl<S: OutlineSink> ttf_parser::OutlineBuilder for Builder<'_, S> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.p(x, y);
        self.sink.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.p(x, y);
        self.sink.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let ((x1, y1), (x, y)) = (self.p(x1, y1), self.p(x, y));
        self.sink.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let ((x1, y1), (x2, y2), (x, y)) = (self.p(x1, y1), self.p(x2, y2), self.p(x, y));
        self.sink.curve_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.sink.close();
    }
}

/// How far the run's start is moved for `textAlign` (ltr), and its baseline
/// for `textBaseline`, from the point given.
pub fn anchor(run: &Run, width: f32, align: &str, baseline: &str, rtl: bool) -> (f32, f32) {
    let dx = match (align, rtl) {
        ("center", _) => -width / 2.0,
        ("right", _) | ("end", false) | ("start", true) => -width,
        _ => 0.0,
    };
    let dy = match baseline {
        "top" => run.ascent,
        "hanging" => run.ascent * 0.8,
        "middle" => (run.ascent + run.descent) / 2.0,
        "bottom" | "ideographic" => run.descent,
        _ => 0.0,
    };
    (dx, dy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_shorthand() {
        let f = FontSpec::parse("italic bold 12px/30px Georgia, 'Times New Roman', serif").unwrap();
        assert_eq!((f.size, f.weight, f.italic), (12.0, 700, true));
        assert_eq!(f.families, vec!["Georgia", "Times New Roman", "serif"]);
        assert_eq!(FontSpec::parse("10px sans-serif").unwrap().size, 10.0);
        assert_eq!(FontSpec::parse("500 1.5em Arial").unwrap().size, 15.0);
        assert!(FontSpec::parse("bold").is_none());
    }
}
