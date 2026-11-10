//! Times, easing functions and animation keywords.

use super::*;

/// `150ms`, `.3s`, `0`.
pub(crate) fn parse_time<'i>(p: &mut Parser<'i>) -> PResult<'i, f32> {
    match p.next()?.clone() {
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("ms") => Ok(value / 1000.0),
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("s") => Ok(value),
        Token::Number { value: 0.0, .. } => Ok(0.0),
        _ => err(p),
    }
}

pub(crate) fn parse_easing<'i>(p: &mut Parser<'i>) -> PResult<'i, Easing> {
    match p.next()?.clone() {
        Token::Ident(id) => Ok(match &*id.to_ascii_lowercase() {
            "linear" => Easing::LINEAR,
            "ease" => Easing::EASE,
            "ease-in" => Easing::CubicBezier(0.42, 0.0, 1.0, 1.0),
            "ease-out" => Easing::CubicBezier(0.0, 0.0, 0.58, 1.0),
            "ease-in-out" => Easing::CubicBezier(0.42, 0.0, 0.58, 1.0),
            "step-start" => Easing::Steps(1, true),
            "step-end" => Easing::Steps(1, false),
            _ => return err(p),
        }),
        Token::Function(f) if f.eq_ignore_ascii_case("cubic-bezier") => p.parse_nested_block(|p| {
            let mut v = [0.0f32; 4];
            for (i, slot) in v.iter_mut().enumerate() {
                if i > 0 {
                    p.expect_comma()?;
                }
                *slot = p.expect_number()?;
            }
            Ok(Easing::CubicBezier(v[0], v[1], v[2], v[3]))
        }),
        Token::Function(f) if f.eq_ignore_ascii_case("steps") => p.parse_nested_block(|p| {
            let n = p.expect_integer()?.max(1) as u32;
            let start = p
                .try_parse(|p| {
                    p.expect_comma()?;
                    p.expect_ident_cloned()
                })
                .is_ok_and(|id| matches!(&*id.to_ascii_lowercase(), "start" | "jump-start" | "jump-both"));
            Ok(Easing::Steps(n, start))
        }),
        Token::Function(f) if f.eq_ignore_ascii_case("linear") => p.parse_nested_block(|p| {
            while p.next().is_ok() {}
            Ok(Easing::LINEAR)
        }),
        _ => err(p),
    }
}

pub(crate) fn parse_iterations<'i>(p: &mut Parser<'i>) -> PResult<'i, f32> {
    match p.next()?.clone() {
        Token::Ident(id) if id.eq_ignore_ascii_case("infinite") => Ok(f32::INFINITY),
        Token::Number { value, .. } if value >= 0.0 => Ok(value),
        _ => err(p),
    }
}

pub(crate) fn parse_direction<'i>(p: &mut Parser<'i>) -> PResult<'i, AnimDirection> {
    keyword(p, &[
        ("normal", AnimDirection::Normal),
        ("reverse", AnimDirection::Reverse),
        ("alternate", AnimDirection::Alternate),
        ("alternate-reverse", AnimDirection::AlternateReverse),
    ])
}

/// `animation-fill-mode` → (forwards, backwards).
pub(crate) fn parse_fill<'i>(p: &mut Parser<'i>) -> PResult<'i, (bool, bool)> {
    keyword(p, &[("none", (false, false)), ("forwards", (true, false)), ("backwards", (false, true)), ("both", (true, true))])
}
