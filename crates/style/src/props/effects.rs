//! Transforms, shadows, opacity, stacking and pointer behavior.

use super::*;

pub(super) fn p_visibility<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::Visible(ident(p)? == "visible"));
    Ok(())
}

pub(super) fn p_z_index<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let z = p.try_parse(|p| p.expect_integer()).ok();
    out.push(D::ZIndex(z));
}
    Ok(())
}

pub(super) fn p_pointer_events<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let v = ident(p)?;
    out.push(D::PointerEventsNone(v.eq_ignore_ascii_case("none")));
}
    Ok(())
}

pub(super) fn p_cursor<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    // `cursor: url(x.cur), pointer`: skip images, take the first keyword we know.
    let mut cursor = None;
    while !p.is_exhausted() {
        if let Ok(Token::Ident(id)) = p.next().cloned() {
            let c = match id.to_ascii_lowercase().as_str() {
                "auto" | "default" | "none" | "context-menu" | "help" | "progress" | "wait" => Some(Cursor::Auto),
                "pointer" => Some(Cursor::Pointer),
                "text" | "vertical-text" => Some(Cursor::Text),
                "not-allowed" | "no-drop" => Some(Cursor::NotAllowed),
                "grab" => Some(Cursor::Grab),
                "grabbing" => Some(Cursor::Grabbing),
                "crosshair" | "cell" => Some(Cursor::Crosshair),
                "move" | "all-scroll" => Some(Cursor::Move),
                "col-resize" | "ew-resize" | "e-resize" | "w-resize" => Some(Cursor::ResizeColumn),
                "row-resize" | "ns-resize" | "n-resize" | "s-resize" => Some(Cursor::ResizeRow),
                _ => None,
            };
            if cursor.is_none() {
                cursor = c;
            }
        }
    }
    out.push(D::Cursor(cursor.unwrap_or(Cursor::Auto)));
}
    Ok(())
}

pub(super) fn p_box_shadow<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        out.push(D::BoxShadow(None));
    } else {
        let layers = p.parse_comma_separated(|p| {
            let mut lengths = Vec::new();
            let mut color = None;
            let mut inset = false;
            while !p.is_exhausted() {
                if p.try_parse(|p| p.expect_ident_matching("inset")).is_ok() {
                    inset = true;
                } else if let Ok(l) = p.try_parse(parse_len) {
                    lengths.push(l);
                } else if let Ok(c) = p.try_parse(parse_color) {
                    color = Some(c);
                } else {
                    p.next()?;
                }
            }
            Ok(ShadowSpec { lengths, color: color.unwrap_or(Color::CurrentColor), inset })
        })?;
        let layers: Vec<ShadowSpec> = layers.into_iter().filter(|l| l.lengths.len() >= 2).collect();
        out.push(D::BoxShadow((!layers.is_empty()).then(|| layers.into())));
    }
}
    Ok(())
}

pub(super) fn p_rotate<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    // `rotate: 45deg` (axis forms like `rotate: x 45deg` aren't 2D).
    let deg = if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() { 0.0 } else { parse_angle(p)? };
    out.push(D::Rotate(deg));
}
    Ok(())
}

pub(super) fn p_scale<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let (x, y) = if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        (1.0, 1.0)
    } else {
        let x = parse_factor(p)?;
        (x, p.try_parse(parse_factor).unwrap_or(x))
    };
    out.push(D::Scale(x, y));
}
    Ok(())
}

/// `transform-origin: <x> <y>? <z>?` (keywords, lengths, percentages).
pub(super) fn p_transform_origin<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let pct = |v: f32| LenSpec::Lin(Lin { pct: v, ..Lin::px(0.0) });
    let mut x = None;
    let mut y = None;
    for _ in 0..3 {
        if p.is_exhausted() {
            break;
        }
        if let Ok(id) = p.try_parse(|p| p.expect_ident_cloned()) {
            match id.to_ascii_lowercase().as_str() {
                "left" => x = Some(pct(0.0)),
                "right" => x = Some(pct(100.0)),
                "top" => y = Some(pct(0.0)),
                "bottom" => y = Some(pct(100.0)),
                "center" => {
                    if x.is_none() { x = Some(pct(50.0)) } else { y = Some(pct(50.0)) }
                }
                _ => return err(p),
            }
        } else {
            let l = parse_len(p)?;
            if x.is_none() { x = Some(l) } else if y.is_none() { y = Some(l) }
            // (A third value, z, means nothing in 2D.)
        }
    }
    out.push(Decl::TransformOrigin(x.unwrap_or(pct(50.0)), y.unwrap_or(pct(50.0))));
    Ok(())
}

pub(super) fn p_transform<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    // Translations, rotations and scales (skews and 3D are ignored).
    let (mut x, mut y) = (Vec::new(), Vec::new());
    let (mut rotate, mut scale) = (0.0f32, (1.0f32, 1.0f32));
    let mut functions = false;
    if name == "translate" {
        if let Ok(v) = parse_len(p) {
            x.push(v);
        }
        if let Ok(v) = p.try_parse(parse_len) {
            y.push(v);
        }
    }
    while !p.is_exhausted() {
        let Ok(token) = p.next().cloned() else { break };
        let Token::Function(f) = token else { continue };
        functions = true;
        let f = f.to_ascii_lowercase();
        p.parse_nested_block(|p| {
            match f.as_str() {
                "rotate" | "rotatez" => rotate += parse_angle(p).unwrap_or(0.0),
                "scale" | "scalex" | "scaley" | "scale3d" => {
                    let a = parse_factor(p).unwrap_or(1.0);
                    let b = p.try_parse(|p| {
                        p.expect_comma()?;
                        parse_factor(p)
                    });
                    match f.as_str() {
                        "scalex" => scale.0 *= a,
                        "scaley" => scale.1 *= a,
                        _ => {
                            scale.0 *= a;
                            scale.1 *= b.unwrap_or(a);
                        }
                    }
                }
                "matrix" => {
                    let mut m = [0.0f32; 6];
                    for (i, v) in m.iter_mut().enumerate() {
                        if i > 0 {
                            p.expect_comma()?;
                        }
                        *v = p.expect_number()?;
                    }
                    let [a, b, c, d, e, f] = m;
                    let sx = (a * a + b * b).sqrt();
                    rotate += b.atan2(a).to_degrees();
                    scale.0 *= sx;
                    scale.1 *= if sx > 0.0 { (a * d - b * c) / sx } else { 0.0 };
                    x.push(LenSpec::px(e));
                    y.push(LenSpec::px(f));
                }
                _ => {
                    let first = parse_len(p);
                    let second = p.try_parse(|p| {
                        p.expect_comma()?;
                        parse_len(p)
                    });
                    match f.as_str() {
                        "translate" | "translate3d" => {
                            x.extend(first.ok());
                            y.extend(second.ok());
                        }
                        "translatex" => x.extend(first.ok()),
                        "translatey" => y.extend(first.ok()),
                        _ => {}
                    }
                }
            }
            while p.next().is_ok() {}
            Ok(())
        })?;
    }
    let sum = |v: Vec<LenSpec>| match v.len() {
        0 => LenSpec::px(0.0),
        1 => v.into_iter().next().expect("one"),
        _ => LenSpec::Lin(v.iter().filter_map(|l| match l { LenSpec::Lin(l) => Some(*l), _ => None }).fold(Lin::default(), |a, b| a + b)),
    };
    out.push(D::Translate(sum(x), sum(y)));
    if name == "transform" {
        out.push(D::Rotate(rotate));
        out.push(D::Scale(scale.0, scale.1));
        out.push(D::Transformed(functions));
    }
}
    Ok(())
}

pub(super) fn p_opacity<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let v = match p.next()?.clone() {
        Token::Number { value, .. } => value,
        Token::Percentage { unit_value, .. } => unit_value,
        _ => return err(p),
    };
    out.push(D::Opacity(v.clamp(0.0, 1.0)));
}
    Ok(())
}

pub(super) fn p_filter<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        out.push(D::Filter(None));
        return Ok(());
    }
    let mut list = Vec::new();
    while !p.is_exhausted() {
        let name = match p.next()?.clone() {
            Token::Function(name) => name.to_ascii_lowercase(),
            // `url(#svg-filter)`: not something we can draw.
            Token::UnquotedUrl(_) => continue,
            _ => return err(p),
        };
        let spec = p.parse_nested_block(|p| {
            let amount = |p: &mut Parser<'i>| p.try_parse(parse_factor).ok().unwrap_or(1.0).max(0.0);
            Ok(match name.as_str() {
                "brightness" => FilterSpec::Color(Filter::Brightness(amount(p))),
                "contrast" => FilterSpec::Color(Filter::Contrast(amount(p))),
                "grayscale" => FilterSpec::Color(Filter::Grayscale(amount(p).min(1.0))),
                "invert" => FilterSpec::Color(Filter::Invert(amount(p).min(1.0))),
                "opacity" => FilterSpec::Color(Filter::Opacity(amount(p).min(1.0))),
                "saturate" => FilterSpec::Color(Filter::Saturate(amount(p))),
                "sepia" => FilterSpec::Color(Filter::Sepia(amount(p).min(1.0))),
                "hue-rotate" => FilterSpec::Color(Filter::HueRotate(p.try_parse(parse_angle).unwrap_or(0.0))),
                "blur" => FilterSpec::Blur(p.try_parse(parse_len).unwrap_or(LenSpec::Lin(Default::default()))),
                "drop-shadow" => {
                    let mut lengths = Vec::new();
                    let mut color = None;
                    while !p.is_exhausted() {
                        if let Ok(l) = p.try_parse(parse_len) {
                            lengths.push(l);
                        } else if let Ok(c) = p.try_parse(parse_color) {
                            color = Some(c);
                        } else {
                            p.next()?;
                        }
                    }
                    if lengths.len() < 2 {
                        return err(p);
                    }
                    FilterSpec::DropShadow(ShadowSpec { lengths, color: color.unwrap_or(Color::CurrentColor), inset: false })
                }
                _ => return err(p),
            })
        })?;
        list.push(spec);
    }
    out.push(D::Filter((!list.is_empty()).then(|| list.into())));
    Ok(())
}

/// The properties this module parses.
pub(super) const PROPERTIES: &[(&str, ParseFn)] = &[
    ("visibility", p_visibility),
    ("z-index", p_z_index),
    ("pointer-events", p_pointer_events),
    ("cursor", p_cursor),
    ("box-shadow", p_box_shadow),
    ("rotate", p_rotate),
    ("scale", p_scale),
    ("transform-origin", p_transform_origin),
    ("transform", p_transform),
    ("translate", p_transform),
    ("opacity", p_opacity),
    ("filter", p_filter),
    ("-webkit-filter", p_filter),
];
