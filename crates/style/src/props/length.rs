//! Lengths, angles and `calc()` / `min()` / `max()` / `clamp()`.

use super::*;

pub(crate) fn unit_lin(value: f32, unit: &str) -> Option<Lin> {
    let mut l = Lin::default();
    match unit.to_ascii_lowercase().as_str() {
        "px" => l.px = value,
        "pt" => l.px = value * 4.0 / 3.0,
        "pc" => l.px = value * 16.0,
        "in" => l.px = value * 96.0,
        "cm" => l.px = value * 96.0 / 2.54,
        "mm" => l.px = value * 96.0 / 25.4,
        "q" => l.px = value * 96.0 / 101.6,
        "em" | "cap" | "ic" | "lh" => l.em = value,
        "ex" | "ch" => l.em = value * 0.5,
        "rem" | "rlh" | "rch" | "rex" => l.rem = value,
        "vw" | "svw" | "lvw" | "dvw" | "vi" | "svi" | "lvi" | "dvi" => l.vw = value,
        "vh" | "svh" | "lvh" | "dvh" | "vb" | "svb" | "lvb" | "dvb" => l.vh = value,
        "vmin" | "svmin" | "lvmin" | "dvmin" => l.vmin = value,
        "vmax" | "svmax" | "lvmax" | "dvmax" => l.vmax = value,
        // (cqmin/cqmax: the inline size, which is the smaller one mostly.)
        "cqw" | "cqi" | "cqmin" | "cqmax" => l.cqw = value,
        "cqh" | "cqb" => l.cqh = value,
        _ => return None,
    }
    Some(l)
}

/// An angle in degrees (`45deg`, `0.5turn`, `1rad`, `100grad`, `0`).
pub(crate) fn parse_angle<'i>(p: &mut Parser<'i>) -> PResult<'i, f32> {
    match p.next()?.clone() {
        Token::Dimension { value, unit, .. } => Ok(match unit.to_ascii_lowercase().as_str() {
            "deg" => value,
            "rad" => value.to_degrees(),
            "turn" => value * 360.0,
            "grad" => value * 0.9,
            _ => return err(p),
        }),
        Token::Number { value: 0.0, .. } => Ok(0.0),
        _ => err(p),
    }
}

/// A scale factor: `1.5` or `150%`.
pub(crate) fn parse_factor<'i>(p: &mut Parser<'i>) -> PResult<'i, f32> {
    match p.next()?.clone() {
        Token::Number { value, .. } => Ok(value),
        Token::Percentage { unit_value, .. } => Ok(unit_value),
        _ => err(p),
    }
}

pub(crate) fn parse_len<'i>(p: &mut Parser<'i>) -> PResult<'i, LenSpec> {
    let token = p.next()?.clone();
    match token {
        Token::Dimension { value, ref unit, .. } => match unit_lin(value, unit) {
            Some(l) => Ok(LenSpec::Lin(l)),
            None => err(p),
        },
        Token::Percentage { unit_value, .. } => Ok(LenSpec::Lin(Lin::pct(unit_value * 100.0))),
        Token::Number { value: 0.0, .. } => Ok(LenSpec::px(0.0)),
        Token::Ident(ref id) => match id.to_ascii_lowercase().as_str() {
            "auto" | "none" | "min-content" | "max-content" | "fit-content" | "stretch"
            | "-webkit-fill-available" | "-moz-available" | "-moz-fit-content" | "-webkit-fit-content" => {
                Ok(LenSpec::Auto)
            }
            _ => err(p),
        },
        Token::Function(ref name) => {
            let name = name.to_ascii_lowercase();
            p.parse_nested_block(|p| match name.as_str() {
                "calc" | "-webkit-calc" | "-moz-calc" => calc_to_len(p),
                "min" | "max" => {
                    let items = p.parse_comma_separated(calc_or_len)?;
                    Ok(if name == "min" { LenSpec::Min(items) } else { LenSpec::Max(items) })
                }
                "clamp" => {
                    let lo = calc_or_len(p)?;
                    p.expect_comma()?;
                    let pref = calc_or_len(p)?;
                    p.expect_comma()?;
                    let hi = calc_or_len(p)?;
                    Ok(LenSpec::Clamp(Box::new([lo, pref, hi])))
                }
                "fit-content" => {
                    while p.next().is_ok() {}
                    Ok(LenSpec::Auto)
                }
                _ => err(p),
            })
        }
        _ => err(p),
    }
}

pub(crate) fn calc_or_len<'i>(p: &mut Parser<'i>) -> PResult<'i, LenSpec> {
    p.try_parse(calc_to_len).or_else(|_| parse_len(p))
}

pub(crate) fn calc_to_len<'i>(p: &mut Parser<'i>) -> PResult<'i, LenSpec> {
    match calc_sum(p)? {
        CalcVal::Len(l) => Ok(LenSpec::Lin(l)),
        CalcVal::Num(0.0) => Ok(LenSpec::px(0.0)),
        CalcVal::Num(_) => err(p),
        CalcVal::Terms(terms) => Ok(LenSpec::Sum(terms)),
    }
}

#[derive(Clone)]
pub(crate) enum CalcVal {
    Num(f32),
    Len(Lin),
    /// Lengths that aren't linear (`min()`/`max()`/`clamp()` inside): Σ k·term.
    Terms(Vec<(f32, LenSpec)>),
}

impl CalcVal {
    /// As a sum of terms (numbers only as `0`).
    fn terms(self) -> Option<Vec<(f32, LenSpec)>> {
        match self {
            CalcVal::Terms(t) => Some(t),
            CalcVal::Len(l) => Some(vec![(1.0, LenSpec::Lin(l))]),
            CalcVal::Num(0.0) => Some(Vec::new()),
            CalcVal::Num(_) => None,
        }
    }

    fn scaled(self, k: f32) -> CalcVal {
        match self {
            CalcVal::Num(a) => CalcVal::Num(a * k),
            CalcVal::Len(l) => CalcVal::Len(l.scale(k)),
            CalcVal::Terms(t) => CalcVal::Terms(t.into_iter().map(|(a, s)| (a * k, s)).collect()),
        }
    }
}

pub(crate) fn calc_sum<'i>(p: &mut Parser<'i>) -> PResult<'i, CalcVal> {
    let mut acc = calc_product(p)?;
    while let Ok((negative, rhs)) = p.try_parse(|p| {
        let negative = match p.next()? {
            Token::Delim('+') => false,
            Token::Delim('-') => true,
            _ => return err(p),
        };
        Ok((negative, calc_product(p)?))
    }) {
        let sign = if negative { -1.0 } else { 1.0 };
        acc = match (acc, rhs) {
            (CalcVal::Num(a), CalcVal::Num(b)) => CalcVal::Num(a + sign * b),
            (CalcVal::Len(a), CalcVal::Len(b)) => CalcVal::Len(a.plus(b.scale(sign))),
            // `0` mixed with lengths is fine in practice
            (CalcVal::Len(a), CalcVal::Num(0.0)) | (CalcVal::Num(0.0), CalcVal::Len(a)) => CalcVal::Len(a),
            (a, b) => match (a.terms(), b.scaled(sign).terms()) {
                (Some(mut a), Some(b)) => {
                    a.extend(b);
                    CalcVal::Terms(a)
                }
                _ => return err(p),
            },
        };
    }
    Ok(acc)
}

pub(crate) fn calc_product<'i>(p: &mut Parser<'i>) -> PResult<'i, CalcVal> {
    let mut acc = calc_value(p)?;
    while let Ok((divide, rhs)) = p.try_parse(|p| {
        let divide = match p.next()? {
            Token::Delim('*') => false,
            Token::Delim('/') => true,
            _ => return err(p),
        };
        Ok((divide, calc_value(p)?))
    }) {
        acc = match (acc, rhs, divide) {
            (CalcVal::Num(a), CalcVal::Num(b), false) => CalcVal::Num(a * b),
            (CalcVal::Num(a), CalcVal::Num(b), true) if b != 0.0 => CalcVal::Num(a / b),
            (v, CalcVal::Num(b), false) | (CalcVal::Num(b), v, false) => v.scaled(b),
            (v, CalcVal::Num(b), true) if b != 0.0 => v.scaled(1.0 / b),
            _ => return err(p),
        };
    }
    Ok(acc)
}

pub(crate) fn calc_value<'i>(p: &mut Parser<'i>) -> PResult<'i, CalcVal> {
    match p.next()?.clone() {
        Token::Number { value, .. } => Ok(CalcVal::Num(value)),
        Token::Dimension { value, ref unit, .. } => match unit_lin(value, unit) {
            Some(l) => Ok(CalcVal::Len(l)),
            None => err(p),
        },
        Token::Percentage { unit_value, .. } => Ok(CalcVal::Len(Lin::pct(unit_value * 100.0))),
        Token::ParenthesisBlock => p.parse_nested_block(calc_sum),
        Token::Function(ref f) if f.eq_ignore_ascii_case("calc") => p.parse_nested_block(calc_sum),
        // `calc(clamp(...) - 32px)`: a term of its own.
        Token::Function(ref f) if ["min", "max", "clamp"].iter().any(|n| f.eq_ignore_ascii_case(n)) => {
            let name = f.to_ascii_lowercase();
            let spec = p.parse_nested_block(|p| match name.as_str() {
                "clamp" => {
                    let lo = calc_or_len(p)?;
                    p.expect_comma()?;
                    let pref = calc_or_len(p)?;
                    p.expect_comma()?;
                    let hi = calc_or_len(p)?;
                    Ok(LenSpec::Clamp(Box::new([lo, pref, hi])))
                }
                _ => {
                    let items = p.parse_comma_separated(calc_or_len)?;
                    Ok(if name == "min" { LenSpec::Min(items) } else { LenSpec::Max(items) })
                }
            })?;
            Ok(CalcVal::Terms(vec![(1.0, spec)]))
        }
        _ => err(p),
    }
}

// ---------------------------------------------------------------------------
// Colors
