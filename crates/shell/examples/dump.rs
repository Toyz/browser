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

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = net::parse_input(&args.next().expect("usage: dump <url> [max_rows]"))?;
    let max_rows: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(25);

    let t = Instant::now();
    let response = net::fetch(&url)?;
    let fetched = t.elapsed();
    let t = Instant::now();
    let doc = html::parse(&response.body, response.url.clone());
    let parsed = t.elapsed();
    let t = Instant::now();
    let fetch = |u: &url::Url| net::fetch(u).ok().filter(|r| (200..300).contains(&r.status)).map(|r| r.body);
    let styler = style::StyleData::for_document(&doc, &fetch).styler(style::MediaEnv::default());
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
