//! Flexbox and grid: containers, items, alignment and gaps.

use super::*;

// ---- flexbox ----
pub(super) fn p_flex_direction<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::FlexDirection(parse_flex_direction(p)?));
    Ok(())
}

pub(super) fn p_flex_wrap<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::FlexWrap(ident(p)? != "nowrap"));
    Ok(())
}

pub(super) fn p_flex_flow<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    while !p.is_exhausted() {
        if let Ok(d) = p.try_parse(parse_flex_direction) {
            out.push(D::FlexDirection(d));
        } else {
            out.push(D::FlexWrap(ident(p)? != "nowrap"));
        }
    }
}
    Ok(())
}

pub(super) fn p_justify_content<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::JustifyContent(parse_justify(p)?));
    Ok(())
}

pub(super) fn p_align_items<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::AlignItems(parse_align(p)?));
    Ok(())
}

pub(super) fn p_align_self<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    out.push(D::AlignSelf(match p.try_parse(|p| p.expect_ident_matching("auto")) {
        Ok(()) => None,
        Err(_) => Some(parse_align(p)?),
    }))
}
    Ok(())
}

pub(super) fn p_align_content<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::AlignContent(parse_justify(p)?));
    Ok(())
}

pub(super) fn p_place_items<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    out.push(D::AlignItems(parse_align(p)?));
    let _ = p.try_parse(parse_align);
}
    Ok(())
}

pub(super) fn p_place_content<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let a = parse_justify(p)?;
    let j = p.try_parse(parse_justify).unwrap_or(a);
    out.push(D::AlignContent(a));
    out.push(D::JustifyContent(j));
}
    Ok(())
}

pub(super) fn p_gap<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let row = parse_len(p)?;
    let col = p.try_parse(parse_len).unwrap_or_else(|_| row.clone());
    out.push(D::RowGap(row));
    out.push(D::ColumnGap(col));
}
    Ok(())
}

pub(super) fn p_row_gap<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::RowGap(parse_len(p)?));
    Ok(())
}

pub(super) fn p_column_gap<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::ColumnGap(parse_len(p)?));
    Ok(())
}

pub(super) fn p_flex_grow<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::FlexGrow(p.expect_number()?));
    Ok(())
}

pub(super) fn p_flex_shrink<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::FlexShrink(p.expect_number()?));
    Ok(())
}

pub(super) fn p_flex_basis<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::FlexBasis(parse_len(p)?));
    Ok(())
}

pub(super) fn p_flex<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
parse_flex(p, out)?;
    Ok(())
}

// ---- grid ----
pub(super) fn p_grid_template_columns<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
parse_template_columns(p, out)?;
    Ok(())
}

pub(super) fn p_grid_template_areas<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::GridAreas(parse_areas(p)));
    Ok(())
}

pub(super) fn p_grid_template<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    // `<rows> / <columns>`, or `"a b" "c d" / <columns>`.
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        out.push(D::GridColumns(GridCols::None));
        out.push(D::GridTracks(None));
        out.push(D::GridAreas(None));
    } else {
        let mut areas: Vec<Arc<[Arc<str>]>> = Vec::new();
        let mut slash = false;
        while !p.is_exhausted() {
            match p.next()?.clone() {
                Token::Delim('/') => {
                    slash = true;
                    break;
                }
                Token::QuotedString(row) => areas.push(area_row(&row)),
                _ => {}
            }
        }
        if slash {
            parse_template_columns(p, out)?;
        }
        if !areas.is_empty() {
            out.push(D::GridAreas(Some(areas.into())));
        }
    }
}
    Ok(())
}

pub(super) fn p_grid_area<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let name = p.try_parse(|p| p.expect_ident_cloned()).ok().filter(|n| !n.eq_ignore_ascii_case("auto"));
    // `grid-area: 1 / 1` (row start / column start / ...): line numbers.
    let mut lines = Vec::new();
    while let Ok(t) = p.next().cloned() {
        if let Token::Number { int_value: Some(n), .. } = t {
            lines.push(n);
        }
    }
    if name.is_none() && !lines.is_empty() {
        out.push(D::GridRowStart(lines.first().map(|&n| n as i16)));
        out.push(D::GridColumnStart(lines.get(1).map(|&n| n as i16)));
    }
    out.push(D::GridArea(name.map(|n| Arc::from(&*n))));
}
    Ok(())
}

pub(super) fn p_grid_row_start<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let n = p.try_parse(|p| p.expect_integer()).ok();
    while p.next().is_ok() {}
    out.push(D::GridRowStart(n.map(|n| n as i16)));
}
    Ok(())
}

pub(super) fn p_grid_column_start<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let n = p.try_parse(|p| p.expect_integer()).ok();
    while p.next().is_ok() {}
    out.push(D::GridColumnStart(n.map(|n| n as i16)));
}
    Ok(())
}

pub(super) fn p_grid_column<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    if name == "grid-column"
        && let Ok(n) = p.try_parse(|p| p.expect_integer())
    {
        // `grid-column: 2 / 4`: a start line (and maybe an end).
        out.push(D::GridColumnStart(Some(n as i16)));
    }
    let mut span = None;
    let mut values = Vec::new();
    while !p.is_exhausted() {
        match p.next()?.clone() {
            Token::Ident(id) if id.eq_ignore_ascii_case("span") => {
                if let Ok(n) = p.expect_integer() {
                    span = Some(GridSpan::Span(n.clamp(1, 100) as u16));
                }
            }
            Token::Number { int_value: Some(n), .. } => values.push(n),
            _ => {}
        }
    }
    if values.last() == Some(&-1) {
        span = Some(GridSpan::Full);
    }
    out.push(D::GridColumn(span));
}
    Ok(())
}

/// The properties this module parses.
pub(super) const PROPERTIES: &[(&str, ParseFn)] = &[
    ("flex-direction", p_flex_direction),
    ("flex-wrap", p_flex_wrap),
    ("flex-flow", p_flex_flow),
    ("justify-content", p_justify_content),
    ("align-items", p_align_items),
    ("align-self", p_align_self),
    ("align-content", p_align_content),
    ("place-items", p_place_items),
    ("place-content", p_place_content),
    ("gap", p_gap),
    ("grid-gap", p_gap),
    ("row-gap", p_row_gap),
    ("grid-row-gap", p_row_gap),
    ("column-gap", p_column_gap),
    ("grid-column-gap", p_column_gap),
    ("flex-grow", p_flex_grow),
    ("flex-shrink", p_flex_shrink),
    ("flex-basis", p_flex_basis),
    ("flex", p_flex),
    ("grid-template-areas", p_grid_template_areas),
    ("grid-template", p_grid_template),
];
