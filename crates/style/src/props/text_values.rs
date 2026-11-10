//! Fonts, generated content, list markers and text decoration.

use super::*;

/// Icon fonts put glyphs in the Private Use Area; without the font they're tofu.
pub(crate) fn strip_private_use(s: &str) -> String {
    s.chars().filter(|&c| !matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0x10FFFF)).collect()
}

pub(crate) fn parse_content<'i>(p: &mut Parser<'i>) -> PResult<'i, Option<Arc<[ContentItem]>>> {
    if p.try_parse(|p| p.expect_ident_matching("none").or_else(|_| p.expect_ident_matching("normal"))).is_ok() {
        return Ok(None);
    }
    let mut items = Vec::new();
    while !p.is_exhausted() {
        match p.next()?.clone() {
            Token::QuotedString(s) => items.push(ContentItem::Str(strip_private_use(&s).into())),
            Token::Ident(id) => match id.to_ascii_lowercase().as_str() {
                "open-quote" => items.push(ContentItem::OpenQuote),
                "close-quote" => items.push(ContentItem::CloseQuote),
                _ => {} // no-open-quote etc.
            },
            Token::Function(f) if f.eq_ignore_ascii_case("attr") => {
                let name = p.parse_nested_block(|p| {
                    let name = p.expect_ident()?.to_ascii_lowercase();
                    while p.next().is_ok() {}
                    Ok(name)
                })?;
                items.push(ContentItem::Attr(name.into()));
            }
            // `content: "x" / "alt text"`: the alt part is for screen readers.
            Token::Delim('/') => {
                while p.next().is_ok() {}
            }
            _ => {} // counter(), url(), image-set()...: not supported yet
        }
    }
    Ok(Some(items.into()))
}

pub(crate) fn parse_list_style_type<'i>(p: &mut Parser<'i>) -> PResult<'i, ListStyle> {
    keyword(p, &[
        ("none", ListStyle::None),
        ("disc", ListStyle::Disc),
        ("circle", ListStyle::Circle),
        ("square", ListStyle::Square),
        ("decimal", ListStyle::Decimal),
        ("decimal-leading-zero", ListStyle::Decimal),
        ("lower-alpha", ListStyle::LowerAlpha),
        ("lower-latin", ListStyle::LowerAlpha),
        ("upper-alpha", ListStyle::UpperAlpha),
        ("upper-latin", ListStyle::UpperAlpha),
        ("lower-roman", ListStyle::LowerRoman),
        ("upper-roman", ListStyle::UpperRoman),
        ("disclosure-open", ListStyle::Disc),
        ("disclosure-closed", ListStyle::Disc),
    ])
}

pub(crate) fn parse_text_decoration<'i>(p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let mut underline = false;
    let mut strikethrough = false;
    while !p.is_exhausted() {
        // Colors and thickness are ignored; so are `none`, `overline` and styles.
        if let Token::Ident(id) = p.next()?.clone() {
            match id.to_ascii_lowercase().as_str() {
                "underline" => underline = true,
                "line-through" => strikethrough = true,
                _ => {}
            }
        }
    }
    out.push(Decl::TextDecoration { underline, strikethrough });
    Ok(())
}

// ---------------------------------------------------------------------------
// Borders

pub(crate) fn parse_weight<'i>(p: &mut Parser<'i>) -> PResult<'i, WeightSpec> {
    if let Ok(n) = p.try_parse(|p| p.expect_number()) {
        return if (1.0..=1000.0).contains(&n) { Ok(WeightSpec::Abs(n as u16)) } else { err(p) };
    }
    Ok(match ident(p)?.as_str() {
        "normal" => WeightSpec::Abs(400),
        "bold" => WeightSpec::Abs(700),
        "bolder" => WeightSpec::Bolder,
        "lighter" => WeightSpec::Lighter,
        _ => return err(p),
    })
}

pub(crate) fn parse_font_size<'i>(p: &mut Parser<'i>) -> PResult<'i, FontSizeSpec> {
    if let Ok(k) = p.try_parse(ident) {
        let px = match k.as_str() {
            "xx-small" => 9.0,
            "x-small" => 10.0,
            "small" => 13.0,
            "medium" => 16.0,
            "large" => 18.0,
            "x-large" => 24.0,
            "xx-large" => 32.0,
            "xxx-large" => 48.0,
            "smaller" => return Ok(FontSizeSpec::Smaller),
            "larger" => return Ok(FontSizeSpec::Larger),
            _ => return err(p),
        };
        return Ok(FontSizeSpec::Len(LenSpec::px(px)));
    }
    Ok(FontSizeSpec::Len(parse_len(p)?))
}

pub(crate) fn parse_line_height<'i>(p: &mut Parser<'i>) -> PResult<'i, LineHeightSpec> {
    if p.try_parse(|p| p.expect_ident_matching("normal")).is_ok() {
        return Ok(LineHeightSpec::Normal);
    }
    if let Ok(n) = p.try_parse(|p| p.expect_number()) {
        return Ok(LineHeightSpec::Number(n));
    }
    // `calc(1em + 4px)` or a plain length/percentage
    match parse_len(p)? {
        LenSpec::Lin(l) if l.pct != 0.0 && Lin { pct: 0.0, ..l }.is_zero() => {
            Ok(LineHeightSpec::Number(l.pct / 100.0))
        }
        other => Ok(LineHeightSpec::Len(other)),
    }
}

pub(crate) fn classify_family(name: &str) -> Option<FontFamily> {
    let n = name.to_ascii_lowercase();
    let n = n.trim();
    const MONO: &[&str] = &["mono", "courier", "consolas", "menlo", "monaco", "source code", "fira code", "jetbrains", "cascadia", "inconsolata", "sfmono"];
    const SERIF: &[&str] = &["georgia", "times", "palatino", "garamond", "cambria", "charter", "baskerville", "merriweather", "iowan", "bookman", "didot", "hoefler", "minion", "libertine", "lora", "playfair"];
    const SANS: &[&str] = &["system-ui", "-apple-system", "blinkmacsystemfont", "helvetica", "arial", "inter", "roboto", "segoe", "verdana", "tahoma", "lato", "ubuntu", "cantarell", "geneva", "lucida grande", "avenir", "futura", "gill sans", "trebuchet", "montserrat", "poppins", "nunito"];
    if n == "monospace" || MONO.iter().any(|m| n.contains(m)) {
        Some(FontFamily::Mono)
    } else if n.contains("sans") || SANS.iter().any(|s| n.contains(s)) {
        Some(FontFamily::Sans)
    } else if n.contains("serif") || SERIF.iter().any(|s| n.contains(s)) {
        Some(FontFamily::Serif)
    } else {
        None
    }
}

pub(crate) fn parse_font_family<'i>(p: &mut Parser<'i>) -> PResult<'i, (FontFamily, FontNames)> {
    let families = p.parse_comma_separated(|p| {
        if let Ok(s) = p.try_parse(|p| p.expect_string().map(|s| s.to_string())) {
            return Ok(s);
        }
        let mut name = String::new();
        while let Ok(id) = p.try_parse(|p| p.expect_ident().map(|i| i.to_string())) {
            if !name.is_empty() {
                name.push(' ');
            }
            name.push_str(&id);
        }
        if name.is_empty() { err(p) } else { Ok(name) }
    })?;
    let generic = families.iter().find_map(|f| classify_family(f)).unwrap_or(FontFamily::Sans);
    let names: Vec<Arc<str>> = families.iter().map(|f| Arc::from(f.as_str())).collect();
    Ok((generic, names.into()))
}

pub(crate) fn parse_font<'i>(p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    // System fonts: `font: menu` etc.
    if let Ok(()) = p.try_parse(|p| {
        keyword(p, &[("caption", ()), ("icon", ()), ("menu", ()), ("message-box", ()), ("small-caption", ()), ("status-bar", ())])
    }) {
        out.extend([
            Decl::FontSize(FontSizeSpec::Len(LenSpec::px(13.0))),
            Decl::FontFamily(FontFamily::Sans),
            Decl::FontNames(FontNames::from([])),
            Decl::FontWeight(WeightSpec::Abs(400)),
            Decl::Italic(false),
            Decl::LineHeight(LineHeightSpec::Normal),
        ]);
        return Ok(());
    }
    let mut italic = false;
    let mut weight = WeightSpec::Abs(400);
    for _ in 0..4 {
        if let Ok(k) = p.try_parse(ident) {
            match k.as_str() {
                "normal" | "small-caps" | "condensed" | "expanded" | "semi-condensed" | "semi-expanded" => continue,
                "italic" | "oblique" => {
                    italic = true;
                    continue;
                }
                "bold" => weight = WeightSpec::Abs(700),
                "bolder" => weight = WeightSpec::Bolder,
                "lighter" => weight = WeightSpec::Lighter,
                _ => return err(p),
            }
            continue;
        }
        if let Ok(n) = p.try_parse(|p| p.expect_number()) {
            weight = WeightSpec::Abs(n.clamp(1.0, 1000.0) as u16);
            continue;
        }
        break;
    }
    let size = parse_font_size(p)?;
    let line_height = if p.try_parse(|p| p.expect_delim('/')).is_ok() {
        parse_line_height(p)?
    } else {
        LineHeightSpec::Normal
    };
    let (family, names) = parse_font_family(p)?;
    out.extend([
        Decl::FontSize(size),
        Decl::LineHeight(line_height),
        Decl::FontWeight(weight),
        Decl::Italic(italic),
        Decl::FontFamily(family),
        Decl::FontNames(names),
    ]);
    Ok(())
}

// ---------------------------------------------------------------------------
// Lengths & calc()
