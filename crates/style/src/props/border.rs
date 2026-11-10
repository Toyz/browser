//! Borders: styles, widths and the `border-*` family.

use super::*;

pub(crate) fn parse_border_style<'i>(p: &mut Parser<'i>) -> PResult<'i, BorderStyle> {
    keyword(p, &[
        ("none", BorderStyle::None),
        ("hidden", BorderStyle::None),
        ("solid", BorderStyle::Solid),
        ("dashed", BorderStyle::Dashed),
        ("dotted", BorderStyle::Dotted),
        ("double", BorderStyle::Double),
        ("groove", BorderStyle::Other),
        ("ridge", BorderStyle::Other),
        ("inset", BorderStyle::Other),
        ("outset", BorderStyle::Other),
    ])
}

pub(crate) fn parse_border_width<'i>(p: &mut Parser<'i>) -> PResult<'i, LenSpec> {
    if let Ok(k) = p.try_parse(|p| keyword(p, &[("thin", 1.0), ("medium", 3.0), ("thick", 5.0)])) {
        return Ok(LenSpec::px(k));
    }
    parse_len(p)
}

pub(crate) fn parse_border_inner<'i>(rest: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    // border-radius family
    if rest == "-radius" {
        let v = four(p, parse_len)?;
        // Elliptical radii (`a / b`): ignore the vertical part.
        if p.try_parse(|p| p.expect_delim('/')).is_ok() {
            while p.next().is_ok() {}
        }
        for (i, r) in v.into_iter().enumerate() {
            out.push(Decl::BorderRadius(i, r));
        }
        return Ok(());
    }
    let corner = match rest {
        "-top-left-radius" | "-start-start-radius" => Some(0),
        "-top-right-radius" | "-start-end-radius" => Some(1),
        "-bottom-right-radius" | "-end-end-radius" => Some(2),
        "-bottom-left-radius" | "-end-start-radius" => Some(3),
        _ => None,
    };
    if let Some(corner) = corner {
        let r = parse_len(p)?;
        let _ = p.try_parse(parse_len);
        out.push(Decl::BorderRadius(corner, r));
        return Ok(());
    }

    // Split into sides + which component(s).
    let (sides, component): (&[Side], &str) = match rest {
        "" => (&Side::ALL, ""),
        "-width" | "-style" | "-color" => (&Side::ALL, rest),
        _ => {
            let rest = rest.strip_prefix('-').unwrap_or(rest);
            let (side_name, component) = match rest.rsplit_once('-') {
                Some((s, c @ ("width" | "style" | "color"))) => (s, c),
                _ => (rest, ""),
            };
            let Some(sides) = logical_sides(side_name) else {
                while p.next().is_ok() {}
                return Ok(()); // border-collapse, border-spacing, border-image...
            };
            (sides, component)
        }
    };

    match component.trim_start_matches('-') {
        "width" if sides.len() == 4 => {
            for (side, w) in Side::ALL.into_iter().zip(four(p, parse_border_width)?) {
                out.push(Decl::BorderWidth(side, w));
            }
        }
        "style" if sides.len() == 4 => {
            for (side, s) in Side::ALL.into_iter().zip(four(p, parse_border_style)?) {
                out.push(Decl::BorderStyle(side, s));
            }
        }
        "color" if sides.len() == 4 => {
            for (side, c) in Side::ALL.into_iter().zip(four(p, parse_color)?) {
                out.push(Decl::BorderColor(side, c));
            }
        }
        "width" => {
            let w = parse_border_width(p)?;
            sides.iter().for_each(|&s| out.push(Decl::BorderWidth(s, w.clone())));
        }
        "style" => {
            let st = parse_border_style(p)?;
            sides.iter().for_each(|&s| out.push(Decl::BorderStyle(s, st)));
        }
        "color" => {
            let c = parse_color(p)?;
            sides.iter().for_each(|&s| out.push(Decl::BorderColor(s, c)));
        }
        _ => {
            // Shorthand: <width> || <style> || <color>, any order.
            let mut width = None;
            let mut style = None;
            let mut color = None;
            while !p.is_exhausted() {
                if style.is_none()
                    && let Ok(s) = p.try_parse(parse_border_style)
                {
                    style = Some(s);
                } else if width.is_none()
                    && let Ok(w) = p.try_parse(parse_border_width)
                {
                    width = Some(w);
                } else if color.is_none() {
                    color = Some(parse_color(p)?);
                } else {
                    return err(p);
                }
            }
            for &side in sides {
                out.push(Decl::BorderWidth(side, width.clone().unwrap_or(LenSpec::px(3.0))));
                out.push(Decl::BorderStyle(side, style.unwrap_or(BorderStyle::None)));
                out.push(Decl::BorderColor(side, color.unwrap_or(Color::CurrentColor)));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Backgrounds
