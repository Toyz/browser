//! CSS grid in full: track lists with sizing functions, `repeat()` and
//! line names; placements by number, name and span; auto tracks and flow.
//! (The older summaries, `GridColumns`/`GridTracks`/`GridColumnStart`...,
//! are still emitted for the row-based layout.)

use super::*;

/// `name` parsed by `old` (the summary) and then again by `new` (in full).
fn both<'i>(
    name: &str,
    p: &mut Parser<'i>,
    out: &mut Vec<Decl>,
    old: ParseFn,
    new: fn(&str, &mut Parser<'i>, &mut Vec<Decl>) -> PResult<'i, ()>,
) -> PResult<'i, ()> {
    let start = p.state();
    let mark = out.len();
    if old(name, p, out).is_err() {
        out.truncate(mark);
    }
    p.reset(&start);
    new(name, p, out)
}

pub(super) fn p_template_columns<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    both(name, p, out, super::flex_grid::p_grid_template_columns, |_, p, out| {
        let value = parse_template(p)?;
        out.push(Decl::GridTemplateFull { rows: false, value: value.map(Arc::new) });
        Ok(())
    })
}

/// `grid-template`: `<rows> / <columns>`, or area strings each with an
/// optional row size, then `/ <columns>`. The summary (areas, the older
/// tracks) as before, then both track lists in full.
pub(super) fn p_template<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    both(name, p, out, super::flex_grid::p_grid_template, |_, p, out| {
        if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
            out.push(Decl::GridTemplateFull { rows: true, value: None });
            out.push(Decl::GridTemplateFull { rows: false, value: None });
            return Ok(());
        }
        // Split at the top-level `/`; note the row sizes beside area strings.
        let mut seg_start = p.position();
        let mut segments: Vec<String> = Vec::new();
        let mut sizes: Vec<String> = Vec::new();
        let (mut strings, mut sized) = (false, true);
        loop {
            let before = p.position();
            match p.next() {
                Err(_) => {
                    segments.push(p.slice_from(seg_start).to_owned());
                    break;
                }
                Ok(Token::Delim('/')) if segments.is_empty() => {
                    segments.push(p.slice(seg_start..before).to_owned());
                    seg_start = p.position();
                }
                Ok(Token::QuotedString(_)) if segments.is_empty() => {
                    if !sized {
                        sizes.push("auto".into());
                    }
                    (strings, sized) = (true, false);
                }
                Ok(Token::SquareBracketBlock) => {}
                Ok(_) if segments.is_empty() && strings => {
                    sizes.push(p.slice_from(before).trim().to_owned());
                    sized = true;
                }
                Ok(_) => {}
            }
        }
        if strings && !sized {
            sizes.push("auto".into());
        }
        let list = |text: &str| -> PResult<'i, Option<GridTemplateSpec>> {
            let mut q = Parser::new(text);
            parse_template(&mut q)
        };
        let rows = if strings { sizes.join(" ") } else { segments[0].clone() };
        if !rows.trim().is_empty() {
            out.push(Decl::GridTemplateFull { rows: true, value: list(&rows)?.map(Arc::new) });
        }
        if let Some(columns) = segments.get(1).filter(|c| !c.trim().is_empty()) {
            out.push(Decl::GridTemplateFull { rows: false, value: list(columns)?.map(Arc::new) });
        }
        Ok(())
    })
}

pub(super) fn p_template_rows<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let value = parse_template(p)?;
    out.push(Decl::GridTemplateFull { rows: true, value: value.map(Arc::new) });
    Ok(())
}

pub(super) fn p_auto_tracks<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let mut tracks = Vec::new();
    while !p.is_exhausted() {
        tracks.push(parse_track(p)?);
    }
    out.push(Decl::GridAutoTracks { rows: name.ends_with("rows"), tracks: tracks.into() });
    Ok(())
}

pub(super) fn p_auto_flow<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let (mut column, mut dense) = (false, false);
    while !p.is_exhausted() {
        match ident(p)?.as_str() {
            "column" => column = true,
            "row" => column = false,
            "dense" => dense = true,
            _ => return err(p),
        }
    }
    out.push(Decl::GridAutoFlow { column, dense });
    Ok(())
}

/// `grid-row-start` and the other three longhands.
pub(super) fn p_place_longhand<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let old: ParseFn = if name.starts_with("grid-row") { super::flex_grid::p_grid_row_start } else if name == "grid-column-start" { super::flex_grid::p_grid_column_start } else { super::flex_grid::p_grid_column };
    both(name, p, out, old, |name, p, out| {
        let line = parse_line(p)?;
        out.push(Decl::GridPlace { row: name.starts_with("grid-row"), end: name.ends_with("-end"), line });
        Ok(())
    })
}

/// `grid-row` / `grid-column`: `<start> [/ <end>]`.
pub(super) fn p_place_shorthand<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let old: ParseFn = if name == "grid-row" { super::flex_grid::p_grid_row_start } else { super::flex_grid::p_grid_column };
    both(name, p, out, old, |name, p, out| {
        let row = name == "grid-row";
        let start = parse_line(p)?;
        let end = if p.try_parse(|p| p.expect_delim('/')).is_ok() { parse_line(p)? } else { implied_end(&start) };
        out.push(Decl::GridPlace { row, end: false, line: start });
        out.push(Decl::GridPlace { row, end: true, line: end });
        Ok(())
    })
}

/// `grid-area: <row-start> [/ <column-start> [/ <row-end> [/ <column-end>]]]`.
pub(super) fn p_area<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    both(name, p, out, super::flex_grid::p_grid_area, |_, p, out| {
        let mut lines = vec![parse_line(p)?];
        while lines.len() < 4 && p.try_parse(|p| p.expect_delim('/')).is_ok() {
            lines.push(parse_line(p)?);
        }
        // Missing ones: a name repeats; anything else is auto.
        let row_start = lines[0].clone();
        let column_start = lines.get(1).cloned().unwrap_or_else(|| implied_end(&row_start));
        let row_end = lines.get(2).cloned().unwrap_or_else(|| implied_end(&row_start));
        let column_end = lines.get(3).cloned().unwrap_or_else(|| implied_end(&column_start));
        for (row, end, line) in [(true, false, row_start), (false, false, column_start), (true, true, row_end), (false, true, column_end)] {
            out.push(Decl::GridPlace { row, end, line });
        }
        Ok(())
    })
}

/// `justify-items` / `justify-self`.
pub(super) fn p_justify_items<'i>(name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let mut value = None;
    while !p.is_exhausted() {
        value = match ident(p)?.as_str() {
            "start" | "flex-start" | "self-start" | "left" => Some(Align::Start),
            "end" | "flex-end" | "self-end" | "right" => Some(Align::End),
            "center" => Some(Align::Center),
            "stretch" => Some(Align::Stretch),
            "baseline" => Some(Align::Baseline),
            "normal" | "auto" | "legacy" | "safe" | "unsafe" | "first" | "last" => value,
            _ => return err(p),
        };
    }
    out.push(if name == "justify-self" { Decl::JustifySelf(value) } else { Decl::JustifyItems(value) });
    Ok(())
}

/// The end a lone start implies: the same name, else auto.
fn implied_end(start: &GridLine) -> GridLine {
    match start {
        GridLine::Named(name, 0) => GridLine::Named(name.clone(), 0),
        _ => GridLine::Auto,
    }
}

/// A numeric `calc()` body: `+ - * /` over numbers and parentheses.
fn calc_number<'i>(p: &mut Parser<'i>) -> PResult<'i, f64> {
    let mut total = calc_product(p)?;
    while !p.is_exhausted() {
        let state = p.state();
        match p.next()?.clone() {
            Token::Delim('+') => total += calc_product(p)?,
            Token::Delim('-') => total -= calc_product(p)?,
            // (`11 -2`: a signed number right after.)
            Token::Number { value, has_sign: true, .. } => total += value as f64,
            _ => {
                p.reset(&state);
                return err(p);
            }
        }
    }
    Ok(total)
}

fn calc_product<'i>(p: &mut Parser<'i>) -> PResult<'i, f64> {
    let mut v = calc_unit(p)?;
    loop {
        let state = p.state();
        match p.next() {
            Ok(Token::Delim('*')) => v *= calc_unit(p)?,
            Ok(Token::Delim('/')) => v /= calc_unit(p)?,
            _ => {
                p.reset(&state);
                return Ok(v);
            }
        }
    }
}

fn calc_unit<'i>(p: &mut Parser<'i>) -> PResult<'i, f64> {
    match p.next()?.clone() {
        Token::Number { value, .. } => Ok(value as f64),
        Token::ParenthesisBlock => p.parse_nested_block(|p| calc_number(p)),
        Token::Function(f) if f.eq_ignore_ascii_case("calc") => p.parse_nested_block(|p| calc_number(p)),
        _ => err(p),
    }
}

/// One grid line: `auto`, `3`, `-1`, `span 2`, `name`, `name 2`, `span name`.
fn parse_line<'i>(p: &mut Parser<'i>) -> PResult<'i, GridLine> {
    let (mut span, mut number, mut name) = (false, None, None);
    while !p.is_exhausted() {
        let state = p.state();
        match p.next()?.clone() {
            Token::Delim('/') => {
                p.reset(&state);
                break;
            }
            Token::Ident(id) if id.eq_ignore_ascii_case("auto") => return Ok(GridLine::Auto),
            Token::Ident(id) if id.eq_ignore_ascii_case("span") => span = true,
            Token::Ident(id) => name = Some(Arc::<str>::from(&*id)),
            Token::Number { int_value: Some(n), .. } => number = Some(n),
            // `span calc(11 - 2 + 1)` (custom properties substituted):
            // an integer, rounded.
            Token::Function(f) if f.eq_ignore_ascii_case("calc") => {
                let v = p.parse_nested_block(|p| calc_number(p))?;
                if !v.is_finite() {
                    return err(p);
                }
                number = Some(v.round() as i32);
            }
            _ => return err(p),
        }
    }
    Ok(match (span, number, name) {
        (true, n, None) => GridLine::Span(n.unwrap_or(1).clamp(1, 1000) as u16),
        (true, n, Some(name)) => GridLine::NamedSpan(name, n.unwrap_or(1).clamp(1, 1000) as u16),
        (false, Some(n), None) => GridLine::Line(n.clamp(-1000, 1000) as i16),
        // `0 name` is invalid; a bare name keeps index 0, "none given", which
        // first means the edge of the area with that name.
        (false, Some(0), Some(_)) => return err(p),
        (false, n, Some(name)) => GridLine::Named(name, n.unwrap_or(0).clamp(-1000, 1000) as i16),
        (false, None, None) => return err(p),
    })
}

/// A track list (`none`: `None`).
fn parse_template<'i>(p: &mut Parser<'i>) -> PResult<'i, Option<GridTemplateSpec>> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        return Ok(None);
    }
    if p.try_parse(|p| p.expect_ident_matching("subgrid")).is_ok() || p.try_parse(|p| p.expect_ident_matching("masonry")).is_ok() {
        while p.next().is_ok() {}
        return Ok(None);
    }
    let (entries, names) = parse_track_list(p, true)?;
    Ok(Some(GridTemplateSpec { entries, names }))
}

/// Tracks and the line names between them (`names.len() == tracks + 1`);
/// `repeat()` allowed at the top level.
fn parse_track_list<'i>(p: &mut Parser<'i>, top: bool) -> PResult<'i, (Vec<TemplateEntrySpec>, Vec<Vec<Arc<str>>>)> {
    let mut entries = Vec::new();
    let mut names: Vec<Vec<Arc<str>>> = vec![Vec::new()];
    while !p.is_exhausted() {
        if p.try_parse(|p| p.expect_square_bracket_block()).is_ok() {
            let found = p.parse_nested_block(|p| {
                let mut v = Vec::new();
                while let Ok(id) = p.expect_ident_cloned() {
                    v.push(Arc::<str>::from(&*id));
                }
                Ok(v)
            })?;
            names.last_mut().expect("never empty").extend(found);
            continue;
        }
        if top && p.try_parse(|p| p.expect_function_matching("repeat")).is_ok() {
            let entry = p.parse_nested_block(|p| {
                let count = match p.next()?.clone() {
                    Token::Number { int_value: Some(n), .. } => RepeatCount::Count(n.clamp(1, 1000) as u16),
                    Token::Ident(id) if id.eq_ignore_ascii_case("auto-fill") => RepeatCount::AutoFill,
                    Token::Ident(id) if id.eq_ignore_ascii_case("auto-fit") => RepeatCount::AutoFit,
                    _ => return err(p),
                };
                p.expect_comma()?;
                let (inner, inner_names) = parse_track_list(p, false)?;
                let tracks = inner.into_iter().map(|e| match e {
                    TemplateEntrySpec::Track(t) => t,
                    TemplateEntrySpec::Repeat { .. } => TrackSizeSpec::Auto,
                }).collect::<Vec<_>>();
                let names = if inner_names.iter().all(|n| n.is_empty()) { Vec::new() } else { inner_names };
                Ok(TemplateEntrySpec::Repeat { count, tracks, names })
            })?;
            entries.push(entry);
            names.push(Vec::new());
            continue;
        }
        entries.push(TemplateEntrySpec::Track(parse_track(p)?));
        names.push(Vec::new());
    }
    if entries.is_empty() {
        return err(p);
    }
    Ok((entries, names))
}

/// One track size.
fn parse_track<'i>(p: &mut Parser<'i>) -> PResult<'i, TrackSizeSpec> {
    if p.try_parse(|p| p.expect_function_matching("minmax")).is_ok() {
        return p.parse_nested_block(|p| {
            let min = parse_track(p)?;
            p.expect_comma()?;
            let max = parse_track(p)?;
            Ok(TrackSizeSpec::MinMax(Box::new(min), Box::new(max)))
        });
    }
    if p.try_parse(|p| p.expect_function_matching("fit-content")).is_ok() {
        return p.parse_nested_block(|p| Ok(TrackSizeSpec::FitContent(parse_len(p)?)));
    }
    let state = p.state();
    match p.next()?.clone() {
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("fr") => return Ok(TrackSizeSpec::Fr(value.max(0.0))),
        Token::Ident(id) if id.eq_ignore_ascii_case("auto") => return Ok(TrackSizeSpec::Auto),
        Token::Ident(id) if id.eq_ignore_ascii_case("min-content") => return Ok(TrackSizeSpec::MinContent),
        Token::Ident(id) if id.eq_ignore_ascii_case("max-content") => return Ok(TrackSizeSpec::MaxContent),
        _ => p.reset(&state),
    }
    Ok(TrackSizeSpec::Len(parse_len(p)?))
}

pub(super) const PROPERTIES: &[(&str, ParseFn)] = &[
    ("grid-template", p_template),
    ("grid-template-columns", p_template_columns),
    ("grid-template-rows", p_template_rows),
    ("grid-auto-columns", p_auto_tracks),
    ("grid-auto-rows", p_auto_tracks),
    ("grid-auto-flow", p_auto_flow),
    ("grid-row-start", p_place_longhand),
    ("grid-row-end", p_place_longhand),
    ("grid-column-start", p_place_longhand),
    ("grid-column-end", p_place_longhand),
    ("grid-row", p_place_shorthand),
    ("grid-column", p_place_shorthand),
    ("grid-area", p_area),
    ("justify-items", p_justify_items),
    ("justify-self", p_justify_items),
];
