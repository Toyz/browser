//! Backgrounds, masks and how replaced content fits its box.

use super::*;

pub(super) fn p_background_color<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::BackgroundColor(parse_color(p)?));
    Ok(())
}

// `text`: the background shows through the glyphs (gradient text).
pub(super) fn p_background_clip<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let mut text = false;
    let mut layers = vec![true];
    while !p.is_exhausted() {
        match p.next() {
            Ok(Token::Ident(id)) => {
                text |= id.eq_ignore_ascii_case("text");
                if let Some(last) = layers.last_mut() {
                    *last = id.eq_ignore_ascii_case("border-box");
                }
            }
            Ok(Token::Comma) => layers.push(true),
            _ => {}
        }
    }
    out.push(D::ClipText(text));
    out.push(D::BgClipBorder(layers.into()));
}
    Ok(())
}

pub(super) fn p_background<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
parse_background(p, out)?;
    Ok(())
}

pub(super) fn p_background_image<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let layer = parse_image_layer(p)?;
    out.push(D::BackgroundImage(layer.gradient));
    out.push(D::Gradients(layer.gradients()));
    out.push(D::BackgroundUrl(layer.url));
}
    Ok(())
}

pub(super) fn p_background_repeat<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let layer = parse_image_layer(p)?;
    out.push(D::BackgroundRepeat(layer.repeat.unwrap_or_default()));
}
    Ok(())
}

pub(super) fn p_object_fit<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let fit = match ident(p)?.to_ascii_lowercase().as_str() {
        "contain" => crate::ObjectFit::Contain,
        "cover" => crate::ObjectFit::Cover,
        "none" => crate::ObjectFit::None,
        "scale-down" => crate::ObjectFit::ScaleDown,
        _ => crate::ObjectFit::Fill,
    };
    out.push(D::ObjectFit(fit));
}
    Ok(())
}

pub(super) fn p_mask<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let layer = parse_image_layer(p)?;
    out.push(D::Masked(layer.url.is_some() || layer.other_image));
    out.push(D::MaskUrl(layer.url));
    out.push(D::MaskSize(layer.size.unwrap_or(BgSizeSpec::Auto)));
    out.push(D::MaskPosition(layer.position.unwrap_or(BgPos::TOP_LEFT)));
}
    Ok(())
}

pub(super) fn p_mask_image<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let layer = parse_image_layer(p)?;
    out.push(D::Masked(layer.url.is_some() || layer.other_image));
    out.push(D::MaskUrl(layer.url));
}
    Ok(())
}

pub(super) fn p_mask_size<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::MaskSize(parse_bg_size(p)?));
    Ok(())
}

pub(super) fn p_mask_position<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::MaskPosition(parse_bg_position(p)?));
    Ok(())
}

pub(super) fn p_background_size<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::BackgroundSize(parse_bg_size(p)?));
    Ok(())
}

pub(super) fn p_background_position<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::BackgroundPosition(parse_bg_position(p)?));
    Ok(())
}

/// The properties this module parses.
pub(super) const PROPERTIES: &[(&str, ParseFn)] = &[
    ("background-color", p_background_color),
    ("background-clip", p_background_clip),
    ("-webkit-background-clip", p_background_clip),
    ("background", p_background),
    ("background-image", p_background_image),
    ("background-repeat", p_background_repeat),
    ("object-fit", p_object_fit),
    ("mask", p_mask),
    ("-webkit-mask", p_mask),
    ("mask-image", p_mask_image),
    ("-webkit-mask-image", p_mask_image),
    ("mask-size", p_mask_size),
    ("-webkit-mask-size", p_mask_size),
    ("mask-position", p_mask_position),
    ("-webkit-mask-position", p_mask_position),
    ("background-size", p_background_size),
    ("background-position", p_background_position),
];
