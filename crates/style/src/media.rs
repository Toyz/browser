//! Media query evaluation (`@media`, `<link media>`, `@import ... media`).

use crate::values::ResolveCx;

/// What the page is being displayed on.
#[derive(Debug, Clone, Copy)]
pub struct MediaEnv {
    pub width: f32,
    pub height: f32,
    pub dark: bool,
    /// We don't run scripts (yet), so `(scripting: none)` matches.
    pub scripting: bool,
}

impl Default for MediaEnv {
    fn default() -> Self {
        Self { width: 1100.0, height: 800.0, dark: false, scripting: false }
    }
}

impl MediaEnv {
    /// A length in a media query: `768px`, `40em`, `calc(1120px - 1px)`...
    fn length(&self, value: &str) -> Option<f32> {
        let mut parser = cssparser::Parser::new(value.trim());
        let spec = crate::props::parse_len(&mut parser).ok()?;
        parser.expect_exhausted().ok()?;
        // In media queries, `em` means the initial font size.
        let cx = ResolveCx {
            font_size: 16.0,
            root_font_size: 16.0,
            viewport_width: self.width,
            viewport_height: self.height,
        };
        match spec.resolve(&cx) {
            crate::values::Length::Px(v) => Some(v),
            _ => None,
        }
    }
}

/// Could this query list match on *some* screen (any size, either color
/// scheme)? Print-only stylesheets and the like can be dropped at parse time.
pub fn can_ever_match(list: &str) -> bool {
    [(320.0, 640.0), (1280.0, 900.0), (3000.0, 2000.0)].into_iter().any(|(width, height)| {
        [false, true].into_iter().any(|dark| matches(list, &MediaEnv { width, height, dark, scripting: false }))
    })
}

/// Does a comma-separated media query list match? An empty list matches.
pub fn matches(list: &str, env: &MediaEnv) -> bool {
    let list = list.trim().to_ascii_lowercase();
    if list.is_empty() {
        return true;
    }
    split_top_level(&list, ',').iter().any(|q| eval_query(q, env))
}

fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn eval_query(q: &str, env: &MediaEnv) -> bool {
    let q = q.trim();
    if let Some(rest) = q.strip_prefix("not ") {
        // `not screen and (...)` negates the whole query; `not (...)` is a condition.
        return !eval_query(rest, env);
    }
    let q = q.strip_prefix("only ").unwrap_or(q);
    eval_condition(q, env)
}

/// Terms joined by `and` / `or` at the top level.
fn eval_condition(s: &str, env: &MediaEnv) -> bool {
    let mut result: Option<bool> = None;
    let mut connective = "and";
    let mut rest = s.trim();
    while !rest.is_empty() {
        let (term, after) = if rest.starts_with('(') {
            let close = matching_paren(rest).unwrap_or(rest.len() - 1);
            (&rest[..=close], &rest[close + 1..])
        } else {
            match rest.find(char::is_whitespace) {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, ""),
            }
        };
        rest = after.trim_start();
        match term {
            "and" | "or" => {
                connective = if term == "and" { "and" } else { "or" };
                continue;
            }
            "not" => {
                // `not (cond)`
                let close = matching_paren(rest).unwrap_or(rest.len().saturating_sub(1));
                let value = !eval_term(&rest[..=close.min(rest.len() - 1)], env);
                rest = rest[(close + 1).min(rest.len())..].trim_start();
                result = Some(combine(result, value, connective));
                continue;
            }
            _ => {}
        }
        let value = eval_term(term, env);
        result = Some(combine(result, value, connective));
    }
    result.unwrap_or(true)
}

fn combine(acc: Option<bool>, value: bool, connective: &str) -> bool {
    match acc {
        None => value,
        Some(a) if connective == "or" => a || value,
        Some(a) => a && value,
    }
}

fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn eval_term(term: &str, env: &MediaEnv) -> bool {
    let term = term.trim();
    if let Some(inner) = term.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
        let inner = inner.trim();
        if inner.starts_with('(') || inner.starts_with("not ") {
            return eval_condition(inner, env);
        }
        return eval_feature(inner, env);
    }
    // A media type.
    matches!(term, "all" | "screen")
}

fn eval_feature(f: &str, env: &MediaEnv) -> bool {
    // Range syntax: `width >= 600px`, `400px <= width <= 700px`.
    if f.contains('<') || f.contains('>') || (f.contains('=') && !f.contains(':')) {
        return eval_range(f, env);
    }
    let (name, value) = match f.split_once(':') {
        Some((n, v)) => (n.trim(), Some(v.trim())),
        None => (f.trim(), None),
    };
    let len = |v: Option<&str>| v.and_then(|v| env.length(v));
    match (name, value) {
        ("min-width", v) => len(v).is_some_and(|l| env.width >= l),
        ("max-width", v) => len(v).is_some_and(|l| env.width <= l),
        ("width", v) => len(v).is_some_and(|l| (env.width - l).abs() < 0.5),
        ("min-height", v) => len(v).is_some_and(|l| env.height >= l),
        ("max-height", v) => len(v).is_some_and(|l| env.height <= l),
        ("prefers-color-scheme", Some(v)) => (v == "dark") == env.dark,
        ("prefers-reduced-motion" | "prefers-contrast" | "prefers-reduced-transparency" | "prefers-reduced-data", v) => {
            v.is_none_or(|v| v == "no-preference")
        }
        ("hover" | "any-hover", v) => v.is_none_or(|v| v == "hover"),
        ("pointer" | "any-pointer", v) => v.is_none_or(|v| v == "fine"),
        ("orientation", Some(v)) => (v == "landscape") == (env.width >= env.height),
        ("scripting", Some(v)) => (v == "none") != env.scripting,
        ("forced-colors" | "inverted-colors", v) => v.is_some_and(|v| v == "none"),
        ("display-mode", Some(v)) => v == "browser",
        ("color" | "min-color" | "grid", _) => name != "grid",
        ("min-resolution" | "-webkit-min-device-pixel-ratio" | "min--moz-device-pixel-ratio", Some(v)) => {
            let n: f32 = v.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().unwrap_or(1.0);
            let dppx = if v.ends_with("dpi") { n / 96.0 } else { n };
            2.0 >= dppx
        }
        _ => false,
    }
}

fn eval_range(f: &str, env: &MediaEnv) -> bool {
    // Tokenize into operands and operators.
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut chars = f.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, '<' | '>' | '=') {
            if !cur.trim().is_empty() {
                parts.push(cur.trim().to_owned());
            }
            cur.clear();
            let mut op = c.to_string();
            if chars.peek() == Some(&'=') {
                op.push(chars.next().unwrap());
            }
            parts.push(op);
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_owned());
    }
    let value = |s: &str| -> Option<f32> {
        match s {
            "width" => Some(env.width),
            "height" => Some(env.height),
            other => env.length(other),
        }
    };
    // Evaluate each `a op b` pair in the chain.
    parts.windows(3).step_by(2).all(|w| {
        let (Some(a), Some(b)) = (value(&w[0]), value(&w[2])) else { return false };
        match w[1].as_str() {
            "<" => a < b,
            "<=" => a <= b,
            ">" => a > b,
            ">=" => a >= b,
            "=" => (a - b).abs() < 0.5,
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(width: f32) -> MediaEnv {
        MediaEnv { width, height: 800.0, dark: false, scripting: false }
    }

    #[test]
    fn media_queries() {
        let e = env(1000.0);
        assert!(matches("", &e));
        assert!(matches("screen", &e));
        assert!(!matches("print", &e));
        assert!(matches("print, screen", &e));
        assert!(matches("screen and (min-width: 768px)", &e));
        assert!(!matches("screen and (max-width: 768px)", &e));
        assert!(matches("(min-width: 40em) and (max-width: 80em)", &e));
        assert!(!matches("not screen", &e));
        assert!(matches("not print", &e));
        assert!(matches("(width >= 600px)", &e));
        assert!(!matches("(400px <= width <= 700px)", &e));
        assert!(matches("(400px <= width <= 1200px)", &e));
        assert!(!matches("(prefers-color-scheme: dark)", &e));
        assert!(matches("(prefers-color-scheme: light)", &e));
        assert!(matches("(max-width: 500px) or (min-width: 900px)", &e));
        assert!(matches("not (max-width: 500px)", &e));
        assert!(matches("(scripting: none)", &e));
    }
}
