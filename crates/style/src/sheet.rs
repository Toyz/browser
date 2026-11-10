//! Stylesheet parsing with `cssparser`: style rules (keeping their `@media`
//! conditions), `@import`, and group rules.

use std::sync::Arc;

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser,
};
use selectors::SelectorList;
use selectors::parser::{ParseRelative, SelectorParseErrorKind};

use crate::media;
use crate::props::{Decl, parse_declaration};
use crate::selector::{SelectorParser, Simple};

#[derive(Clone)]
pub struct StyleRule {
    pub selectors: SelectorList<Simple>,
    pub normal: Arc<[Decl]>,
    pub important: Arc<[Decl]>,
    /// Media query lists that must all match for the rule to apply (from
    /// enclosing `@media` blocks, `<link media>`, `@import ... media`).
    /// Kept rather than evaluated at parse time, so resizing can re-check them.
    pub media: Arc<[Arc<str>]>,
    /// The cascade layer it's in (`a.b` for nested ones), if any.
    pub layer: Option<Arc<str>>,
    /// Where its layer sits in the cascade (set when a document's sheets
    /// are put together; higher wins for normal declarations).
    pub layer_rank: u16,
}

/// An `@font-face` rule.
#[derive(Debug, Clone, PartialEq)]
pub struct FontFace {
    /// The family name pages use it by.
    pub family: Arc<str>,
    /// Font files to try, in order: (absolute URL, `format()` hint).
    pub sources: Vec<(Arc<str>, Option<String>)>,
    pub weight: (u16, u16),
    pub italic: bool,
}

/// `@keyframes` frames: (offset 0..=1, declarations), in source order.
pub type Keyframes = Vec<(f32, Arc<[Decl]>)>;

#[derive(Default)]
pub struct Stylesheet {
    pub rules: Vec<StyleRule>,
    /// `@keyframes name { ... }`.
    pub keyframes: Vec<(Arc<str>, Keyframes)>,
    /// `@font-face` rules (only those whose `unicode-range` covers Latin).
    pub font_faces: Vec<FontFace>,
    /// `@import`s: (url as written, media query list). Resolved by the caller.
    pub imports: Vec<(String, String)>,
    /// Cascade layer names in the order this sheet first mentions them
    /// (`@layer a, b;` statements and `@layer a { }` blocks).
    pub layers: Vec<Arc<str>>,
}

/// Parse a stylesheet. Rules inside `@media` keep their conditions (see
/// [`StyleRule::media`]); nothing is evaluated against the viewport here.
pub fn parse_stylesheet(css: &str) -> Stylesheet {
    let mut sheet = Stylesheet::default();
    let mut parser = Parser::new(css);
    let mut top = TopLevel { sheet: &mut sheet, seen_rule: false, media: Vec::new(), layer: None };
    parse_rule_list(&mut parser, &mut top);
    sheet
}

/// Parse a `style=""` attribute into (normal, important) declarations.
pub fn parse_style_attribute(css: &str) -> (Vec<Decl>, Vec<Decl>) {
    let mut parser = Parser::new(css);
    let mut decls = Declarations::default();
    for _ in RuleBodyParser::new(&mut parser, &mut decls) {}
    (decls.normal, decls.important)
}

fn parse_rule_list<'i>(input: &mut Parser<'i>, top: &mut TopLevel<'_>) {
    // Errors are per-rule and already recovered from; nothing to report.
    for _ in StyleSheetParser::new(input, top) {}
}

struct TopLevel<'a> {
    sheet: &'a mut Stylesheet,
    seen_rule: bool,
    /// Conditions of the `@media` blocks we're inside.
    media: Vec<Arc<str>>,
    /// The `@layer` block we're inside (full dotted name).
    layer: Option<Arc<str>>,
}

impl TopLevel<'_> {
    /// `name` declared inside the current layer: its full name, recorded in
    /// the sheet's layer order (parents first).
    fn declare_layer(&mut self, name: &str) -> Arc<str> {
        let mut full = self.layer.as_deref().map(str::to_owned).unwrap_or_default();
        for segment in name.split('.') {
            if !full.is_empty() {
                full.push('.');
            }
            full.push_str(segment.trim());
            if !self.sheet.layers.iter().any(|l| **l == *full) {
                self.sheet.layers.push(full.as_str().into());
            }
        }
        full.into()
    }
}

/// A name for an anonymous `@layer { }`: unique, and not one a page can write.
fn anonymous_layer() -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    format!("\u{0}anonymous{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

enum AtPrelude {
    /// A group rule whose contents apply (true) or not (false).
    Group(bool),
    Media(Arc<str>),
    Import(String, String),
    Keyframes(Arc<str>),
    /// `@layer a, b;` (names, in order) or `@layer a { }` (one name; an
    /// anonymous block gets a generated one).
    Layer(Vec<String>),
    FontFace,
    Skip,
}

fn consume_rest<'i>(input: &mut Parser<'i>) -> String {
    let start = input.position();
    while input.next().is_ok() {}
    input.slice_from(start).to_owned()
}

impl<'i> AtRuleParser<'i> for TopLevel<'_> {
    type Prelude = AtPrelude;
    type AtRule = ();
    type Error = SelectorParseErrorKind;

    fn parse_prelude(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<AtPrelude, ParseError<Self::Error>> {
        let name = name.to_ascii_lowercase();
        Ok(match name.as_str() {
            "media" => {
                let query = consume_rest(input);
                // Queries that can never match a screen (print-only) are dropped now.
                if media::can_ever_match(&query) {
                    AtPrelude::Media(query.trim().into())
                } else {
                    AtPrelude::Group(false)
                }
            }
            "supports" => {
                // Optimistic: we claim support for anything, except explicit negations.
                let condition = consume_rest(input);
                AtPrelude::Group(!condition.trim_start().to_ascii_lowercase().starts_with("not"))
            }
            "container" => AtPrelude::Media(media::container_condition(&consume_rest(input)).into()),
            "layer" => {
                let names = consume_rest(input);
                let names: Vec<String> = names.split(',').map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()).collect();
                AtPrelude::Layer(names)
            }
            "scope" | "document" | "-moz-document" => {
                consume_rest(input);
                AtPrelude::Group(true)
            }
            "font-face" => AtPrelude::FontFace,
            "keyframes" | "-webkit-keyframes" | "-moz-keyframes" => {
                let name = match input.next()?.clone() {
                    cssparser::Token::Ident(s) | cssparser::Token::QuotedString(s) => Arc::from(&*s),
                    _ => return Err(input.new_error_for_next_token()),
                };
                AtPrelude::Keyframes(name)
            }
            "import" if !self.seen_rule => {
                let url = input.expect_url_or_string()?.to_string();
                let rest = consume_rest(input);
                // Drop `layer(...)` / `supports(...)` from the media list.
                let media = rest
                    .split([')', '('])
                    .collect::<Vec<_>>();
                let media = if rest.contains("layer") || rest.contains("supports") {
                    media.last().map(|s| s.trim().to_owned()).unwrap_or_default()
                } else {
                    rest.trim().to_owned()
                };
                AtPrelude::Import(url, media)
            }
            _ => {
                consume_rest(input);
                AtPrelude::Skip
            }
        })
    }

    fn rule_without_block(&mut self, prelude: AtPrelude, _start: &ParserState) -> Result<(), ()> {
        match prelude {
            AtPrelude::Import(url, media) => self.sheet.imports.push((url, media)),
            // `@layer a, b;`: just the order.
            AtPrelude::Layer(names) => {
                for name in names {
                    self.declare_layer(&name);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn parse_block(
        &mut self,
        prelude: AtPrelude,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<Self::Error>> {
        match prelude {
            AtPrelude::Group(true) => parse_rule_list(input, self),
            AtPrelude::Layer(names) => {
                let name = names.into_iter().next().unwrap_or_else(anonymous_layer);
                let full = self.declare_layer(&name);
                let outer = self.layer.replace(full);
                parse_rule_list(input, self);
                self.layer = outer;
            }
            AtPrelude::Media(query) => {
                self.media.push(query);
                parse_rule_list(input, self);
                self.media.pop();
            }
            AtPrelude::FontFace => {
                let mut face = FontFaceParser::default();
                for _ in RuleBodyParser::new(input, &mut face) {}
                if let (Some(family), false, true) = (face.family, face.sources.is_empty(), face.latin) {
                    self.sheet.font_faces.push(FontFace {
                        family,
                        sources: face.sources,
                        weight: face.weight.unwrap_or((400, 400)),
                        italic: face.italic,
                    });
                }
            }
            AtPrelude::Keyframes(name) => {
                let mut frames = KeyframesParser::default();
                for _ in StyleSheetParser::new(input, &mut frames) {}
                frames.frames.sort_by(|a, b| a.0.total_cmp(&b.0));
                self.sheet.keyframes.push((name, frames.frames));
            }
            _ => {}
        }
        Ok(())
    }
}

impl<'i> QualifiedRuleParser<'i> for TopLevel<'_> {
    /// The selectors, and their text (for rules nested inside).
    type Prelude = (SelectorList<Simple>, String);
    type QualifiedRule = ();
    type Error = SelectorParseErrorKind;

    fn parse_prelude(
        &mut self,
        input: &mut Parser<'i>,
    ) -> Result<(SelectorList<Simple>, String), ParseError<Self::Error>> {
        self.seen_rule = true;
        let start = input.position();
        let selectors = SelectorList::parse(&SelectorParser, input, ParseRelative::No)?;
        Ok((selectors, input.slice_from(start).trim().to_owned()))
    }

    fn parse_block(
        &mut self,
        (selectors, text): (SelectorList<Simple>, String),
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<Self::Error>> {
        let mut body = NestedBody { parent: text, media: self.media.clone(), layer: self.layer.clone(), decls: Declarations::default(), nested: Vec::new() };
        for _ in RuleBodyParser::new(input, &mut body) {}
        let decls = body.decls;
        if !decls.normal.is_empty() || !decls.important.is_empty() {
            self.sheet.rules.push(StyleRule {
                selectors,
                normal: decls.normal.into(),
                important: decls.important.into(),
                media: self.media.clone().into(),
                layer: self.layer.clone(),
                layer_rank: 0,
            });
        }
        // Nested rules (CSS nesting) follow, as plain rules.
        self.sheet.rules.append(&mut body.nested);
        Ok(())
    }
}

/// `parent` composed into a nested rule's selector: `&` stands for it, and
/// a selector without one is a descendant of it (`> a` a child).
fn nest_selector(parent: &str, nested: &str) -> String {
    let parent = parent.trim();
    // A list goes in `:is()`; one selector can be spliced in as it is
    // (`:is()` can't hold pseudo-elements).
    let one = split_top_level_commas(parent).len() == 1;
    let whole = if one { parent.to_owned() } else { format!(":is({parent})") };
    split_top_level_commas(nested)
        .into_iter()
        .map(|part| {
            let part = part.trim();
            if part.contains('&') { part.replace('&', &whole) } else { format!("{whole} {part}") }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn split_top_level_commas(s: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// A style rule's block with nesting: its declarations, and the rules
/// nested in it (flattened, with composed selectors).
struct NestedBody {
    parent: String,
    media: Vec<Arc<str>>,
    layer: Option<Arc<str>>,
    decls: Declarations,
    nested: Vec<StyleRule>,
}

impl NestedBody {
    /// A nested block (`& .b { ... }`, or `@media` inside a rule).
    fn child(&mut self, selector: String, media: Vec<Arc<str>>, input: &mut Parser<'_>) {
        let mut body = NestedBody { parent: selector.clone(), media: media.clone(), layer: self.layer.clone(), decls: Declarations::default(), nested: Vec::new() };
        for _ in RuleBodyParser::new(input, &mut body) {}
        let decls = body.decls;
        if !decls.normal.is_empty() || !decls.important.is_empty() {
            let mut parser = Parser::new(&selector);
            if let Ok(selectors) = SelectorList::parse(&SelectorParser, &mut parser, ParseRelative::No) {
                self.nested.push(StyleRule {
                    selectors,
                    normal: decls.normal.into(),
                    important: decls.important.into(),
                    media: media.into(),
                    layer: self.layer.clone(),
                    layer_rank: 0,
                });
            }
        }
        self.nested.append(&mut body.nested);
    }
}

impl<'i> DeclarationParser<'i> for NestedBody {
    type Declaration = ();
    type Error = SelectorParseErrorKind;

    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        start: &ParserState,
    ) -> Result<(), ParseError<Self::Error>> {
        self.decls.parse_value(name, input, start)
    }
}

impl<'i> AtRuleParser<'i> for NestedBody {
    /// `@media` (its query) or another group rule that applies (`None`).
    type Prelude = Option<Option<Arc<str>>>;
    type AtRule = ();
    type Error = SelectorParseErrorKind;

    fn parse_prelude(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<Self::Prelude, ParseError<Self::Error>> {
        let query = consume_rest(input);
        Ok(match name.to_ascii_lowercase().as_str() {
            "media" => Some(Some(query.trim().into())),
            "container" => Some(Some(media::container_condition(&query).into())),
            "supports" | "layer" | "scope" => Some(None),
            _ => None,
        })
    }

    fn parse_block(
        &mut self,
        prelude: Self::Prelude,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<Self::Error>> {
        let Some(query) = prelude else { return Ok(()) };
        let mut media = self.media.clone();
        media.extend(query);
        // Its declarations belong to the enclosing rule's selector.
        let selector = self.parent.clone();
        self.child(selector, media, input);
        Ok(())
    }
}

impl<'i> QualifiedRuleParser<'i> for NestedBody {
    type Prelude = String;
    type QualifiedRule = ();
    type Error = SelectorParseErrorKind;

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<String, ParseError<Self::Error>> {
        Ok(nest_selector(&self.parent, &consume_rest(input)))
    }

    fn parse_block(
        &mut self,
        selector: String,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<Self::Error>> {
        let media = self.media.clone();
        self.child(selector, media, input);
        Ok(())
    }
}

impl<'i> RuleBodyItemParser<'i, (), SelectorParseErrorKind> for NestedBody {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        true
    }
}

/// The descriptors of an `@font-face` block.
struct FontFaceParser {
    family: Option<Arc<str>>,
    sources: Vec<(Arc<str>, Option<String>)>,
    weight: Option<(u16, u16)>,
    italic: bool,
    /// Its `unicode-range` covers Latin text (or it has none).
    latin: bool,
}

impl Default for FontFaceParser {
    fn default() -> Self {
        Self { family: None, sources: Vec::new(), weight: None, italic: false, latin: true }
    }
}

impl<'i> DeclarationParser<'i> for FontFaceParser {
    type Declaration = ();
    type Error = SelectorParseErrorKind;

    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _start: &ParserState,
    ) -> Result<(), ParseError<Self::Error>> {
        use cssparser::Token;
        match &*name.to_ascii_lowercase() {
            "font-family" => {
                let mut words = Vec::new();
                while let Ok(t) = input.next() {
                    match t {
                        Token::QuotedString(s) | Token::Ident(s) => words.push(s.to_string()),
                        _ => {}
                    }
                }
                if !words.is_empty() {
                    self.family = Some(words.join(" ").into());
                }
            }
            "src" => {
                let _ = input.parse_comma_separated(|p| {
                    let url = p.try_parse(crate::props::parse_url).ok().flatten();
                    let mut format = None;
                    while let Ok(t) = p.next().cloned() {
                        if let Token::Function(f) = t
                            && f.eq_ignore_ascii_case("format")
                        {
                            format = p
                                .parse_nested_block(|p| {
                                    Ok::<_, ParseError<SelectorParseErrorKind>>(match p.next()?.clone() {
                                        Token::QuotedString(s) | Token::Ident(s) => Some(s.to_ascii_lowercase()),
                                        _ => None,
                                    })
                                })
                                .unwrap_or(None);
                        }
                    }
                    if let Some(url) = url {
                        self.sources.push((url, format));
                    }
                    Ok::<_, ParseError<SelectorParseErrorKind>>(())
                });
            }
            "font-weight" => {
                let mut values = Vec::new();
                while let Ok(t) = input.next() {
                    match t {
                        Token::Number { value, .. } => values.push(value.clamp(1.0, 1000.0) as u16),
                        Token::Ident(id) if id.eq_ignore_ascii_case("bold") => values.push(700),
                        Token::Ident(id) if id.eq_ignore_ascii_case("normal") => values.push(400),
                        _ => {}
                    }
                }
                if let Some(&first) = values.first() {
                    self.weight = Some((first, *values.last().unwrap_or(&first)));
                }
            }
            "font-style" => {
                if let Ok(id) = input.expect_ident_cloned() {
                    self.italic = matches!(&*id.to_ascii_lowercase(), "italic" | "oblique");
                }
                while input.next().is_ok() {}
            }
            "unicode-range" => {
                // Keep the subset that has the basic Latin letters.
                let raw = consume_rest(input).to_ascii_lowercase();
                self.latin = raw.split(',').any(|range| {
                    let range = range.trim().trim_start_matches("u+");
                    let (lo, hi) = match range.split_once('-') {
                        Some((a, b)) => (a.replace('?', "0"), b.to_owned()),
                        None => (range.replace('?', "0"), range.replace('?', "f")),
                    };
                    let (lo, hi) = (u32::from_str_radix(&lo, 16).unwrap_or(u32::MAX), u32::from_str_radix(&hi, 16).unwrap_or(0));
                    lo <= 0x41 && 0x41 <= hi
                });
            }
            _ => {
                while input.next().is_ok() {}
            }
        }
        Ok(())
    }
}

impl<'i> AtRuleParser<'i> for FontFaceParser {
    type Prelude = ();
    type AtRule = ();
    type Error = SelectorParseErrorKind;
}

impl<'i> QualifiedRuleParser<'i> for FontFaceParser {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = SelectorParseErrorKind;
}

impl<'i> RuleBodyItemParser<'i, (), SelectorParseErrorKind> for FontFaceParser {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

/// The blocks inside `@keyframes`: `from`, `to`, `50%`, `0%, 100%`.
#[derive(Default)]
struct KeyframesParser {
    frames: Keyframes,
}

impl<'i> QualifiedRuleParser<'i> for KeyframesParser {
    type Prelude = Vec<f32>;
    type QualifiedRule = ();
    type Error = SelectorParseErrorKind;

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<Vec<f32>, ParseError<Self::Error>> {
        input.parse_comma_separated(|p| {
            match p.next()?.clone() {
                cssparser::Token::Ident(id) if id.eq_ignore_ascii_case("from") => Ok(0.0),
                cssparser::Token::Ident(id) if id.eq_ignore_ascii_case("to") => Ok(1.0),
                cssparser::Token::Percentage { unit_value, .. } => Ok(unit_value.clamp(0.0, 1.0)),
                _ => Err(p.new_error_for_next_token()),
            }
        })
    }

    fn parse_block(&mut self, offsets: Vec<f32>, _start: &ParserState, input: &mut Parser<'i>) -> Result<(), ParseError<Self::Error>> {
        let mut decls = Declarations::default();
        for _ in RuleBodyParser::new(input, &mut decls) {}
        let decls: Arc<[Decl]> = decls.normal.into();
        for offset in offsets {
            self.frames.push((offset, decls.clone()));
        }
        Ok(())
    }
}

impl<'i> AtRuleParser<'i> for KeyframesParser {
    type Prelude = ();
    type AtRule = ();
    type Error = SelectorParseErrorKind;
}

#[derive(Default)]
struct Declarations {
    normal: Vec<Decl>,
    important: Vec<Decl>,
}

/// Split a trailing `!important` off a raw declaration value.
fn split_important(raw: &str) -> (&str, bool) {
    let trimmed = raw.trim_end();
    // `get`: the cut may fall inside a multi-byte character (`content: "英"`).
    let lower_tail = trimmed.len().checked_sub("important".len()).and_then(|i| trimmed.get(i..));
    if let Some(tail) = lower_tail
        && tail.eq_ignore_ascii_case("important")
    {
        let before = trimmed.get(..trimmed.len() - "important".len()).unwrap_or("").trim_end();
        if let Some(value) = before.strip_suffix('!') {
            return (value, true);
        }
    }
    (raw, false)
}

impl<'i> DeclarationParser<'i> for Declarations {
    type Declaration = ();
    type Error = SelectorParseErrorKind;

    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _declaration_start: &ParserState,
    ) -> Result<(), ParseError<Self::Error>> {
        let raw = consume_rest(input);
        let (value, important) = split_important(&raw);
        let out = if important { &mut self.important } else { &mut self.normal };
        parse_declaration(&name, value, out);
        Ok(())
    }
}

impl<'i> AtRuleParser<'i> for Declarations {
    type Prelude = ();
    type AtRule = ();
    type Error = SelectorParseErrorKind;
}

impl<'i> QualifiedRuleParser<'i> for Declarations {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = SelectorParseErrorKind;
}

impl<'i> RuleBodyItemParser<'i, (), SelectorParseErrorKind> for Declarations {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        // CSS nesting isn't supported yet; nested rules are skipped.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::*;

    #[test]
    fn multibyte_values_dont_panic() {
        assert_eq!(split_important("\"英\""), ("\"英\"", false));
        assert_eq!(split_important("red !important"), ("red ", true));
        let sheet = parse_stylesheet(r#"a::after{content:"英"}@font-face{font-family:X;src:url(https://e.com/x.woff2)}"#);
        assert_eq!(sheet.font_faces.len(), 1);
    }

    #[test]
    fn font_faces_by_unicode_range() {
        let base = url::Url::parse("https://developer.mozilla.org/en-US/docs/x").unwrap();
        let sheet = crate::props::with_base(Some(&base), || parse_stylesheet(
            r#"@font-face{font-display:swap;font-family:Inter;font-feature-settings:"calt" 0;font-style:normal;font-weight:100 900;src:url(/a.woff2) format("woff2");unicode-range:u+00??,u+0131,u+2000-206f}
               @font-face{font-family:Inter;src:url(/cyr.woff2) format("woff2");unicode-range:u+0301,u+0400-045f}"#,
        ));
        let urls: Vec<&str> = sheet.font_faces.iter().flat_map(|f| f.sources.iter().map(|(u, _)| &**u)).collect();
        assert_eq!(urls, vec!["https://developer.mozilla.org/a.woff2"]);
        assert_eq!(sheet.font_faces[0].weight, (100, 900));
    }

    #[test]
    fn parses_rules_media_and_important() {
        let css = r#"
            @import url("base.css") screen;
            @charset "utf-8";
            p, .x > a { color: red; margin: 0 !important }
            @media print { p { color: blue } }
            @media (min-width: 100px) { h1 { display: none } }
            @font-face { font-family: X; src: url(x.woff) }
            @keyframes spin { from { opacity: 0 } }
            .broken { color: ; width: 10px }
            :hover > b, :nope { color: green }
        "#;
        let sheet = parse_stylesheet(css);
        assert_eq!(sheet.imports, vec![("base.css".to_owned(), "screen".to_owned())]);
        // p/.x rule, h1 (media matched), .broken (width survives); `:nope` invalidates its rule.
        assert_eq!(sheet.rules.len(), 3, "{:?}", sheet.rules.iter().map(|r| r.normal.clone()).collect::<Vec<_>>());
        assert_eq!(&*sheet.rules[0].normal, &[Decl::Color(Color::Rgba(0xff0000ff))]);
        assert_eq!(sheet.rules[0].important.len(), 4);
        assert!(sheet.rules[0].media.is_empty());
        assert_eq!(&*sheet.rules[1].normal, &[Decl::Display(Display::None)]);
        assert_eq!(&*sheet.rules[1].media, &["(min-width: 100px)".into()], "condition kept for resizing");
        assert_eq!(&*sheet.rules[2].normal, &[Decl::Width(LenSpec::px(10.0))]);
    }

    #[test]
    fn style_attribute() {
        let (normal, important) = parse_style_attribute("color: #fff; display:none ! IMPORTANT");
        assert_eq!(normal, vec![Decl::Color(Color::Rgba(0xffffffff))]);
        assert_eq!(important, vec![Decl::Display(Display::None)]);
    }
}

