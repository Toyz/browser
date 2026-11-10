//! CSS property parsing: declaration text → typed [`Decl`]s.
//!
//! Shorthands expand into longhands here. Values that depend on context (`em`,
//! `%`, `currentColor`) stay symbolic until the cascade resolves them.
//! Declarations containing `var()` can't be parsed until the element's custom
//! properties are known, so they're kept as [`Decl::Unresolved`].

mod animation;
mod background;
mod box_model;
mod effects;
mod flex_grid;
mod grid;
pub mod meta;
mod text;

mod border;
mod color;
mod decl;
mod image;
mod layout;
mod length;
mod text_values;
mod timing;

pub use color::parse_color_str;
pub use decl::*;
pub use image::with_base;
pub(crate) use self::{border::*, color::*, image::*, layout::*, length::*, text_values::*, timing::*};

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use cssparser::{ParseError, Parser, Token};

use crate::values::*;

type PResult<'i, T> = Result<T, ParseError<()>>;

thread_local! {
    /// The URL relative `url()`s resolve against while parsing a stylesheet.
    static CSS_BASE: std::cell::RefCell<Option<url::Url>> = const { std::cell::RefCell::new(None) };
}

pub fn contains_var(value: &str) -> bool {
    value.as_bytes().windows(4).any(|w| w.eq_ignore_ascii_case(b"var("))
}

/// Parse one declaration. Invalid declarations produce nothing (CSS's error recovery).
/// SVG presentation properties, passed through to inline SVGs.
pub const SVG_PAINT: &[&str] = &["fill", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin", "fill-opacity", "stroke-opacity", "fill-rule"];

pub fn parse_declaration(name: &str, value: &str, out: &mut Vec<Decl>) {
    if name.starts_with("--") {
        out.push(Decl::Custom(name.into(), value.trim().into()));
        return;
    }
    let name = name.to_ascii_lowercase();
    if contains_var(value) {
        out.push(Decl::Unresolved { name: name.into(), raw: value.trim().into() });
        return;
    }
    let trimmed = value.trim();
    let wide = match trimmed.to_ascii_lowercase().as_str() {
        "inherit" => Some(Wide::Inherit),
        "initial" => Some(Wide::Initial),
        "unset" | "revert" | "revert-layer" => Some(Wide::Unset),
        _ => None,
    };
    if let Some(wide) = wide {
        out.push(Decl::Wide(name.into(), wide));
        return;
    }
    // SVG paint: kept as written, for the SVG renderer to read.
    if SVG_PAINT.contains(&name.as_str()) {
        out.push(Decl::SvgPaint(name.into(), trimmed.into()));
        return;
    }
    let start = out.len();
    let mut p = Parser::new(trimmed);
    let ok = parse_property(&name, &mut p, out).is_ok() && p.expect_exhausted().is_ok();
    if !ok {
        out.truncate(start);
    }
}

fn err<'i, T>(_p: &Parser<'i>) -> PResult<'i, T> {
    Err(ParseError::custom(()))
}

fn ident<'i>(p: &mut Parser<'i>) -> PResult<'i, String> {
    Ok(p.expect_ident()?.to_ascii_lowercase())
}

fn keyword<'i, T: Copy>(p: &mut Parser<'i>, table: &[(&str, T)]) -> PResult<'i, T> {
    let id = ident(p)?;
    match table.iter().find(|(k, _)| *k == id) {
        Some(&(_, v)) => Ok(v),
        None => err(p),
    }
}

/// Up to four values in TRBL shorthand order.
fn four<'i, T: Clone>(
    p: &mut Parser<'i>,
    mut f: impl FnMut(&mut Parser<'i>) -> PResult<'i, T>,
) -> PResult<'i, [T; 4]> {
    let mut v = vec![f(p)?];
    while v.len() < 4 {
        match p.try_parse(&mut f) {
            Ok(x) => v.push(x),
            Err(_) => break,
        }
    }
    Ok(match v.len() {
        1 => [v[0].clone(), v[0].clone(), v[0].clone(), v[0].clone()],
        2 => [v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()],
        3 => [v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()],
        _ => [v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()],
    })
}

/// Logical (`-inline`/`-block`, `-start`/`-end`) to physical sides, assuming LTR horizontal text.
fn logical_sides(suffix: &str) -> Option<&'static [Side]> {
    Some(match suffix {
        "inline" => &[Side::Left, Side::Right],
        "block" => &[Side::Top, Side::Bottom],
        "inline-start" | "left" => &[Side::Left],
        "inline-end" | "right" => &[Side::Right],
        "block-start" | "top" => &[Side::Top],
        "block-end" | "bottom" => &[Side::Bottom],
        _ => return None,
    })
}

fn parse_property<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    match registry().get(name) {
        Some(parse) => parse(name, p, out),
        None => {
            // Unsupported property: consume it so it isn't "invalid", just ignored.
            while p.next().is_ok() {}
            Ok(())
        }
    }
}

/// How a property's value is parsed into declarations (`name` is the
/// property as written, for parsers shared by several).
pub(crate) type ParseFn = for<'i> fn(&str, &mut Parser<'i>, &mut Vec<Decl>) -> PResult<'i, ()>;

/// Every property we understand, by name.
fn registry() -> &'static HashMap<&'static str, ParseFn> {
    static REGISTRY: OnceLock<HashMap<&'static str, ParseFn>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut map: HashMap<&'static str, ParseFn> = HashMap::new();
        for table in [
            box_model::PROPERTIES,
            text::PROPERTIES,
            background::PROPERTIES,
            flex_grid::PROPERTIES,
            grid::PROPERTIES,
            effects::PROPERTIES,
            animation::PROPERTIES,
            FAMILIES,
        ] {
            for &(name, parse) in table {
                let clash = map.insert(name, parse);
                debug_assert!(clash.is_none(), "property {name} registered twice");
            }
        }
        map
    })
}

/// Whether the registry knows `name` (for `@supports` and tests).
pub fn is_supported(name: &str) -> bool {
    registry().contains_key(name.to_ascii_lowercase().as_str())
}

/// Margins, padding and borders: a property per side, per logical side and
/// per component, all parsed by a few shared functions.
const FAMILIES: &[(&str, ParseFn)] = &[
    ("margin", p_margin), ("margin-top", p_margin), ("margin-right", p_margin), ("margin-bottom", p_margin), ("margin-left", p_margin),
    ("margin-inline", p_margin), ("margin-block", p_margin), ("margin-inline-start", p_margin), ("margin-inline-end", p_margin),
    ("margin-block-start", p_margin), ("margin-block-end", p_margin),
    ("padding", p_padding), ("padding-top", p_padding), ("padding-right", p_padding), ("padding-bottom", p_padding), ("padding-left", p_padding),
    ("padding-inline", p_padding), ("padding-block", p_padding), ("padding-inline-start", p_padding), ("padding-inline-end", p_padding),
    ("padding-block-start", p_padding), ("padding-block-end", p_padding),
    ("border", p_border), ("border-width", p_border), ("border-style", p_border), ("border-color", p_border), ("border-radius", p_border),
    ("border-top", p_border), ("border-right", p_border), ("border-bottom", p_border), ("border-left", p_border),
    ("border-top-width", p_border), ("border-right-width", p_border), ("border-bottom-width", p_border), ("border-left-width", p_border),
    ("border-top-style", p_border), ("border-right-style", p_border), ("border-bottom-style", p_border), ("border-left-style", p_border),
    ("border-top-color", p_border), ("border-right-color", p_border), ("border-bottom-color", p_border), ("border-left-color", p_border),
    ("border-top-left-radius", p_border), ("border-top-right-radius", p_border), ("border-bottom-left-radius", p_border), ("border-bottom-right-radius", p_border),
    ("border-inline", p_border), ("border-block", p_border), ("border-inline-start", p_border), ("border-inline-end", p_border),
    ("border-block-start", p_border), ("border-block-end", p_border),
    ("border-inline-width", p_border), ("border-block-width", p_border), ("border-inline-color", p_border), ("border-block-color", p_border),
    ("border-inline-style", p_border), ("border-block-style", p_border),
    ("border-inline-start-width", p_border), ("border-inline-end-width", p_border), ("border-block-start-width", p_border), ("border-block-end-width", p_border),
    ("border-inline-start-color", p_border), ("border-inline-end-color", p_border), ("border-block-start-color", p_border), ("border-block-end-color", p_border),
    ("border-inline-start-style", p_border), ("border-inline-end-style", p_border), ("border-block-start-style", p_border), ("border-block-end-style", p_border),
    ("border-start-start-radius", p_border), ("border-start-end-radius", p_border), ("border-end-start-radius", p_border), ("border-end-end-radius", p_border),
    ("border-collapse", p_border), ("border-spacing", p_border), ("border-image", p_border),
];

fn p_margin<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    box_sides("margin", Decl::Margin, name, p, out)
}

fn p_padding<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    box_sides("padding", Decl::Padding, name, p, out)
}

/// `margin`/`padding` and their per-side and logical longhands.
fn box_sides<'i>(prefix: &str, make: fn(Side, LenSpec) -> Decl, name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    if name == prefix {
        let v = four(p, parse_len)?;
        for (side, len) in Side::ALL.into_iter().zip(v) {
            out.push(make(side, len));
        }
        return Ok(());
    }
    let Some(sides) = name.strip_prefix(prefix).and_then(|s| s.strip_prefix('-')).and_then(logical_sides) else { return err(p) };
    let first = parse_len(p)?;
    let second = p.try_parse(parse_len).unwrap_or_else(|_| first.clone());
    for (i, &side) in sides.iter().enumerate() {
        out.push(make(side, if i == 0 { first.clone() } else { second.clone() }));
    }
    Ok(())
}

fn p_border<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    parse_border_inner(name.strip_prefix("border").unwrap_or(name), p, out)
}

#[cfg(test)]
mod tests;
