//! CSS text for the CSSOM: a stylesheet's top-level rules and a declaration
//! block's declarations, tokenized as CSS (strings, `url()`, comments and
//! nested blocks are whole tokens, so a `;` or `}` inside one doesn't end
//! anything).

use cssparser::{Delimiter, ParseError, Parser, Token};

/// The text of each top-level rule (`a { … }`, `@media … { … }`,
/// `@import …;`), without the comments and whitespace between them.
pub fn rules(css: &str) -> Vec<String> {
    let mut p = Parser::new(css);
    let mut out = Vec::new();
    loop {
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        let start = p.position();
        let mut end = None;
        while let Ok(t) = p.next_including_whitespace_and_comments() {
            match t {
                Token::CurlyBracketBlock => {
                    let _ = p.parse_nested_block(|p| {
                        while p.next_including_whitespace_and_comments().is_ok() {}
                        Ok::<(), ParseError<()>>(())
                    });
                    end = Some(true);
                    break;
                }
                Token::Semicolon => {
                    end = Some(false);
                    break;
                }
                _ => {}
            }
        }
        let text = p.slice_from(start).trim();
        match end {
            Some(true) => out.push(text.to_owned()),
            // A `;` only ends an at-rule statement (`@import …;`); before
            // a block, it's an invalid rule that's dropped.
            Some(false) if text.starts_with('@') => out.push(text.to_owned()),
            None if text.starts_with('@') && !text.is_empty() => out.push(format!("{text};")),
            _ => {}
        }
    }
    out
}

/// A declaration block's declarations: (name, value, `!important`), in
/// order. Names are lowercased (custom properties keep their case); values
/// are the text as written, trimmed.
pub fn declarations(block: &str) -> Vec<(String, String, bool)> {
    let mut p = Parser::new(block);
    let mut out = Vec::new();
    while !p.is_exhausted() {
        let _ = p.parse_until_after(Delimiter::Semicolon, |p| {
            p.skip_whitespace();
            let name = match p.next()? {
                Token::Ident(name) => name.to_string(),
                _ => return Err(p.new_error_for_next_token::<()>()),
            };
            p.skip_whitespace();
            if !matches!(p.next()?, Token::Colon) {
                return Err(p.new_error_for_next_token());
            }
            p.skip_whitespace();
            let start = p.position();
            // The value's end, before a trailing `!important`.
            let mut value_end = None;
            loop {
                let before = p.state();
                match p.next() {
                    Ok(Token::Delim('!')) => {
                        let at = before.position();
                        p.skip_whitespace();
                        match p.next() {
                            Ok(Token::Ident(i)) if i.eq_ignore_ascii_case("important") => {
                                p.skip_whitespace();
                                if p.is_exhausted() {
                                    value_end = Some(at);
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            let important = value_end.is_some();
            let value = match value_end {
                Some(end) => {
                    let all = p.slice_from(start);
                    let len = p.slice(start..end).len();
                    all[..len].trim().to_owned()
                }
                None => p.slice_from(start).trim().to_owned(),
            };
            let name = if name.starts_with("--") { name } else { name.to_ascii_lowercase() };
            out.push((name, value, important));
            Ok(())
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_split_at_the_top_level() {
        assert_eq!(
            rules("/* lead */ :host { a: b } /* x */ nav { c: d } @media (min-width: 1px) { .b { color: blue } } @import url(x.css);"),
            vec![":host { a: b }", "nav { c: d }", "@media (min-width: 1px) { .b { color: blue } }", "@import url(x.css);"]
        );
        // Braces and semicolons in strings don't end a rule.
        assert_eq!(rules(r#"a::before { content: "}"; } b { c: d }"#), vec![r#"a::before { content: "}"; }"#, "b { c: d }"]);
        // A stray `;` drops the broken rule only.
        assert_eq!(rules("junk; a { b: c }"), vec!["a { b: c }"]);
    }

    #[test]
    fn declarations_keep_their_values_whole() {
        assert_eq!(
            declarations(r#"color: red; background: url("data:image/svg+xml;utf8,<svg/>") no-repeat ;--Var: a;b ; Width : 10px !important"#),
            vec![
                ("color".into(), "red".into(), false),
                ("background".into(), r#"url("data:image/svg+xml;utf8,<svg/>") no-repeat"#.into(), false),
                ("--Var".into(), "a".into(), false),
                ("width".into(), "10px".into(), true),
            ]
        );
        assert_eq!(declarations(r#"content: ";"; top: 0"#), vec![("content".into(), r#"";""#.into(), false), ("top".into(), "0".into(), false)]);
        assert_eq!(declarations("garbage; left: 1px"), vec![("left".into(), "1px".into(), false)]);
    }
}
