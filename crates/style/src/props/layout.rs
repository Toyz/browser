//! Display, overflow, flexbox and grid values.

use super::*;

impl Overflow {
    pub(crate) fn max_with(self, other: Overflow) -> Overflow {
        use Overflow::*;
        match (self, other) {
            (Hidden, _) | (_, Hidden) => Hidden,
            (Scroll, _) | (_, Scroll) => Scroll,
            _ => Visible,
        }
    }
}

pub(crate) fn parse_display<'i>(p: &mut Parser<'i>) -> PResult<'i, Display> {
    let first = ident(p)?;
    let second = p.try_parse(ident).ok();
    let d = match (first.as_str(), second.as_deref()) {
        ("none", _) => Display::None,
        ("contents", _) => Display::Contents,
        ("block" | "flow-root" | "run-in" | "table-caption" | "table-row-group" | "table-header-group"
        | "table-footer-group", None) => Display::Block,
        ("inline", None) | ("ruby" | "ruby-text" | "ruby-base", _) => Display::Inline,
        ("inline-block", _) | ("inline", Some("flow-root")) => Display::InlineBlock,
        ("list-item", _) => Display::ListItem,
        ("inline-flex" | "-webkit-inline-box" | "-webkit-inline-flex" | "-ms-inline-flexbox", _) | ("inline", Some("flex")) => {
            Display::InlineFlex
        }
        ("flex" | "-webkit-box" | "-webkit-flex" | "-ms-flexbox", _) | (_, Some("flex")) => Display::Flex,
        ("inline-grid" | "-ms-inline-grid", _) | ("inline", Some("grid")) => Display::InlineGrid,
        ("grid" | "-ms-grid", _) | (_, Some("grid")) => Display::Grid,
        ("table" | "inline-table", _) => Display::Table,
        ("table-row", _) => Display::TableRow,
        ("table-cell", _) => Display::TableCell,
        ("table-column" | "table-column-group", _) => Display::None,
        ("block", Some(_)) => Display::Block,
        _ => return err(p),
    };
    Ok(d)
}

pub(crate) fn parse_flex_direction<'i>(p: &mut Parser<'i>) -> PResult<'i, FlexDirection> {
    keyword(p, &[
        ("row", FlexDirection::Row),
        ("row-reverse", FlexDirection::RowReverse),
        ("column", FlexDirection::Column),
        ("column-reverse", FlexDirection::ColumnReverse),
    ])
}

pub(crate) fn parse_justify<'i>(p: &mut Parser<'i>) -> PResult<'i, Justify> {
    // `safe`/`unsafe` prefixes (`safe center` is kept apart).
    let safe = p.try_parse(|p| p.expect_ident_matching("safe")).is_ok();
    if !safe {
        let _ = p.try_parse(|p| p.expect_ident_matching("unsafe"));
    }
    let j = parse_justify_keyword(p)?;
    Ok(if safe && j == Justify::Center { Justify::SafeCenter } else { j })
}

fn parse_justify_keyword<'i>(p: &mut Parser<'i>) -> PResult<'i, Justify> {
    keyword(p, &[
        ("normal", Justify::Start),
        ("flex-start", Justify::Start),
        ("start", Justify::Start),
        ("left", Justify::Start),
        ("stretch", Justify::Start),
        ("center", Justify::Center),
        ("flex-end", Justify::End),
        ("end", Justify::End),
        ("right", Justify::End),
        ("space-between", Justify::SpaceBetween),
        ("space-around", Justify::SpaceAround),
        ("space-evenly", Justify::SpaceEvenly),
    ])
}

pub(crate) fn parse_align<'i>(p: &mut Parser<'i>) -> PResult<'i, Align> {
    let _ = p.try_parse(|p| p.expect_ident_matching("safe").or_else(|_| p.expect_ident_matching("unsafe")));
    let _ = p.try_parse(|p| p.expect_ident_matching("first").or_else(|_| p.expect_ident_matching("last")));
    keyword(p, &[
        ("normal", Align::Stretch),
        ("stretch", Align::Stretch),
        ("flex-start", Align::Start),
        ("start", Align::Start),
        ("self-start", Align::Start),
        ("center", Align::Center),
        ("flex-end", Align::End),
        ("end", Align::End),
        ("self-end", Align::End),
        ("baseline", Align::Baseline),
    ])
}

pub(crate) fn parse_flex<'i>(p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        out.extend([Decl::FlexGrow(0.), Decl::FlexShrink(0.), Decl::FlexBasis(LenSpec::Auto)]);
        return Ok(());
    }
    if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
        out.extend([Decl::FlexGrow(1.), Decl::FlexShrink(1.), Decl::FlexBasis(LenSpec::Auto)]);
        return Ok(());
    }
    let mut grow = None;
    let mut shrink = None;
    let mut basis = None;
    while !p.is_exhausted() {
        if let Ok(n) = p.try_parse(|p| p.expect_number()) {
            if grow.is_none() {
                grow = Some(n);
            } else if shrink.is_none() {
                shrink = Some(n);
            } else if n == 0.0 && basis.is_none() {
                // `flex: 1 1 0`: a unitless zero basis.
                basis = Some(LenSpec::px(0.0));
            } else {
                return err(p);
            }
        } else if basis.is_none() {
            basis = Some(parse_len(p)?);
        } else {
            return err(p);
        }
    }
    let (grow, shrink, basis) = match (grow, shrink, basis) {
        (None, None, Some(b)) => (1.0, 1.0, b),
        (g, s, b) => (g.unwrap_or(1.0), s.unwrap_or(1.0), b.unwrap_or(LenSpec::Lin(Lin::pct(0.0)))),
    };
    out.extend([Decl::FlexGrow(grow), Decl::FlexShrink(shrink), Decl::FlexBasis(basis)]);
    Ok(())
}

/// `grid-template-columns`: the column count (for equal columns) and, if
/// the tracks differ, the tracks themselves.
pub(crate) fn parse_template_columns<'i>(p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let start = p.state();
    let tracks = parse_tracks(p).ok().flatten();
    p.reset(&start);
    let begin = p.position();
    out.push(Decl::GridColumns(parse_grid_columns(p)?));
    let text = p.slice_from(begin).to_ascii_lowercase();
    out.push(Decl::GridContentMin(text.contains("fr") && !text.contains("minmax")));
    // Equal shares are equal columns; equal `auto` (or `min-content`)
    // tracks are each as wide as their content (Primer's buttons put an
    // icon, a label and an icon in `min-content auto min-content`).
    let uniform = tracks
        .as_ref()
        .is_some_and(|t| t.windows(2).all(|w| w[0] == w[1]) && t.iter().all(|t| matches!(t, TrackSpec::Fr(_))));
    out.push(Decl::GridTracks(if uniform { None } else { tracks.map(Into::into) }));
    Ok(())
}

/// Explicit tracks; `None` for anything we can't list (`auto-fill`).
pub(crate) fn parse_tracks<'i>(p: &mut Parser<'i>) -> PResult<'i, Option<Vec<TrackSpec>>> {
    fn track<'i>(p: &mut Parser<'i>) -> PResult<'i, Option<TrackSpec>> {
        Ok(Some(match p.next()?.clone() {
            Token::SquareBracketBlock => return Ok(None),
            Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("fr") => TrackSpec::Fr(value),
            Token::Dimension { value, unit, .. } => match unit_lin(value, &unit) {
                Some(l) => TrackSpec::Len(LenSpec::Lin(l)),
                None => TrackSpec::Auto,
            },
            Token::Number { value: 0.0, .. } => TrackSpec::Len(LenSpec::px(0.0)),
            Token::Function(f) if f.eq_ignore_ascii_case("minmax") => p.parse_nested_block(|p| {
                // The minimum, when it's a length (`minmax(15rem, 1fr)`).
                let min: Result<LenSpec, ()> = p.try_parse(|p| match p.next().map_err(|_| ())?.clone() {
                    Token::Dimension { value, unit, .. } if !unit.eq_ignore_ascii_case("fr") => unit_lin(value, &unit).map(LenSpec::Lin).ok_or(()),
                    _ => Err(()),
                });
                while !p.is_exhausted() && !matches!(p.next()?, Token::Comma) {}
                Ok(match p.next()?.clone() {
                    Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("fr") => match min {
                        Ok(min) => TrackSpec::FrMin(value, min),
                        Err(()) => TrackSpec::Fr(value),
                    },
                    Token::Dimension { value, unit, .. } => match unit_lin(value, &unit) {
                        Some(l) => TrackSpec::Max(LenSpec::Lin(l)),
                        None => TrackSpec::Auto,
                    },
                    _ => TrackSpec::Auto,
                })
            })?,
            Token::Function(_) => {
                p.parse_nested_block(|p| {
                    while p.next().is_ok() {}
                    Ok(())
                })?;
                TrackSpec::Auto
            }
            _ => TrackSpec::Auto,
        }))
    }
    let mut out = Vec::new();
    while !p.is_exhausted() {
        let state = p.state();
        if let Ok(Token::Function(f)) = p.next().cloned()
            && f.eq_ignore_ascii_case("repeat")
        {
            let repeated = p.parse_nested_block(|p| {
                let n = match p.next()?.clone() {
                    Token::Number { int_value: Some(n), .. } => n.clamp(1, 64) as usize,
                    _ => return Ok(None), // auto-fill / auto-fit
                };
                p.expect_comma()?;
                let mut inner = Vec::new();
                while !p.is_exhausted() {
                    if let Some(t) = track(p)? {
                        inner.push(t);
                    }
                }
                Ok(Some((n, inner)))
            })?;
            let Some((n, inner)) = repeated else { return Ok(None) };
            for _ in 0..n {
                out.extend(inner.iter().cloned());
            }
            continue;
        }
        p.reset(&state);
        if let Some(t) = track(p)? {
            out.push(t);
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

pub(crate) fn area_row(row: &str) -> Arc<[Arc<str>]> {
    row.split_whitespace().map(Arc::from).collect::<Vec<_>>().into()
}

pub(crate) fn parse_areas(p: &mut Parser) -> Option<GridAreas> {
    let mut rows = Vec::new();
    while let Ok(token) = p.next() {
        if let Token::QuotedString(row) = token {
            rows.push(area_row(row));
        }
    }
    (!rows.is_empty()).then(|| rows.into())
}

pub(crate) fn parse_grid_columns<'i>(p: &mut Parser<'i>) -> PResult<'i, GridCols> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        return Ok(GridCols::None);
    }
    if p.try_parse(|p| p.expect_ident_matching("subgrid")).is_ok() || p.try_parse(|p| p.expect_ident_matching("masonry")).is_ok() {
        while p.next().is_ok() {}
        return Ok(GridCols::None);
    }
    let mut count: u32 = 0;
    let mut auto_fill = None;
    let mut fit = false;
    while !p.is_exhausted() {
        match p.next()?.clone() {
            Token::SquareBracketBlock => {} // line names
            Token::Function(name) if name.eq_ignore_ascii_case("repeat") => {
                p.parse_nested_block(|p| {
                    let n = match p.next()?.clone() {
                        Token::Number { int_value: Some(n), .. } => Some(n.max(1) as u32),
                        Token::Ident(ref i) => {
                            fit = i.eq_ignore_ascii_case("auto-fit");
                            None
                        }
                        _ => return err(p),
                    };
                    p.expect_comma()?;
                    let mut inner = 0u32;
                    let mut min = None;
                    while !p.is_exhausted() {
                        match p.next()?.clone() {
                            Token::SquareBracketBlock => {}
                            Token::Function(f) if f.eq_ignore_ascii_case("minmax") => {
                                inner += 1;
                                if min.is_none() {
                                    min = p.parse_nested_block(|p| {
                                        let m = parse_len(p);
                                        while p.next().is_ok() {}
                                        Ok(m.ok())
                                    })?;
                                }
                            }
                            Token::Dimension { value, unit, .. } if min.is_none() && !unit.eq_ignore_ascii_case("fr") => {
                                inner += 1;
                                min = unit_lin(value, &unit).map(LenSpec::Lin);
                            }
                            _ => inner += 1,
                        }
                    }
                    match n {
                        Some(n) => count += n * inner.max(1),
                        None => auto_fill = Some(min.unwrap_or(LenSpec::px(200.0))),
                    }
                    Ok(())
                })?;
            }
            _ => count += 1,
        }
    }
    Ok(match auto_fill {
        Some(min) if count == 0 => GridCols::AutoFill(min, fit),
        _ => GridCols::Count(count.clamp(1, 64) as u16),
    })
}
