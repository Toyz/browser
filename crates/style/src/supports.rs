//! `@supports` conditions (and `CSS.supports()`): whether this engine
//! supports a declaration or a selector, combined with `not`/`and`/`or`.

use cssparser::{ParseError, Parser, Token};
use selectors::SelectorList;
use selectors::parser::ParseRelative;

use crate::selector::SelectorParser;

/// Does the `@supports` condition hold? Unknown forms don't.
pub fn supports(condition: &str) -> bool {
    let mut p = Parser::new(condition);
    let ok = condition_of(&mut p);
    p.skip_whitespace();
    ok && p.is_exhausted()
}

/// `CSS.supports(property, value)`.
pub fn supports_declaration(name: &str, value: &str) -> bool {
    crate::props::declaration_valid(name.trim(), value)
}

fn condition_of(p: &mut Parser) -> bool {
    p.skip_whitespace();
    if p.try_parse(|p| p.expect_ident_matching("not")).is_ok() {
        return !in_parens(p);
    }
    let mut value = in_parens(p);
    loop {
        p.skip_whitespace();
        let state = p.state();
        match p.next() {
            Ok(Token::Ident(word)) if word.eq_ignore_ascii_case("and") => value &= in_parens(p),
            Ok(Token::Ident(word)) if word.eq_ignore_ascii_case("or") => value |= in_parens(p),
            _ => {
                p.reset(&state);
                return value;
            }
        }
    }
}

fn in_parens(p: &mut Parser) -> bool {
    p.skip_whitespace();
    let token = match p.next() {
        Ok(t) => t.clone(),
        Err(_) => return false,
    };
    let inner = |p: &mut Parser, f: &dyn Fn(&mut Parser) -> bool| {
        p.parse_nested_block(|p| {
            let ok = f(p);
            // Whatever is left (an unknown form) is consumed, not an error.
            while p.next().is_ok() {}
            Ok::<bool, ParseError<()>>(ok)
        })
        .unwrap_or(false)
    };
    match token {
        Token::ParenthesisBlock => inner(p, &|p| {
            p.skip_whitespace();
            let start = p.state();
            // A nested condition, else a declaration.
            let nested = matches!(p.next(), Ok(Token::ParenthesisBlock))
                || { p.reset(&start); p.try_parse(|p| p.expect_ident_matching("not")).is_ok() };
            p.reset(&start);
            if nested {
                return condition_of(p) && { p.skip_whitespace(); p.is_exhausted() };
            }
            declaration(p)
        }),
        Token::Function(name) if name.eq_ignore_ascii_case("selector") => inner(p, &|p| {
            SelectorList::parse(&SelectorParser, p, ParseRelative::No).is_ok() && p.is_exhausted()
        }),
        Token::Function(name) if name.eq_ignore_ascii_case("font-format") => inner(p, &|p| {
            p.expect_ident().is_ok_and(|f| matches!(&*f.to_ascii_lowercase(), "woff" | "woff2" | "truetype" | "opentype"))
        }),
        _ => false,
    }
}

/// `name: value` inside parentheses.
fn declaration(p: &mut Parser) -> bool {
    let Ok(name) = p.expect_ident().map(|n| n.to_string()) else { return false };
    p.skip_whitespace();
    if p.expect_colon().is_err() {
        return false;
    }
    let start = p.position();
    while p.next_including_whitespace().is_ok() {}
    let value = p.slice_from(start).trim();
    !value.is_empty() && supports_declaration(&name, value)
}

#[cfg(test)]
mod tests {
    use super::supports;

    #[test]
    fn conditions() {
        assert!(supports("(display: grid)"));
        assert!(supports("(display:flex) and (gap: 1rem)"));
        assert!(!supports("(display: nonsense)"));
        // iOS Safari only: Stripe paints a white sheet over its page in it.
        assert!(!supports("(-webkit-touch-callout: none)"));
        assert!(supports("not (-webkit-touch-callout: none)"));
        assert!(supports("(-webkit-touch-callout: none) or (display: block)"));
        assert!(!supports("(-webkit-touch-callout: none) and (display: block)"));
        assert!(supports("((display: grid) and (not (display: inline-grid-nonsense)))"));
        assert!(supports("(--anything: 1px)"));
        assert!(supports("selector(:has(a))"));
        assert!(!supports("selector(:unknown-pseudo)"));
        assert!(supports("(color: rgb(0 0 0 / 50%))"));
        assert!(!supports("(width)"));
        assert!(!supports("unknown-func(x)"));
    }
}
