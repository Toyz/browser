//! Fonts, text, lists and generated content.

use super::*;

pub(super) fn p_color<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::Color(parse_color(p)?));
    Ok(())
}

pub(super) fn p_webkit_text_fill_color<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::TextFill(parse_color(p)?));
    Ok(())
}

pub(super) fn p_font<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
parse_font(p, out)?;
    Ok(())
}

pub(super) fn p_font_size<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::FontSize(parse_font_size(p)?));
    Ok(())
}

pub(super) fn p_font_weight<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::FontWeight(parse_weight(p)?));
    Ok(())
}

pub(super) fn p_font_style<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let v = ident(p)?;
    // `oblique 10deg`
    let _ = p.try_parse(|p| p.next().map(|_| ()));
    out.push(D::Italic(matches!(v.as_str(), "italic" | "oblique")));
}
    Ok(())
}

pub(super) fn p_font_family<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let (family, names) = parse_font_family(p)?;
    out.extend([D::FontFamily(family), D::FontNames(names)]);
}
    Ok(())
}

pub(super) fn p_line_height<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::LineHeight(parse_line_height(p)?));
    Ok(())
}

pub(super) fn p_text_align<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::TextAlign(keyword(p, &[
    ("left", TextAlign::Left),
    ("start", TextAlign::Left),
    ("justify", TextAlign::Left),
    ("-webkit-left", TextAlign::Left),
    ("center", TextAlign::Center),
    ("-webkit-center", TextAlign::Center),
    ("-moz-center", TextAlign::Center),
    ("right", TextAlign::Right),
    ("end", TextAlign::Right),
    ("-webkit-right", TextAlign::Right),
    ("match-parent", TextAlign::Left),
])?));
    Ok(())
}

pub(super) fn p_text_decoration<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
parse_text_decoration(p, out)?;
    Ok(())
}

pub(super) fn p_text_transform<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::TextTransform(keyword(p, &[
    ("none", TextTransform::None),
    ("uppercase", TextTransform::Uppercase),
    ("lowercase", TextTransform::Lowercase),
    ("capitalize", TextTransform::Capitalize),
])?));
    Ok(())
}

/// `font-variant` / `font-variant-caps`: only the capitals are drawn
/// (ligature and numeric variants are what fonts do by default anyway).
pub(super) fn p_font_variant<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
    let mut caps = None;
    while !p.is_exhausted() {
        match p.next()?.clone() {
            Token::Ident(id) => match id.to_ascii_lowercase().as_str() {
                "small-caps" | "petite-caps" | "unicase" | "titling-caps" => caps = Some(false),
                "all-small-caps" | "all-petite-caps" => caps = Some(true),
                _ => {}
            },
            _ => {}
        }
    }
    out.push(D::SmallCaps(caps));
    Ok(())
}

pub(super) fn p_white_space<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::WhiteSpace(keyword(p, &[
    ("normal", WhiteSpace::Normal),
    ("nowrap", WhiteSpace::NoWrap),
    ("pre", WhiteSpace::Pre),
    ("pre-wrap", WhiteSpace::PreWrap),
    ("break-spaces", WhiteSpace::PreWrap),
    ("pre-line", WhiteSpace::PreLine),
])?));
    Ok(())
}

pub(super) fn p_list_style<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let mut found = None;
    while !p.is_exhausted() {
        if let Ok(ls) = p.try_parse(parse_list_style_type) {
            found = Some(ls);
        } else {
            p.next()?; // image / position: ignored
        }
    }
    match found {
        Some(ls) => out.push(D::ListStyle(ls)),
        None if name == "list-style" => out.push(D::ListStyle(ListStyle::Disc)),
        None => return err(p),
    }
}
    Ok(())
}

pub(super) fn p_content<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::Content(parse_content(p)?));
    Ok(())
}

pub(super) fn p_color_scheme<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let mut dark = false;
    while let Ok(id) = p.expect_ident_cloned() {
        dark |= id.eq_ignore_ascii_case("dark");
    }
    out.push(D::ColorScheme(dark));
}
    Ok(())
}

pub(super) fn p_text_overflow<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let v = ident(p)?;
    out.push(D::TextEllipsis(v.eq_ignore_ascii_case("ellipsis")));
}
    Ok(())
}

pub(super) fn p_webkit_line_clamp<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let n = p.try_parse(|p| p.expect_integer()).ok().filter(|&n| n > 0).map(|n| n as u32);
    out.push(D::LineClamp(n));
}
    Ok(())
}

pub(super) fn p_vertical_align<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let v = match p.try_parse(|p| keyword(p, &[
        ("baseline", VerticalAlign::Baseline),
        ("sub", VerticalAlign::Sub),
        ("super", VerticalAlign::Super),
        ("middle", VerticalAlign::Middle),
        ("top", VerticalAlign::Top),
        ("bottom", VerticalAlign::Bottom),
        ("text-top", VerticalAlign::TextTop),
        ("text-bottom", VerticalAlign::TextBottom),
    ])) {
        Ok(k) => VAlignSpec::Keyword(k),
        Err(_) => VAlignSpec::Len(parse_len(p)?),
    };
    out.push(D::VerticalAlign(v));
}
    Ok(())
}

/// The properties this module parses.
pub(super) const PROPERTIES: &[(&str, ParseFn)] = &[
    ("color", p_color),
    ("-webkit-text-fill-color", p_webkit_text_fill_color),
    ("font", p_font),
    ("font-size", p_font_size),
    ("font-weight", p_font_weight),
    ("font-style", p_font_style),
    ("font-family", p_font_family),
    ("line-height", p_line_height),
    ("text-align", p_text_align),
    ("text-decoration", p_text_decoration),
    ("text-decoration-line", p_text_decoration),
    ("text-transform", p_text_transform),
    ("font-variant", p_font_variant),
    ("font-variant-caps", p_font_variant),
    ("white-space", p_white_space),
    ("list-style", p_list_style),
    ("list-style-type", p_list_style),
    ("content", p_content),
    ("color-scheme", p_color_scheme),
    ("text-overflow", p_text_overflow),
    ("-webkit-line-clamp", p_webkit_line_clamp),
    ("line-clamp", p_webkit_line_clamp),
    ("vertical-align", p_vertical_align),
];
