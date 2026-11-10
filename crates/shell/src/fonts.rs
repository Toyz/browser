//! Web fonts (`@font-face`): fetched after the page shows (it renders with
//! fallback fonts first, like `font-display: swap`), decoded from WOFF/WOFF2,
//! and registered with GPUI's text system under the family name inside the
//! font file. The page's CSS name for it becomes an alias.

use std::collections::HashMap;

use gpui_kit::{App, Global};

/// Fonts larger than this aren't worth the wait.
const MAX_FONT_BYTES: usize = 4 << 20;
/// At most this many font files per page.
pub const MAX_FONTS_PER_PAGE: usize = 16;

/// Font files already registered (app-wide): URL → family in the file.
#[derive(Default)]
pub struct WebFonts {
    pub loaded: HashMap<String, Option<String>>,
}

impl Global for WebFonts {}

pub fn init(cx: &mut App) {
    cx.set_global(WebFonts::default());
}

/// The best source of an `@font-face` (formats we can decode, smallest first).
pub fn pick_source(face: &style::FontFace) -> Option<&str> {
    let rank = |url: &str, format: &Option<String>| -> u8 {
        let hint = format.as_deref().unwrap_or("");
        let url = url.to_ascii_lowercase();
        if hint.contains("woff2") || url.contains(".woff2") {
            0
        } else if hint.contains("woff") || url.contains(".woff") {
            1
        } else if hint.contains("truetype") || hint.contains("opentype") || url.contains(".ttf") || url.contains(".otf") {
            2
        } else if hint.is_empty() {
            3
        } else {
            9 // embedded-opentype, svg
        }
    };
    face.sources
        .iter()
        .filter(|(url, format)| rank(url, format) < 9)
        .min_by_key(|(url, format)| rank(url, format))
        .map(|(url, _)| &**url)
}

/// Weights cut from a variable font (those within its range).
const INSTANCE_WEIGHTS: [u16; 6] = [300, 400, 500, 600, 700, 800];

/// Decode a downloaded font into fonts GPUI can use, with their family
/// name: the font itself, or for a variable font, a static instance per
/// common weight (GPUI can't set variation axes, so a variable font would
/// draw every weight as its default).
/// `css_family`: the `@font-face` family it's for. Every face of a page's
/// family is registered under that name (with its own weight and style),
/// so the platform picks among them as CSS does: a file's own name may be
/// per weight (Open Sans ships "Open Sans Light", "Open Sans SemiBold"...,
/// and the family named the last one registered: the Rust book all bold),
/// or nothing readable (BBC's twenty Reith files, sans and serif, upright
/// and italic, are all ".\u{7f}": headlines in italic serif).
pub fn decode_all(bytes: Vec<u8>, weights: (u16, u16), css_family: Option<&str>) -> Vec<(Vec<u8>, String)> {
    let Some((data, family)) = decode(bytes) else { return Vec::new() };
    let family = family.filter(|f| f.chars().any(char::is_alphabetic));
    let css = css_family.map(str::trim).filter(|f| !f.is_empty());
    let variable = ttf_parser::Face::parse(&data, 0).is_ok_and(|f| f.is_variable());
    let (data, family) = match (css, family) {
        // (Variable fonts get their names with each instance below.)
        (Some(css), _) if variable => (data, css.to_owned()),
        (Some(css), family) => match renamed(&data, css) {
            Some(data) => (data, css.to_owned()),
            None => match family {
                Some(family) => (data, family),
                None => return Vec::new(),
            },
        },
        (None, Some(family)) => (data, family),
        (None, None) => return Vec::new(),
    };
    let fonts = if !variable {
        vec![(data, family)]
    } else {
        let (lo, hi) = (weights.0.min(weights.1), weights.0.max(weights.1));
        let instances: Vec<(Vec<u8>, String)> = INSTANCE_WEIGHTS
            .iter()
            .filter(|w| (lo..=hi).contains(*w))
            .filter_map(|&w| instance(&data, w, &family).map(|d| (d, family.clone())))
            .collect();
        if instances.is_empty() { vec![(data, family)] } else { instances }
    };
    // Every font handed to the platform has a family name it can read
    // (CoreText's font-kit binding aborts the app on one without).
    fonts
        .into_iter()
        .filter_map(|(data, family)| {
            if has_family_name(&data) {
                Some((data, family))
            } else {
                renamed(&data, &family).filter(|d| has_family_name(d)).map(|d| (d, family))
            }
        })
        .collect()
}

/// Does the font parse, with a readable family name (name ID 1 or 16)?
fn has_family_name(data: &[u8]) -> bool {
    let Ok(face) = ttf_parser::Face::parse(data, 0) else { return false };
    face.names().into_iter().any(|n| matches!(n.name_id, 1 | 16) && n.to_string().is_some_and(|s| !s.trim().is_empty()))
}

/// A static instance of a variable font at `weight` (other axes at default).
fn instance(data: &[u8], weight: u16, family: &str) -> Option<Vec<u8>> {
    use allsorts::binary::read::ReadScope;
    use allsorts::font_data::FontData;
    use allsorts::tables::variable_fonts::fvar::FvarTable;
    use allsorts::tables::{Fixed, FontTableProvider as _};
    let file = ReadScope::new(data).read::<FontData<'_>>().ok()?;
    let provider = file.table_provider(0).ok()?;
    let fvar_data = provider.read_table_data(allsorts::tag::FVAR).ok()?;
    let fvar = ReadScope::new(&fvar_data).read::<FvarTable<'_>>().ok()?;
    let wght = u32::from_be_bytes(*b"wght");
    let coords: Vec<Fixed> = fvar
        .axes()
        .map(|axis| if axis.axis_tag == wght { Fixed::from(weight as f32) } else { axis.default_value })
        .collect();
    let (mut bytes, _) = allsorts::variations::instance(&provider, &coords).ok()?;
    // allsorts writes Apple's `true` sfnt tag; font-kit (GPUI's loader) only
    // takes 0x00010000 for TrueType and reads anything else as a resource fork.
    if bytes.starts_with(b"true") {
        bytes[..4].copy_from_slice(&[0, 1, 0, 0]);
    }
    // Every instance still carries the variable font's names; CoreText and
    // font-kit key fonts by PostScript name, so give each its own.
    let family = family.to_owned();
    let italic = ttf_parser::Face::parse(&bytes, 0).ok()?.is_italic();
    let style = format!("{}{}", weight_name(weight), if italic { " Italic" } else { "" });
    let postscript: String = format!("{family}-{style}").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let name = name_table(&[
        (1, family.clone()),
        (2, if italic { "Italic".into() } else { "Regular".into() }),
        (4, format!("{family} {style}")),
        (6, postscript),
        (16, family),
        (17, style),
    ]);
    replace_table(&bytes, *b"name", &name)
}

/// The font with a fresh `name` table (Windows and Mac records for its
/// family and style): for files whose names the platform can't read.
pub fn renamed(data: &[u8], family: &str) -> Option<Vec<u8>> {
    use std::hash::{Hash, Hasher};
    let face = ttf_parser::Face::parse(data, 0).ok()?;
    let (weight, italic) = (face.weight().to_number(), face.is_italic());
    let style = format!("{}{}", weight_name(weight), if italic { " Italic" } else { "" });
    // Unique per file: fonts are keyed by PostScript name, and a family's
    // faces of one weight split by `unicode-range` would be taken for one.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut h);
    let tag = h.finish() as u32;
    let postscript: String = format!("{family}-{style}-{tag:08x}").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let name = name_table(&[
        (1, family.to_owned()),
        (2, if italic { "Italic".into() } else { "Regular".into() }),
        (4, format!("{family} {style}")),
        (6, postscript),
        (16, family.to_owned()),
        (17, style),
    ]);
    replace_table(data, *b"name", &name)
}

fn weight_name(weight: u16) -> &'static str {
    match weight {
        0..=150 => "Thin",
        151..=250 => "ExtraLight",
        251..=350 => "Light",
        351..=450 => "Regular",
        451..=550 => "Medium",
        551..=650 => "SemiBold",
        651..=750 => "Bold",
        751..=850 => "ExtraBold",
        _ => "Black",
    }
}

/// A `name` table (format 0) with these (name id, string) records, for
/// both Windows (UTF-16BE) and Mac Roman (ASCII) platforms.
fn name_table(records: &[(u16, String)]) -> Vec<u8> {
    let mut entries: Vec<(u16, u16, u16, u16, Vec<u8>)> = Vec::new();
    for (id, text) in records {
        let ascii: Vec<u8> = text.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect();
        entries.push((1, 0, 0, *id, ascii));
        let utf16: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        entries.push((3, 1, 0x409, *id, utf16));
    }
    entries.sort_by_key(|e| (e.0, e.1, e.2, e.3));
    let count = entries.len() as u16;
    let storage_offset = 6 + 12 * count;
    let mut out = Vec::new();
    let mut storage: Vec<u8> = Vec::new();
    out.extend(0u16.to_be_bytes());
    out.extend(count.to_be_bytes());
    out.extend(storage_offset.to_be_bytes());
    for (platform, encoding, language, id, data) in &entries {
        for v in [*platform, *encoding, *language, *id, data.len() as u16, storage.len() as u16] {
            out.extend(v.to_be_bytes());
        }
        storage.extend(data);
    }
    out.extend(storage);
    out
}

/// Rebuild an sfnt font with table `tag` replaced (offsets, padding and
/// checksums recomputed).
fn replace_table(font: &[u8], tag: [u8; 4], data: &[u8]) -> Option<Vec<u8>> {
    let u16_at = |o: usize| font.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let u32_at = |o: usize| font.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let num = u16_at(4)? as usize;
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::with_capacity(num);
    for i in 0..num {
        let rec = 12 + 16 * i;
        let t: [u8; 4] = font.get(rec..rec + 4)?.try_into().ok()?;
        let (offset, length) = (u32_at(rec + 8)? as usize, u32_at(rec + 12)? as usize);
        let bytes = if t == tag { data.to_vec() } else { font.get(offset..offset + length)?.to_vec() };
        tables.push((t, bytes));
    }
    tables.sort_by_key(|(t, _)| *t);
    let checksum = |bytes: &[u8]| -> u32 {
        bytes.chunks(4).fold(0u32, |sum, c| {
            let mut w = [0u8; 4];
            w[..c.len()].copy_from_slice(c);
            sum.wrapping_add(u32::from_be_bytes(w))
        })
    };
    let n = tables.len() as u16;
    let mut entry_selector = 0u16;
    while (1u16 << (entry_selector + 1)) <= n {
        entry_selector += 1;
    }
    let search_range = (1u16 << entry_selector) * 16;
    let mut out = Vec::new();
    out.extend(&font[..4]);
    for v in [n, search_range, entry_selector, n * 16 - search_range] {
        out.extend(v.to_be_bytes());
    }
    let mut offset = 12 + 16 * tables.len();
    let mut body = Vec::new();
    let mut head_at = None;
    for (t, bytes) in &tables {
        let mut sum_bytes = bytes.clone();
        if t == b"head" && sum_bytes.len() >= 12 {
            sum_bytes[8..12].copy_from_slice(&[0; 4]); // checkSumAdjustment counts as 0
            head_at = Some(offset);
        }
        out.extend(t);
        out.extend(checksum(&sum_bytes).to_be_bytes());
        out.extend((offset as u32).to_be_bytes());
        out.extend((bytes.len() as u32).to_be_bytes());
        body.extend(&sum_bytes);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        offset = 12 + 16 * tables.len() + body.len();
    }
    out.extend(body);
    if let Some(at) = head_at {
        let adjust = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out[at + 8..at + 12].copy_from_slice(&adjust.to_be_bytes());
    }
    Some(out)
}

/// Decode a downloaded font: (sfnt data, its family name if it has one).
pub fn decode(bytes: Vec<u8>) -> Option<(Vec<u8>, Option<String>)> {
    if bytes.len() > MAX_FONT_BYTES {
        return None;
    }
    let data = if bytes.starts_with(b"wOF2") {
        wuff::decompress_woff2(&bytes).ok()?
    } else if bytes.starts_with(b"wOFF") {
        wuff::decompress_woff1(&bytes).ok()?
    } else {
        bytes
    };
    let face = ttf_parser::Face::parse(&data, 0).ok()?;
    let name = |id: u16| {
        face.names()
            .into_iter()
            .filter(|n| n.name_id == id)
            .find_map(|n| n.to_string())
    };
    // The typographic family groups weights under one name; the legacy one
    // may say "Inter Medium".
    let named = |id| name(id).filter(|n: &String| !n.trim().is_empty());
    let family = named(ttf_parser::name_id::TYPOGRAPHIC_FAMILY).or_else(|| named(ttf_parser::name_id::FAMILY));
    Some((data, family))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `FONT=file.woff2 cargo test -p shell --bin feather font_names -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn font_names() {
        let bytes = std::fs::read(std::env::var("FONT").unwrap()).unwrap();
        let (data, family) = decode(bytes).unwrap();
        let face = ttf_parser::Face::parse(&data, 0).unwrap();
        println!("family {family:?} tag {:?} tables {}", &data[..4], face.tables().name.is_some());
        for n in face.names() {
            println!("platform {:?} enc {} lang {} id {} -> {:?}", n.platform_id, n.encoding_id, n.language_id, n.name_id, n.to_string());
        }
    }

    #[test]
    fn prefers_woff2() {
        let face = style::FontFace {
            family: "X".into(),
            sources: vec![
                ("https://a/x.eot".into(), Some("embedded-opentype".into())),
                ("https://a/x.ttf".into(), Some("truetype".into())),
                ("https://a/x.woff2".into(), Some("woff2".into())),
            ],
            weight: (400, 400),
            italic: false,
        };
        assert_eq!(pick_source(&face), Some("https://a/x.woff2"));
        assert!(decode(b"not a font".to_vec()).is_none());
    }
}
