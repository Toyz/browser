use super::*;

fn parse(name: &str, value: &str) -> Vec<Decl> {
    let mut out = Vec::new();
    parse_declaration(name, value, &mut out);
    out
}

#[test]
fn colors() {
    assert_eq!(parse("color", "#ff6600"), vec![Decl::Color(Color::Rgba(0xff6600ff))]);
    assert_eq!(parse("color", "rgb(0 128 255 / 50%)"), vec![Decl::Color(Color::Rgba(0x0080ff80))]);
    assert_eq!(parse("color", "hsl(0 100% 50%)"), vec![Decl::Color(Color::Rgba(0xff0000ff))]);
    assert_eq!(parse("color", "LinkText"), vec![Decl::Color(Color::Link)]);
    assert_eq!(parse("color", "light-dark(#111, #eee)"), vec![Decl::Color(Color::LightDark(0x111111ff, 0xeeeeeeff))]);
    // oklch(62.8% 0.2577 29.23) ≈ pure red
    let Decl::Color(Color::Rgba(v)) = parse("color", "oklch(62.8% 0.2577 29.23)")[0] else { panic!() };
    assert!((v >> 24) > 0xf0 && ((v >> 16) & 0xff) < 0x10, "{v:08x}");
    assert!(parse("color", "nonsense").is_empty());
    // The full CSS named-color table (via cssparser-color).
    assert_eq!(parse("color", "rebeccapurple"), vec![Decl::Color(Color::Rgba(0x663399ff))]);
    assert_eq!(parse("color", "LightGoldenrodYellow"), vec![Decl::Color(Color::Rgba(0xfafad2ff))]);
    assert_eq!(parse("background-color", "transparent"), vec![Decl::BackgroundColor(Color::TRANSPARENT)]);
}

#[test]
fn lengths_and_calc() {
    assert_eq!(parse("width", "10px"), vec![Decl::Width(LenSpec::px(10.0))]);
    assert_eq!(parse("width", "50%"), vec![Decl::Width(LenSpec::Lin(Lin::pct(50.0)))]);
    assert_eq!(
        parse("width", "calc(2rem + 4px * 2)"),
        vec![Decl::Width(LenSpec::Lin(Lin { rem: 2.0, px: 8.0, ..Default::default() }))]
    );
    assert!(matches!(parse("max-width", "min(100%, 60rem)")[0], Decl::MaxWidth(LenSpec::Min(_))));
    assert!(parse("width", "10 px").is_empty());
}

#[test]
fn shorthands() {
    let m = parse("margin", "0 auto");
    assert_eq!(m.len(), 4);
    assert_eq!(m[1], Decl::Margin(Side::Right, LenSpec::Auto));
    assert_eq!(m[2], Decl::Margin(Side::Bottom, LenSpec::px(0.0)));

    let b = parse("border-bottom", "1px solid #ccc");
    assert_eq!(b, vec![
        Decl::BorderWidth(Side::Bottom, LenSpec::px(1.0)),
        Decl::BorderStyle(Side::Bottom, BorderStyle::Solid),
        Decl::BorderColor(Side::Bottom, Color::Rgba(0xccccccff)),
    ]);

    let f = parse("font", "italic bold 12px/1.5 Georgia, serif");
    assert!(f.contains(&Decl::Italic(true)));
    assert!(f.contains(&Decl::FontWeight(WeightSpec::Abs(700))));
    assert!(f.contains(&Decl::LineHeight(LineHeightSpec::Number(1.5))));
    assert!(f.contains(&Decl::FontFamily(FontFamily::Serif)));

    assert_eq!(parse("flex", "1"), vec![Decl::FlexGrow(1.0), Decl::FlexShrink(1.0), Decl::FlexBasis(LenSpec::Lin(Lin::pct(0.0)))]);
    let bg = with_base(Some(&url::Url::parse("https://e.com/css/site.css").unwrap()), || {
        parse("background", "url(\"../img/x.png\") no-repeat center / 10px #fafafa")
    });
    assert_eq!(bg[0], Decl::BackgroundColor(Color::Rgba(0xfafafaff)));
    assert!(bg.contains(&Decl::BackgroundUrl(Some("https://e.com/img/x.png".into()))));
    assert!(bg.contains(&Decl::BackgroundRepeat(BgRepeat { x: false, y: false })));
    assert!(bg.contains(&Decl::BackgroundSize(BgSizeSpec::Explicit(LenSpec::px(10.0), LenSpec::Auto))));
    assert!(bg.contains(&Decl::BackgroundPosition(BgPos::CENTER)));
    let mask = parse("mask-image", "url(data:image/svg+xml,%3Csvg%3E)");
    assert_eq!(mask[0], Decl::Masked(true));
    assert!(matches!(&mask[1], Decl::MaskUrl(Some(u)) if u.starts_with("data:image/svg+xml")));
}

#[test]
fn grid_columns() {
    // The summary the row-based layout reads, then the full value.
    let starts = |name: &str, value: &str, want: Vec<Decl>| {
        let got = parse(name, value);
        assert_eq!(&got[..want.len()], &want[..], "{got:?}");
        got
    };
    let full = starts(
        "grid-template-columns",
        "1fr 2fr [end] 100px",
        vec![
            Decl::GridColumns(GridCols::Count(3)),
            Decl::GridContentMin(true),
            Decl::GridTracks(Some(vec![TrackSpec::Fr(1.0), TrackSpec::Fr(2.0), TrackSpec::Len(LenSpec::px(100.0))].into())),
        ],
    );
    let Some(Decl::GridTemplateFull { rows: false, value: Some(t) }) = full.last() else { panic!("{full:?}") };
    assert_eq!(t.entries.len(), 3);
    assert_eq!(t.names[2], vec![Arc::<str>::from("end")]);
    // Equal columns don't need tracks.
    starts(
        "grid-template-columns",
        "repeat(4, minmax(0, 1fr))",
        vec![Decl::GridColumns(GridCols::Count(4)), Decl::GridContentMin(false), Decl::GridTracks(None)],
    );
    // `1fr` alone never gets narrower than its content.
    starts("grid-template-columns", "repeat(6, 1fr)", vec![Decl::GridColumns(GridCols::Count(6)), Decl::GridContentMin(true), Decl::GridTracks(None)]);
    assert_eq!(
        parse("flex", "1 1 0"),
        vec![Decl::FlexGrow(1.0), Decl::FlexShrink(1.0), Decl::FlexBasis(LenSpec::px(0.0))]
    );
    starts(
        "grid-template-columns",
        "repeat(auto-fill, minmax(200px, 1fr))",
        vec![Decl::GridColumns(GridCols::AutoFill(LenSpec::px(200.0), false)), Decl::GridContentMin(false), Decl::GridTracks(None)],
    );
}

#[test]
fn grid_placements() {
    let d = parse("grid-column", "main-column-start / main-column-end");
    assert!(d.contains(&Decl::GridPlace { row: false, end: false, line: GridLine::Named("main-column-start".into(), 0) }), "{d:?}");
    assert!(d.contains(&Decl::GridPlace { row: false, end: true, line: GridLine::Named("main-column-end".into(), 0) }), "{d:?}");
    let d = parse("grid-column", "content");
    assert!(d.contains(&Decl::GridPlace { row: false, end: true, line: GridLine::Named("content".into(), 0) }), "{d:?}");
    let d = parse("grid-column", "2 content");
    assert!(d.contains(&Decl::GridPlace { row: false, end: false, line: GridLine::Named("content".into(), 2) }), "{d:?}");
    assert!(d.contains(&Decl::GridPlace { row: false, end: true, line: GridLine::Auto }), "{d:?}");
    let d = parse("grid-row", "1");
    assert!(d.contains(&Decl::GridPlace { row: true, end: false, line: GridLine::Line(1) }), "{d:?}");
    let d = parse("grid-row", "span 2 / -1");
    assert!(d.contains(&Decl::GridPlace { row: true, end: true, line: GridLine::Line(-1) }), "{d:?}");
    assert!(d.contains(&Decl::GridPlace { row: true, end: false, line: GridLine::Span(2) }), "{d:?}");
    // Integers by calc() (Google Workspace's columns, from custom properties).
    let d = parse("grid-column-end", "span calc(11 - 2 + 1)");
    assert!(d.contains(&Decl::GridPlace { row: false, end: true, line: GridLine::Span(10) }), "{d:?}");
    let d = parse("grid-column-start", "calc((3 * 2) / 2)");
    assert!(d.contains(&Decl::GridPlace { row: false, end: false, line: GridLine::Line(3) }), "{d:?}");
}

#[test]
fn grid_templates() {
    let d = parse("grid-template", "min-content 1fr / 12.25rem minmax(0,1fr)");
    assert!(d.contains(&Decl::GridTracks(Some(
        vec![TrackSpec::Len(LenSpec::Lin(Lin { rem: 12.25, ..Default::default() })), TrackSpec::Fr(1.0)].into()
    ))));
    // The shorthand gives both track lists in full too (Primer's
    // ActionList items: areas with a row size, then the columns).
    let full = parse("grid-template", "\"spacer leadingAction leadingVisual content\"min-content/min-content min-content min-content minmax(0,auto)");
    let rows = full.iter().find_map(|d| match d { Decl::GridTemplateFull { rows: true, value } => Some(value.clone()), _ => None });
    let cols = full.iter().find_map(|d| match d { Decl::GridTemplateFull { rows: false, value } => Some(value.clone()), _ => None });
    assert_eq!(rows.flatten().map(|t| t.entries.len()), Some(1), "{full:?}");
    assert_eq!(cols.flatten().map(|t| t.entries.len()), Some(4), "{full:?}");
    assert!(full.iter().any(|d| matches!(d, Decl::GridAreas(Some(a)) if a[0].len() == 4)), "{full:?}");
    let areas = parse("grid-template-areas", "'a a' 'b c'");
    let Decl::GridAreas(Some(rows)) = &areas[0] else { panic!("{areas:?}") };
    assert_eq!(rows.len(), 2);
    assert_eq!(&*rows[1][1], "c");
    let area = parse("grid-area", "pageContent");
    assert_eq!(area[0], Decl::GridArea(Some("pageContent".into())));
    assert!(area.contains(&Decl::GridPlace { row: true, end: true, line: GridLine::Named("pageContent".into(), 0) }), "{area:?}");
}

#[test]
fn var_and_wide_keywords_are_deferred() {
    assert!(matches!(&parse("color", "var(--x, red)")[0], Decl::Unresolved { name, .. } if &**name == "color"));
    assert_eq!(parse("color", "inherit"), vec![Decl::Wide("color".into(), Wide::Inherit)]);
    assert_eq!(parse("--Brand", " #fff "), vec![Decl::Custom("--Brand".into(), "#fff".into())]);
}

#[test]
fn display_and_font_family() {
    assert_eq!(parse("display", "inline-flex"), vec![Decl::Display(Display::InlineFlex)]);
    assert_eq!(
        parse("content", r#"" [" attr(title) "\f007" open-quote"#),
        vec![Decl::Content(Some(vec![
            ContentItem::Str(" [".into()),
            ContentItem::Attr("title".into()),
            ContentItem::Str("".into()),
            ContentItem::OpenQuote,
        ].into()))]
    );
    assert_eq!(parse("content", "none"), vec![Decl::Content(None)]);
    assert_eq!(parse("display", "flow-root"), vec![Decl::Display(Display::Block)]);
    let serif = parse("font-family", "'Linux Libertine', 'Georgia', Times, serif");
    assert_eq!(serif[0], Decl::FontFamily(FontFamily::Serif));
    assert_eq!(
        serif[1],
        Decl::FontNames(vec!["Linux Libertine".into(), "Georgia".into(), "Times".into(), "serif".into()].into())
    );
    assert_eq!(parse("font-family", "ui-monospace, SFMono-Regular, Menlo")[0], Decl::FontFamily(FontFamily::Mono));
}

#[test]
fn minmax_tracks_keep_a_length_minimum() {
    // MDN's layout: sidebars at least 15rem, the article up to 48rem.
    let d = parse("grid-template-columns", "minmax(240px, 1fr) 32px minmax(0, 768px) 32px minmax(15rem, 2fr)");
    assert!(d.contains(&Decl::GridTracks(Some(
        vec![
            TrackSpec::FrMin(1.0, LenSpec::px(240.0)),
            TrackSpec::Len(LenSpec::px(32.0)),
            TrackSpec::Max(LenSpec::px(768.0)),
            TrackSpec::Len(LenSpec::px(32.0)),
            TrackSpec::FrMin(2.0, LenSpec::Lin(Lin { rem: 15.0, ..Default::default() })),
        ]
        .into()
    ))), "{d:?}");
}
