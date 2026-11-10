//! Dev tool: fetch a page and print its layout rows, with timings.
//! `cargo run --example dump -- https://example.com [max_rows]`

use std::time::Instant;

use render::boxes::LayoutBox;

fn describe(b: &LayoutBox) -> String {
    match b {
        LayoutBox::Paragraph(p) => {
            let text: String = p.text.chars().take(70).collect();
            format!("P {:>4.0}px links={} {:?}", p.style.font_size, p.links.len(), text)
        }
        LayoutBox::Block(b) => format!("{:?} children={}", b.kind, b.children.len()),
        LayoutBox::Image(i) => format!("IMG {}", i.src),
    }
}

fn count(b: &LayoutBox) -> usize {
    match b {
        LayoutBox::Paragraph(_) | LayoutBox::Image(_) => 1,
        LayoutBox::Block(b) => 1 + b.children.iter().map(count).sum::<usize>(),
    }
}

/// Print the structure of big blocks (to see what stops rows from splitting).
fn tree(b: &LayoutBox, depth: usize, max_depth: usize) {
    if let LayoutBox::Block(block) = b {
        let s = &block.style;
        println!(
            "{:indent$}{:?} boxes={} grid={:?} h={:?} minh={:?} ovf={:?} bw={:?} radius={:?} w={:?} maxw={:?}",
            "",
            block.kind,
            count(b),
            s.grid_columns,
            s.height,
            s.min_height,
            s.overflow,
            (s.border_width.top, s.border_width.bottom),
            s.border_radius[0],
            s.width,
            s.max_width,
            indent = depth * 2
        );
        if depth < max_depth {
            for child in block.children.iter().filter(|c| count(c) > 20) {
                tree(child, depth + 1, max_depth);
            }
        }
    }
}

fn image_line(s: &style::ComputedStyle) -> String {
    format!("w={:?} h={:?} opacity={} visible={} display={:?} pos={:?}", s.width, s.height, s.opacity, s.visible, s.display, s.position)
}

fn style_line(s: &style::ComputedStyle) -> String {
    format!(
        "display={:?} pos={:?} w={:?} h={:?} minh={:?} maxh={:?} pad={:?} margin={:?} inset={:?} translate={:?} flex={:?}/{:?} lh={:?} fs={} color={:?} weight={}",
        s.display, s.position, s.width, s.height, s.min_height, s.max_height, s.padding, s.margin, s.inset, s.translate,
        s.flex_direction, s.align_items, s.line_height, s.font_size, s.color, s.font_weight
    )
}

/// Print the boxes of elements with this class, and what's inside them.
fn by_class(b: &LayoutBox, doc: &page::Document, class: &str, inside: bool, depth: usize) {
    let has = |n: Option<page::NodeId>| {
        n.and_then(|n| doc.element(n)).and_then(|e| e.attr("class")).is_some_and(|c| c.split_whitespace().any(|c| c == class))
    };
    match b {
        LayoutBox::Block(block) => {
            let here = inside || has(block.node);
            if here {
                let tag = block.node.and_then(|n| doc.element(n)).map(|e| e.name.local.to_string()).unwrap_or_default();
                let cls = block.node.and_then(|n| doc.element(n)).and_then(|e| e.attr("class")).unwrap_or_default().to_owned();
                println!("{:indent$}<{tag} .{cls}> {:?} {}", "", std::mem::discriminant(&block.kind), style_line(&block.style), indent = depth * 2);
            }
            for c in &block.children {
                by_class(c, doc, class, here, if here { depth + 1 } else { depth });
            }
        }
        LayoutBox::Paragraph(p) => {
            if inside {
                println!("{:indent$}P {:?} objects={}", "", p.text.chars().take(40).collect::<String>(), p.objects.len(), indent = depth * 2);
            }
            for o in &p.objects {
                match o {
                    render::boxes::InlineObject::BlockBox(bb) => by_class(&LayoutBox::Block((**bb).clone()), doc, class, inside, depth + 1),
                    render::boxes::InlineObject::Image(i) if inside => {
                        println!("{:indent$}inline img {} {}", "", i.src, image_line(&i.style), indent = (depth + 1) * 2)
                    }
                    _ => {}
                }
            }
        }
        LayoutBox::Image(i) if inside => println!("{:indent$}img {} {}", "", i.src, image_line(&i.style), indent = depth * 2),
        _ => {}
    }
}

/// Print every paragraph containing `needle`, with its text styles.
fn find(b: &LayoutBox, needle: &str) {
    match b {
        LayoutBox::Paragraph(p) if p.text.contains(needle) => {
            println!("FOUND {:?}", p.text.chars().take(60).collect::<String>());
            println!("  block: size {} line-height {:?} family {:?}", p.style.font_size, p.style.line_height, p.style.font_names);
            for span in &p.spans {
                println!("  span len {}: size {} lh {:?} weight {}", span.len, span.style.font_size, span.style.line_height, span.style.font_weight);
            }
        }
        LayoutBox::Block(block) => block.children.iter().for_each(|c| find(c, needle)),
        _ => {}
    }
}

fn main() -> anyhow::Result<()> {
    // `dump <url> [<url>...] [max_rows]`: several URLs load in one process,
    // which shows the effect of the subresource cache on later pages.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let max_rows: usize = args.last().and_then(|n| n.parse().ok()).unwrap_or(25);
    let urls: Vec<_> = args.iter().filter(|a| a.parse::<usize>().is_err()).collect();
    assert!(!urls.is_empty(), "usage: dump <url> [<url>...] [max_rows]");
    for url in urls {
        dump(&net::parse_input(url)?, max_rows)?;
    }
    Ok(())
}

fn dump(url: &url::Url, max_rows: usize) -> anyhow::Result<()> {
    let url = url.clone();
    let t = Instant::now();
    let response = net::fetch(&url)?;
    let fetched = t.elapsed();
    let t = Instant::now();
    let doc = html::parse(&response.body, response.url.clone());
    let parsed = t.elapsed();
    let t = Instant::now();
    let fetch = |u: &url::Url| net::fetch_text_cached(u);
    let styler = style::StyleData::for_document(&doc, &fetch).styler(style::MediaEnv {
        width: std::env::var("WIDTH").ok().and_then(|w| w.parse().ok()).unwrap_or(1100.0),
        ..Default::default()
    });
    let styled = t.elapsed();
    let t = Instant::now();
    let layout = render::build(&doc, &styler);
    let built = t.elapsed();

    println!(
        "{} → {} | {} bytes, {} nodes, {} rows, {} anchors | fetch {:?} parse {:?} css {:?} style+layout {:?}",
        url,
        doc.title().unwrap_or_default(),
        response.body.len(),
        doc.len(),
        layout.rows.len(),
        layout.anchors.len(),
        fetched,
        parsed,
        styled,
        built
    );
    if let Ok(class) = std::env::var("CLASS") {
        for row in &layout.rows {
            for w in row.wrappers.iter() {
                if let Some(n) = w.node
                    && doc.element(n).and_then(|e| e.attr("class")).is_some_and(|c| c.split_whitespace().any(|c| c == class))
                {
                    println!("WRAPPER {class}: {}", style_line(&w.style));
                }
            }
            by_class(&row.content, &doc, &class, false, 0);
            row.absolutes.iter().for_each(|b| by_class(b, &doc, &class, false, 0));
        }
    }
    if let Ok(needle) = std::env::var("FIND") {
        for row in &layout.rows {
            find(&row.content, &needle);
        }
        for b in &layout.fixed {
            find(b, &needle);
        }
    }
    for b in &layout.fixed {
        if let LayoutBox::Block(bl) = b {
            let tag = bl.node.and_then(|n| doc.element(n)).map(|e| format!("<{} id={:?} class={:?}>", e.local_name(), e.attr("id"), e.attr("class"))).unwrap_or_default();
            println!("fixed {tag} bg={:?} opacity={} visible={} inset={:?} w={:?} h={:?}", bl.style.painted_background(), bl.style.opacity, bl.style.visible, bl.style.inset, bl.style.width, bl.style.height);
        }
    }
    // IMAGES=1: the images the tab will load (row images, then fixed ones).
    if std::env::var("IMAGES").is_ok() {
        for (url, rows) in &layout.images {
            println!("image rows={rows:?} {url}");
        }
        for url in &layout.fixed_images {
            println!("image fixed {url}");
        }
    }
    if std::env::var("TREE").is_ok() {
        for row in layout.rows.iter().filter(|r| count(&r.content) > 50) {
            tree(&row.content, 0, 8);
        }
    }
    if std::env::var("WRAPPERS").is_ok() {
        for (ix, row) in layout.rows.iter().take(max_rows).enumerate() {
            for w in row.wrappers.iter() {
                let s = &w.style;
                println!("{ix:>4} wrapper bg={:?} w={:?} maxw={:?} m=({:?},{:?}) p=({:?},{:?})", s.painted_background(), s.width, s.max_width, s.margin.left, s.margin.right, s.padding.left, s.padding.right);
            }
            if let LayoutBox::Block(b) = &row.content {
                let s = &b.style;
                println!("{ix:>4} content {:?} bg={:?} w={:?} maxw={:?} m=({:?},{:?}) disp={:?}", b.kind, s.painted_background(), s.width, s.max_width, s.margin.left, s.margin.right, s.display);
            }
        }
    }
    for (ix, row) in layout.rows.iter().take(max_rows).enumerate() {
        println!(
            "{ix:>4} +{:>3.0} ⇥{:>3.0} w{} {}",
            row.space_before,
            row.indent_left,
            row.wrappers.len(),
            describe(&row.content)
        );
    }
    Ok(())
}
