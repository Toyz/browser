use super::*;

#[test]
fn is_with_complex_selectors() {
    let (doc, s) = styled(
        r##"<div class="toc"><a href="#x">x</a><b aria-current="true">y</b></div><i>z</i>"##,
        "[aria-current=true]:is(.toc a) { color: red } :is(.toc b)[aria-current=true] { color: blue } :is(.toc i) { color: green }",
    );
    assert_ne!(style_of(&doc, &s, "a").color, Color::Rgba(0xff0000ff), "no aria-current");
    assert_eq!(style_of(&doc, &s, "b").color, Color::Rgba(0x0000ffff));
    assert_ne!(style_of(&doc, &s, "i").color, Color::Rgba(0x008000ff), "not inside .toc");
}

fn styled(html_src: &str, css: &str) -> (Document, Styler) {
    let doc = html::parse(html_src, Url::parse("https://example.com/").unwrap());
    let sheet = parse_stylesheet(css);
    (doc, Styler::new(vec![sheet], MediaEnv::default()))
}

/// Compute styles down the ancestor chain of the first element named `tag`.
fn style_of(doc: &Document, styler: &Styler, tag: &str) -> ComputedStyle {
    let target = doc.find_element(tag).unwrap();
    let mut chain = vec![target];
    while let Some(p) = doc.parent(*chain.last().unwrap()) {
        if doc.element(p).is_none() {
            break;
        }
        chain.push(p);
    }
    let mut style = ComputedStyle::root_parent();
    for id in chain.into_iter().rev() {
        style = styler.compute(doc, id, &style);
    }
    style
}

#[test]
fn css_nesting() {
    let (doc, s) = styled(
        r#"<div class="a"><p class="b">x</p><em>y</em></div><section class="e"><i class="d">z</i></section>"#,
        ".a { color: red; & .b { width: 11px } em { width: 22px } &:hover { width: 1px } } .d { .e & { width: 33px } } \
         .a { @media (min-width: 1px) { & > .b { height: 44px } } }",
    );
    assert_eq!(style_of(&doc, &s, "p").width, Length::Px(11.0));
    assert_eq!(style_of(&doc, &s, "p").height, Length::Px(44.0));
    assert_eq!(style_of(&doc, &s, "em").width, Length::Px(22.0));
    assert_eq!(style_of(&doc, &s, "i").width, Length::Px(33.0));
    assert_eq!(style_of(&doc, &s, "div").color, Color::Rgba(0xff0000ff));
}

#[test]
fn inherit_copies_exactly_the_inherited_properties() {
    // A parent with every inherited property set to something other
    // than its initial value.
    let (doc, s) = styled(
        "<div><p>x</p></div>",
        "div { color: red; font: italic bold 20px/30px serif; text-align: center; text-transform: uppercase; \
         white-space: pre; visibility: hidden; list-style: square; cursor: pointer; pointer-events: none; \
         -webkit-text-fill-color: blue }",
    );
    let parent = style_of(&doc, &s, "div");
    let inherited = ComputedStyle::inherit(&parent);
    let mut from_table = ComputedStyle::initial();
    for m in crate::props::meta::META.iter().filter(|m| m.inherited) {
        (m.copy)(&mut from_table, &parent);
    }
    // What inherits without being a property of its own.
    from_table.underline = parent.underline;
    from_table.strikethrough = parent.strikethrough;
    from_table.dark = parent.dark;
    from_table.query_container = parent.query_container;
    from_table.custom = parent.custom.clone();
    from_table.svg_paint = parent.svg_paint.clone();
    from_table.text_gradient = parent.text_gradient.clone();
    assert_eq!(inherited, from_table);
}

#[test]
fn container_queries() {
    let (doc, s) = styled(
        "<nav><b>i</b><i>w</i><em>n</em></nav>",
        "nav { container-type: inline-size; width: 800px } \
         @container (width < 48rem) { b { display: none } } \
         @container (width < 60rem) { i { display: none } } \
         @container name (min-width: 20rem) { em { color: red } }",
    );
    assert_ne!(style_of(&doc, &s, "b").display, Display::None, "800px isn't under 48rem");
    assert_eq!(style_of(&doc, &s, "i").display, Display::None, "800px is under 60rem");
    assert_ne!(style_of(&doc, &s, "em").color, style_of(&doc, &s, "nav").color, "named container query applied");
}

#[test]
fn container_query_units() {
    let (doc, s) = styled(
        r#"<section><div><span><p>x</p></span></div></section>"#,
        "section { width: 400px } div { container-type: inline-size; width: 50%; padding: 0 } p { font-size: 10cqi; width: 50cqw }",
    );
    let p = style_of(&doc, &s, "p");
    assert_eq!(p.font_size, 20.0);
    assert_eq!(p.width, Length::Px(100.0));
}

#[test]
fn clamp_inside_calc() {
    let (doc, s) = styled(
        r#"<p>x</p><em>y</em>"#,
        "p { --w: calc((clamp(1000px, 100vw, 1069px) - 32px) / 2 + 10px); width: var(--w) } em { width: calc(min(50%, 300px) - 20px) }",
    );
    let vw = MediaEnv::default().width;
    let want = (vw.clamp(1000.0, 1069.0) - 32.0) / 2.0 + 10.0;
    assert_eq!(style_of(&doc, &s, "p").width, Length::Px(want));
    // `min(50%, 300px)` resolves to its fixed side here.
    assert_eq!(style_of(&doc, &s, "em").width, Length::Px(280.0));
}

#[test]
fn gradients_parse_fully() {
    let (doc, s) = styled(
        r#"<p>x</p><em>y</em><b>z</b>"#,
        "p { background: radial-gradient(circle at 30% 30%, #fff, red 60%, blue) } \
         em { background: repeating-linear-gradient(45deg, red 0 10px, blue 10px 20px) no-repeat, linear-gradient(to top right, red, blue) } \
         b { background-image: linear-gradient(red, blue); background-repeat: repeat-x }",
    );
    let p = style_of(&doc, &s, "p");
    let g = &p.gradients.as_ref().unwrap()[0];
    assert_eq!(g.shape, GradientShape::Radial { circle: true, size: RadialSize::FarthestCorner, at: BgPos::at(0.3, 0.3) });
    assert_eq!(g.stops.len(), 3);
    assert_eq!(g.stops[1].pos, Some(StopPos::Fraction(0.6)));
    let em = style_of(&doc, &s, "em");
    let layers = em.gradients.as_ref().unwrap();
    assert_eq!(layers.len(), 2);
    assert!(layers[0].repeating);
    assert_eq!(layers[0].stops.len(), 4, "two positions make two stops");
    assert_eq!(layers[1].shape, GradientShape::LinearCorner(1.0, -1.0));
    assert_eq!(em.background_repeat, BgRepeat { x: false, y: false });
    let b = style_of(&doc, &s, "b");
    assert_eq!(b.gradients.as_ref().unwrap()[0].shape, GradientShape::Linear(180.0));
    assert_eq!(b.background_repeat, BgRepeat { x: true, y: false });
    // The checkerboard idiom, and angle stops.
    let (doc, s) = styled(
        r#"<p>x</p><em>y</em>"#,
        "p { background: repeating-conic-gradient(#222 0% 25%, #111 0% 50%) 0 0 / 20px 20px } \
         em { background: conic-gradient(from 90deg at 25% 75%, red 0deg 90deg, blue 0.5turn) }",
    );
    let p = style_of(&doc, &s, "p");
    let g = &p.gradients.as_ref().unwrap()[0];
    assert!(g.repeating);
    assert_eq!(g.shape, GradientShape::Conic { from: 0.0, at: BgPos::CENTER });
    assert_eq!(g.stops.len(), 4);
    let em = style_of(&doc, &s, "em");
    let g = &em.gradients.as_ref().unwrap()[0];
    assert_eq!(g.shape, GradientShape::Conic { from: 90.0, at: BgPos::at(0.25, 0.75) });
    assert_eq!(g.stops[1].pos, Some(StopPos::Px(90.0)));
    assert_eq!(g.stops[2].pos, Some(StopPos::Px(180.0)));
}

#[test]
fn rotations_and_scales() {
    let (doc, s) = styled(
        r#"<p>x</p><em>y</em><b>z</b><i>w</i>"#,
        "p { rotate: 45deg } em { transform: translate(2px, 3px) rotate(0.25turn) scale(2, 3) } b { transform: matrix(0, 1, -1, 0, 5, 6) } i { transform: rotate(10deg); scale: 1.5 }",
    );
    assert_eq!(style_of(&doc, &s, "p").rotate, 45.0);
    let em = style_of(&doc, &s, "em");
    assert_eq!((em.rotate, em.scale), (90.0, (2.0, 3.0)));
    assert_eq!(em.translate.0.px, 2.0);
    let b = style_of(&doc, &s, "b");
    assert!((b.rotate - 90.0).abs() < 1e-3 && (b.scale.0 - 1.0).abs() < 1e-3, "{:?}", (b.rotate, b.scale));
    let i = style_of(&doc, &s, "i");
    assert_eq!((i.rotate, i.scale), (10.0, (1.5, 1.5)));
}

#[test]
fn specificity_order_and_important() {
    let (doc, s) = styled(
        r#"<p id="x" class="a b">hi</p>"#,
        "#x { color: red } .a.b { color: blue !important } p { color: green; font-size: 2em } .a { margin: 0 auto }",
    );
    let st = style_of(&doc, &s, "p");
    assert_eq!(st.color, Color::Rgba(0x0000ffff), "!important beats the id selector");
    assert_eq!(st.font_size, 32.0);
    assert_eq!(st.margin.left, Length::Auto);
    assert_eq!(st.margin.top, Length::Px(0.0), "author beats UA p margins");
    assert_eq!(st.display, Display::Block, "UA sheet");
}

#[test]
fn inline_style_and_hints() {
    let (doc, s) = styled(
        r##"<table bgcolor="ff6600"><tr><td><span style="color: #00f; font-weight: 700">x</span></td></tr></table>"##,
        "",
    );
    let span = style_of(&doc, &s, "span");
    assert_eq!(span.color, Color::Rgba(0x0000ffff));
    assert_eq!(span.font_weight, 700);
    let table = style_of(&doc, &s, "table");
    assert_eq!(table.background, Some(Color::Rgba(0xff6600ff)));
}

#[test]
fn custom_properties_and_var() {
    let (doc, s) = styled(
        r#"<div><p>x</p></div>"#,
        ":root { --gap: 12px; --fg: #123456 } div { --fg2: var(--fg) } p { color: var(--fg2); padding: var(--gap) var(--missing, 3px); margin: var(--nope) }",
    );
    let p = style_of(&doc, &s, "p");
    assert_eq!(p.color, Color::Rgba(0x123456ff));
    assert_eq!(p.padding.top, Length::Px(12.0));
    assert_eq!(p.padding.right, Length::Px(3.0));
    // Unresolvable var: invalid at computed-value time, so `unset` (the
    // initial 0), not the UA's 1em.
    assert_eq!(p.margin.top, Length::Px(0.0));
}

#[test]
fn em_rem_and_inheritance() {
    let (doc, s) = styled(
        r#"<div><p><b>x</b></p></div>"#,
        "html { font-size: 10px } div { font-size: 2em; color: red } p { padding-left: 1em; margin-left: 2rem; line-height: 1.5 }",
    );
    let p = style_of(&doc, &s, "p");
    assert_eq!(p.font_size, 20.0);
    assert_eq!(p.padding.left, Length::Px(20.0));
    assert_eq!(p.margin.left, Length::Px(20.0));
    let b = style_of(&doc, &s, "b");
    assert_eq!(b.color, Color::Rgba(0xff0000ff), "color inherits");
    assert_eq!(b.line_height, LineHeight::Number(1.5), "numbers inherit as numbers");
    assert!(b.is_bold(), "UA b");
}

#[test]
fn structural_selectors() {
    let (doc, s) = styled(
        "<ul><li>a</li><li>b</li></ul><section><h2>t</h2></section>",
        "li:first-child { color: red } li + li { color: blue } section > h2:is(h1, h2) { display: none } ul:has(> li) { padding-left: 0 }",
    );
    let first = style_of(&doc, &s, "li");
    assert_eq!(first.color, Color::Rgba(0xff0000ff));
    assert_eq!(style_of(&doc, &s, "h2").display, Display::None);
    assert_eq!(style_of(&doc, &s, "ul").padding.left, Length::Px(0.0));
    assert_eq!(style_of(&doc, &s, "ul").list_style, ListStyle::Disc);
}

#[test]
fn link_style_and_decoration() {
    let (doc, s) = styled(r#"<a href="/x">l</a><p>t</p>"#, "a { text-decoration: none }");
    let a = style_of(&doc, &s, "a");
    assert_eq!(a.color, Color::Link);
    assert!(!a.underline, "author none beats UA underline");
}

#[test]
fn border_shorthand_and_currentcolor() {
    let (doc, s) = styled("<div>x</div>", "div { color: #f00; border: 2px solid; border-left: none }");
    let d = style_of(&doc, &s, "div");
    assert_eq!(d.border_width.top, 2.0);
    assert_eq!(d.border_width.left, 0.0);
    assert_eq!(d.border_color.top, Color::Rgba(0xff0000ff));
}

#[test]
fn before_and_after_content() {
    let (doc, s) = styled(
        r#"<span class="edit" title="T">edit</span><p>plain</p>"#,
        r#".edit::before { content: "[" } .edit::after { content: "]" attr(title); color: red } p::after { content: none }"#,
    );
    let span_id = doc.find_element("span").unwrap();
    let span = style_of(&doc, &s, "span");
    let before = s.compute_pseudo(&doc, span_id, &span, Pseudo::Before).expect("::before");
    assert_eq!(before.content.as_deref(), Some(&[ContentItem::Str("[".into())][..]));
    let after = s.compute_pseudo(&doc, span_id, &span, Pseudo::After).expect("::after");
    assert_eq!(after.color, Color::Rgba(0xff0000ff));
    assert_eq!(after.content.as_ref().unwrap().len(), 2);
    let p_id = doc.find_element("p").unwrap();
    let p = style_of(&doc, &s, "p");
    assert!(s.compute_pseudo(&doc, p_id, &p, Pseudo::After).is_none());
    assert!(s.compute_pseudo(&doc, p_id, &p, Pseudo::Before).is_none());
}

#[test]
fn quirks_mode_tables_reset_text_align() {
    let (doc, s) = styled("<center><table><tr><td>x</td></tr></table></center>", "");
    assert!(doc.quirks);
    assert_eq!(style_of(&doc, &s, "td").text_align, TextAlign::Left);
    let (doc, s) = styled("<!DOCTYPE html><center><table><tr><td>x</td></tr></table></center>", "");
    assert_eq!(style_of(&doc, &s, "td").text_align, TextAlign::Center);
}

#[test]
fn media_queries_reevaluate_per_viewport() {
    let doc = html::parse("<p>x</p>", Url::parse("https://example.com/").unwrap());
    let data = StyleData::from_sheets(
        vec![parse_stylesheet("p { color: red } @media (max-width: 600px) { p { color: blue } }")],
        None,
    );
    let narrow = MediaEnv { width: 400.0, ..Default::default() };
    let wide = MediaEnv { width: 1200.0, ..Default::default() };
    assert_ne!(data.signature(&narrow), data.signature(&wide));
    let color = |env| {
        let styler = data.styler(env);
        let p = doc.find_element("p").unwrap();
        let mut style = ComputedStyle::root_parent();
        for id in [doc.find_element("html").unwrap(), doc.find_element("body").unwrap(), p] {
            style = styler.compute(&doc, id, &style);
        }
        style.color
    };
    assert_eq!(color(narrow), Color::Rgba(0x0000ffff));
    assert_eq!(color(wide), Color::Rgba(0xff0000ff));
}

#[test]
fn hover_state_matches() {
    let doc = html::parse(r#"<nav><a href="/x">x</a></nav>"#, Url::parse("https://example.com/").unwrap());
    let data = StyleData::from_sheets(vec![parse_stylesheet("a:hover { color: red } nav:hover a { font-weight: 700 }")], None);
    assert!(data.has_state_rules());
    let a = doc.find_element("a").unwrap();
    let style = |state: ElementState| {
        let styler = data.styler_with_state(MediaEnv::default(), Arc::new(state));
        let mut st = ComputedStyle::root_parent();
        let mut chain = vec![a];
        while let Some(p) = doc.parent(*chain.last().unwrap()) {
            if doc.element(p).is_none() {
                break;
            }
            chain.push(p);
        }
        for id in chain.into_iter().rev() {
            st = styler.compute(&doc, id, &st);
        }
        st
    };
    let idle = style(ElementState::default());
    assert_eq!(idle.color, Color::Link);
    let mut hovered = ElementState::default();
    let mut node = Some(a);
    while let Some(n) = node {
        hovered.hovered.insert(n);
        node = doc.parent(n);
    }
    let hot = style(hovered);
    assert_eq!(hot.color, Color::Rgba(0xff0000ff));
    assert_eq!(hot.font_weight, 700);
}

#[test]
fn collects_sources_in_order() {
    let doc = html::parse(
        r#"<link rel="stylesheet" href="/a.css"><style>p{}</style><link rel="alternate stylesheet" href="/b.css"><link rel=icon href=/i>"#,
        Url::parse("https://example.com/dir/").unwrap(),
    );
    let sources = collect_style_sources(&doc);
    assert_eq!(sources.len(), 2);
    assert!(matches!(&sources[0], StyleSource::External { url, .. } if url.as_str() == "https://example.com/a.css"));
    assert!(matches!(&sources[1], StyleSource::Inline { css, .. } if css == "p{}"));
}

#[test]
fn undefined_variables_unset_the_property() {
    // Invalid at computed-value time: `color` inherits, `width` goes back to
    // its initial value; neither keeps what an earlier rule said.
    let (doc, s) = styled(
        "<div><a href=x>a</a></div>",
        "div { color: rgb(1, 2, 3) } a { color: blue; width: 10px } a { color: var(--nope); width: var(--nope) }",
    );
    let a = style_of(&doc, &s, "a");
    assert_eq!(a.color, style_of(&doc, &s, "div").color);
    assert_eq!(a.width, Length::Auto);
}


#[test]
fn cascade_layers() {
    let red = Color::Rgba(0xff0000ff);
    let green = Color::Rgba(0x008000ff);
    let color = |css: &str| {
        let (doc, s) = styled("<nav data-v=n><a class=i>x</a></nav>", css);
        style_of(&doc, &s, "a").color
    };
    // Unlayered beats a layer, whatever the specificity (GitHub's
    // breadcrumbs: Primer's rules are in `@layer primer-react`).
    assert_eq!(color("@layer lib { [data-v=n] .i { color: red } } .i { color: green }"), green);
    // Later layers win; `@layer a, b;` fixes the order up front.
    assert_eq!(color("@layer b, a; @layer a { .i { color: green } } @layer b { .i { color: red } }"), green);
    assert_eq!(color("@layer a { .i { color: red } } @layer b { .i { color: green } }"), green);
    // A layer's own rules beat its nested layers.
    assert_eq!(color("@layer a { .i { color: green } @layer inner { .i { color: red } } }"), green);
    // !important: the earlier layer wins.
    assert_eq!(color("@layer a { .i { color: green !important } } @layer b { .i { color: red !important } }"), green);
    assert_eq!(color("@layer a { .i { color: green !important } } .i { color: red !important }"), green);
    let _ = red;
}

#[test]
fn registered_properties_have_initial_values() {
    // Tailwind v4: borders are `border-style: var(--tw-border-style)`, with
    // the default from `@property` (no fallback set anywhere else).
    let (doc, s) = styled(
        r#"<div class="p"><p class="b">x</p></div>"#,
        "@property --tw-border-style { syntax: \"*\"; inherits: false; initial-value: solid }
         @property --shift { syntax: \"*\"; inherits: false; initial-value: 0px }
         .b { border-width: 1px; border-style: var(--tw-border-style); margin-left: var(--shift) }
         .p { --shift: 20px }",
    );
    let p = style_of(&doc, &s, "p");
    assert_eq!(p.border_style.top, BorderStyle::Solid);
    // Not inherited: the child starts over at the initial value.
    assert_eq!(p.margin.left, Length::Px(0.0));
}

#[test]
fn font_inherit_beats_earlier_rules_but_not_later_sizes() {
    let (doc, s) = styled(
        r#"<div class="m"><button class="b x">go</button></div>"#,
        "button { font-family: serif } .m { font-family: monospace; font-size: 20px }
         button.b { font: inherit } .x.x.x { font-size: 12px }",
    );
    let b = style_of(&doc, &s, "button");
    assert_eq!(&*b.font_names[0], "monospace");
    assert_eq!(b.font_size, 12.0);
}

#[test]
fn place_items_sets_both_axes_and_small_caps_parse() {
    let (doc, s) = styled(r#"<div><p>x</p></div>"#, "div { place-items: center } p { font-variant: small-caps }");
    let d = style_of(&doc, &s, "div");
    assert_eq!((d.align_items, d.justify_items), (Some(Align::Center), Some(Align::Center)));
    assert_eq!(style_of(&doc, &s, "p").small_caps, Some(false));
}

#[test]
fn any_transform_but_none_is_noted() {
    let (doc, s) = styled(r#"<div><p>x</p><span>y</span><b>z</b></div>"#, "div { transform: matrix(1, 0, 0, 1, 0, 0) } span { transform: none } b { transform: translateX(2px) }");
    assert!(style_of(&doc, &s, "div").transformed, "an identity matrix is still a transform");
    assert!(!style_of(&doc, &s, "p").transformed, "not inherited");
    assert!(!style_of(&doc, &s, "span").transformed);
    assert!(style_of(&doc, &s, "b").transformed);
}
