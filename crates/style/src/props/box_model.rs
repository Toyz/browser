//! Display, positioning, sizing, overflow and floats (margins, padding and borders are families in `mod.rs`).

use super::*;

pub(super) fn p_display<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::Display(parse_display(p)?));
    Ok(())
}

pub(super) fn p_position<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::Position(keyword(p, &[
    ("static", Position::Static),
    ("relative", Position::Relative),
    ("absolute", Position::Absolute),
    ("fixed", Position::Fixed),
    ("sticky", Position::Sticky),
    ("-webkit-sticky", Position::Sticky),
])?));
    Ok(())
}

pub(super) fn p_top<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let side = match name.trim_start_matches("inset-") {
        "top" | "block-start" => Side::Top,
        "right" | "inline-end" => Side::Right,
        "bottom" | "block-end" => Side::Bottom,
        _ => Side::Left,
    };
    out.push(D::Inset(side, parse_len(p)?));
}
    Ok(())
}

pub(super) fn p_inset<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    for (side, v) in Side::ALL.into_iter().zip(four(p, parse_len)?) {
        out.push(D::Inset(side, v));
    }
}
    Ok(())
}

pub(super) fn p_inset_inline<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let first = parse_len(p)?;
    let second = p.try_parse(parse_len).unwrap_or_else(|_| first.clone());
    let sides = if name == "inset-inline" { [Side::Left, Side::Right] } else { [Side::Top, Side::Bottom] };
    out.push(D::Inset(sides[0], first));
    out.push(D::Inset(sides[1], second));
}
    Ok(())
}

pub(super) fn p_width<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    // `fit-content` / `max-content` / `min-content`: shrink to the content.
    let fit = p
        .try_parse(|p| match p.next()?.clone() {
            Token::Ident(id)
                if matches!(
                    &*id.to_ascii_lowercase(),
                    "fit-content" | "max-content" | "min-content" | "-moz-fit-content" | "-webkit-fit-content" | "-moz-max-content" | "-webkit-max-content"
                ) =>
            {
                Ok(())
            }
            Token::Function(f) if f.eq_ignore_ascii_case("fit-content") => p.parse_nested_block(|p| {
                while p.next().is_ok() {}
                Ok(())
            }),
            _ => err(p),
        })
        .is_ok();
    if fit {
        out.push(D::Width(LenSpec::Auto));
        out.push(D::FitWidth(true));
    } else {
        out.push(D::Width(parse_len(p)?));
    }
}
    Ok(())
}

pub(super) fn p_height<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::Height(parse_len(p)?));
    Ok(())
}

pub(super) fn p_min_width<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::MinWidth(parse_len(p)?));
    Ok(())
}

pub(super) fn p_min_height<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::MinHeight(parse_len(p)?));
    Ok(())
}

pub(super) fn p_max_width<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::MaxWidth(parse_len(p)?));
    Ok(())
}

pub(super) fn p_max_height<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::MaxHeight(parse_len(p)?));
    Ok(())
}

// `1`, `16 / 9`, `auto 4/3` (`auto` alone: none).
pub(super) fn p_aspect_ratio<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let mut nums = Vec::new();
    while !p.is_exhausted() {
        match p.next()?.clone() {
            Token::Number { value, .. } => nums.push(value),
            Token::Ident(_) | Token::Delim('/') => {}
            _ => return err(p),
        }
    }
    let ratio = match nums.as_slice() {
        [w] => Some(*w),
        [w, h] if *h > 0.0 => Some(w / h),
        _ => None,
    };
    out.push(D::AspectRatio(ratio.filter(|r| *r > 0.0)));
}
    Ok(())
}

pub(super) fn p_float<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::Float(keyword(p, &[
    ("none", Float::None),
    ("left", Float::Left),
    ("inline-start", Float::Left),
    ("right", Float::Right),
    ("inline-end", Float::Right),
])?));
    Ok(())
}

pub(super) fn p_clear<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::Clear(ident(p)? != "none"));
    Ok(())
}

pub(super) fn p_container_type<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    // `container: name / inline-size`: the type follows the slash.
    let mut kind = 0;
    while let Ok(token) = p.next() {
        if let Token::Ident(id) = token {
            match id.to_ascii_lowercase().as_str() {
                "inline-size" => kind = 1,
                "size" => kind = 2,
                _ => {}
            }
        }
    }
    out.push(D::ContainerType(kind));
}
    Ok(())
}

pub(super) fn p_box_sizing<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    out.push(D::BorderBox(ident(p)? == "border-box"))
}
    Ok(())
}

pub(super) fn p_overflow<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let mut overflow = Overflow::Visible;
    while !p.is_exhausted() {
        overflow = overflow.max_with(match ident(p)?.as_str() {
            "hidden" | "clip" => Overflow::Hidden,
            "auto" | "scroll" | "overlay" => Overflow::Scroll,
            _ => Overflow::Visible,
        });
    }
    out.push(D::Overflow(overflow));
}
    Ok(())
}

/// The properties this module parses.
pub(super) const PROPERTIES: &[(&str, ParseFn)] = &[
    ("display", p_display),
    ("position", p_position),
    ("top", p_top),
    ("right", p_top),
    ("bottom", p_top),
    ("left", p_top),
    ("inset-block-start", p_top),
    ("inset-block-end", p_top),
    ("inset-inline-start", p_top),
    ("inset-inline-end", p_top),
    ("inset", p_inset),
    ("inset-inline", p_inset_inline),
    ("inset-block", p_inset_inline),
    ("width", p_width),
    ("inline-size", p_width),
    ("height", p_height),
    ("block-size", p_height),
    ("min-width", p_min_width),
    ("min-inline-size", p_min_width),
    ("min-height", p_min_height),
    ("min-block-size", p_min_height),
    ("max-width", p_max_width),
    ("max-inline-size", p_max_width),
    ("max-height", p_max_height),
    ("max-block-size", p_max_height),
    ("aspect-ratio", p_aspect_ratio),
    ("float", p_float),
    ("clear", p_clear),
    ("container-type", p_container_type),
    ("container", p_container_type),
    ("box-sizing", p_box_sizing),
    ("-webkit-box-sizing", p_box_sizing),
    ("-moz-box-sizing", p_box_sizing),
    ("overflow", p_overflow),
    ("overflow-x", p_overflow),
    ("overflow-y", p_overflow),
];
