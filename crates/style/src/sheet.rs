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
}

/// `@keyframes` frames: (offset 0..=1, declarations), in source order.
pub type Keyframes = Vec<(f32, Arc<[Decl]>)>;

#[derive(Default)]
pub struct Stylesheet {
    pub rules: Vec<StyleRule>,
    /// `@keyframes name { ... }`.
    pub keyframes: Vec<(Arc<str>, Keyframes)>,
    /// `@import`s: (url as written, media query list). Resolved by the caller.
    pub imports: Vec<(String, String)>,
}

/// Parse a stylesheet. Rules inside `@media` keep their conditions (see
/// [`StyleRule::media`]); nothing is evaluated against the viewport here.
pub fn parse_stylesheet(css: &str) -> Stylesheet {
    let mut sheet = Stylesheet::default();
    let mut parser = Parser::new(css);
    let mut top = TopLevel { sheet: &mut sheet, seen_rule: false, media: Vec::new() };
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
}

enum AtPrelude {
    /// A group rule whose contents apply (true) or not (false).
    Group(bool),
    Media(Arc<str>),
    Import(String, String),
    Keyframes(Arc<str>),
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
            "layer" | "container" | "scope" | "document" | "-moz-document" => {
                consume_rest(input);
                AtPrelude::Group(true)
            }
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
        if let AtPrelude::Import(url, media) = prelude {
            self.sheet.imports.push((url, media));
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
            AtPrelude::Media(query) => {
                self.media.push(query);
                parse_rule_list(input, self);
                self.media.pop();
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
    type Prelude = SelectorList<Simple>;
    type QualifiedRule = ();
    type Error = SelectorParseErrorKind;

    fn parse_prelude(
        &mut self,
        input: &mut Parser<'i>,
    ) -> Result<SelectorList<Simple>, ParseError<Self::Error>> {
        self.seen_rule = true;
        SelectorList::parse(&SelectorParser, input, ParseRelative::No)
    }

    fn parse_block(
        &mut self,
        selectors: SelectorList<Simple>,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<Self::Error>> {
        let mut decls = Declarations::default();
        for _ in RuleBodyParser::new(input, &mut decls) {}
        if !decls.normal.is_empty() || !decls.important.is_empty() {
            self.sheet.rules.push(StyleRule {
                selectors,
                normal: decls.normal.into(),
                important: decls.important.into(),
                media: self.media.clone().into(),
            });
        }
        Ok(())
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
    let lower_tail = trimmed.len().checked_sub("important".len()).map(|i| &trimmed[i..]);
    if let Some(tail) = lower_tail
        && tail.eq_ignore_ascii_case("important")
    {
        let before = trimmed[..trimmed.len() - "important".len()].trim_end();
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
