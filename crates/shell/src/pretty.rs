//! Un-minifying: pretty-printers for JSON, CSS, JavaScript and HTML, for
//! the source view (like the `{}` button in a browser's dev tools).
//!
//! None of these parse their language properly; they re-flow tokens, which
//! is all pretty-printing minified code needs, and they never change what
//! the code means (strings, comments, regexes and template literals are
//! copied through untouched).

const INDENT: &str = "  ";

/// Does this look minified (long lines)?
pub fn looks_minified(src: &str) -> bool {
    let lines = src.lines().count().max(1);
    let longest = src.lines().map(str::len).max().unwrap_or(0);
    longest > 400 || src.len() / lines > 200
}

/// Pretty-print source in `language` ("json", "css", "javascript", "html").
pub fn pretty(language: &str, src: &str) -> String {
    match language {
        "json" => json(src),
        "css" => css(src),
        "javascript" => js(src),
        "html" => html(src),
        _ => src.to_owned(),
    }
}

/// Copy a quoted string starting at `i` (the quote) into `out`; returns the
/// index after it.
fn copy_string(chars: &[char], mut i: usize, out: &mut String) -> usize {
    let quote = chars[i];
    out.push(quote);
    i += 1;
    while i < chars.len() {
        let c = chars[i];
        out.push(c);
        i += 1;
        if c == '\\' && i < chars.len() {
            out.push(chars[i]);
            i += 1;
        } else if c == quote {
            break;
        }
    }
    i
}

fn newline(out: &mut String, depth: usize) {
    while out.ends_with(' ') {
        out.pop();
    }
    out.push('\n');
    for _ in 0..depth {
        out.push_str(INDENT);
    }
}

fn next_non_ws(chars: &[char], mut i: usize) -> Option<char> {
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    chars.get(i).copied()
}

// ---- JSON ----

pub fn json(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len() * 2);
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                i = copy_string(&chars, i, &mut out);
                continue;
            }
            '{' | '[' => {
                let close = if c == '{' { '}' } else { ']' };
                if next_non_ws(&chars, i + 1) == Some(close) {
                    out.push(c);
                    out.push(close);
                    // Skip to after the close.
                    i += 1;
                    while chars[i] != close {
                        i += 1;
                    }
                } else {
                    out.push(c);
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(',');
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
        i += 1;
    }
    out.push('\n');
    out
}

// ---- CSS ----

pub fn css(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len() * 2);
    let mut depth = 0usize;
    // Per open brace: does it hold declarations (vs. nested rules)?
    let mut decl_blocks: Vec<bool> = Vec::new();
    let mut parens = 0usize;
    let mut i = 0;
    let in_decls = |blocks: &Vec<bool>| blocks.last().copied().unwrap_or(false);
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' | '\'' => {
                i = copy_string(&chars, i, &mut out);
                continue;
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                let end = (i + 2..chars.len().saturating_sub(1)).find(|&j| chars[j] == '*' && chars[j + 1] == '/');
                let end = end.map_or(chars.len(), |e| e + 2);
                out.extend(&chars[i..end]);
                newline(&mut out, depth);
                i = end;
                continue;
            }
            '(' => {
                parens += 1;
                out.push(c);
            }
            ')' => {
                parens = parens.saturating_sub(1);
                out.push(c);
            }
            '{' => {
                // Declarations unless another `{` comes before the matching `}`.
                let mut j = i + 1;
                let mut nested = false;
                while j < chars.len() && chars[j] != '}' {
                    if chars[j] == '{' {
                        nested = true;
                        break;
                    }
                    j += 1;
                }
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push_str(" {");
                depth += 1;
                decl_blocks.push(!nested);
                newline(&mut out, depth);
            }
            '}' => {
                depth = depth.saturating_sub(1);
                decl_blocks.pop();
                let trimmed = out.trim_end_matches([' ', '\n']).len();
                out.truncate(trimmed);
                newline(&mut out, depth);
                out.push('}');
                newline(&mut out, depth);
                if depth == 0 {
                    newline(&mut out, depth);
                }
            }
            ';' if parens == 0 => {
                out.push(';');
                newline(&mut out, depth);
            }
            ':' if parens == 0 && in_decls(&decl_blocks) => {
                out.push_str(": ");
                while next_non_ws(&chars, i + 1).is_some() && chars.get(i + 1).is_some_and(|c| c.is_whitespace()) {
                    i += 1;
                }
            }
            ',' if parens == 0 && !in_decls(&decl_blocks) => {
                // Selector lists: one per line.
                out.push(',');
                newline(&mut out, depth);
                while chars.get(i + 1).is_some_and(|c| c.is_whitespace()) {
                    i += 1;
                }
            }
            c if c.is_whitespace() => {
                if !out.ends_with([' ', '\n', '(']) && !out.ends_with(INDENT) {
                    out.push(' ');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    let mut out = out.trim_end().to_owned();
    out.push('\n');
    out
}

// ---- JavaScript ----

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Punct(String),
    /// Strings, template literals, regexes, numbers: copied verbatim.
    Literal(String),
    Comment(String, bool),
}

const KEYWORDS_BEFORE_PAREN: &[&str] = &["if", "for", "while", "switch", "catch", "with", "function", "return", "typeof"];
/// After these, a `/` starts a regex rather than dividing.
const EXPR_KEYWORDS: &[&str] = &["return", "typeof", "case", "do", "else", "in", "of", "new", "delete", "void", "throw", "instanceof", "yield", "await"];

fn js_tokens(src: &str) -> Vec<(Tok, bool)> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks: Vec<(Tok, bool)> = Vec::new(); // (token, newline before it)
    let mut i = 0;
    let mut nl = false;
    const PUNCTS: &[&str] = &[
        ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=", "<=", ">=", "&&",
        "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "**", "<<", ">>",
    ];
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            nl |= c == '\n';
            i += 1;
            continue;
        }
        let start = i;
        let tok = if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            Tok::Comment(chars[start..i].iter().collect(), true)
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            Tok::Comment(chars[start..i].iter().collect(), false)
        } else if c == '"' || c == '\'' {
            let mut s = String::new();
            i = copy_string(&chars, i, &mut s);
            Tok::Literal(s)
        } else if c == '`' {
            // Template literal, including `${ ... }` (with nested braces/strings).
            i += 1;
            let mut depth = 0usize;
            while i < chars.len() {
                match chars[i] {
                    '\\' => i += 1,
                    '`' if depth == 0 => {
                        i += 1;
                        break;
                    }
                    '$' if chars.get(i + 1) == Some(&'{') => {
                        depth += 1;
                        i += 1;
                    }
                    '}' if depth > 0 => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            Tok::Literal(chars[start..i.min(chars.len())].iter().collect())
        } else if c == '/' && regex_allowed(toks.last().map(|t| &t.0)) {
            i += 1;
            let mut class = false;
            while i < chars.len() {
                match chars[i] {
                    '\\' => i += 1,
                    '[' => class = true,
                    ']' => class = false,
                    '/' if !class => {
                        i += 1;
                        break;
                    }
                    '\n' => break,
                    _ => {}
                }
                i += 1;
            }
            while i < chars.len() && chars[i].is_alphabetic() {
                i += 1; // flags
            }
            Tok::Literal(chars[start..i.min(chars.len())].iter().collect())
        } else if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '.' || chars[i] == '_') {
                i += 1;
            }
            Tok::Literal(chars[start..i].iter().collect())
        } else if c.is_alphanumeric() || c == '_' || c == '$' || c == '#' || !c.is_ascii() {
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$' || chars[i] == '#' || !chars[i].is_ascii()) {
                i += 1;
            }
            Tok::Word(chars[start..i].iter().collect())
        } else {
            let rest: String = chars[i..(i + 4).min(chars.len())].iter().collect();
            let p = PUNCTS.iter().find(|p| rest.starts_with(**p)).map(|p| p.to_string()).unwrap_or_else(|| c.to_string());
            i += p.chars().count();
            Tok::Punct(p)
        };
        toks.push((tok, nl));
        nl = false;
    }
    toks
}

fn regex_allowed(prev: Option<&Tok>) -> bool {
    match prev {
        None => true,
        Some(Tok::Word(w)) => EXPR_KEYWORDS.contains(&w.as_str()),
        Some(Tok::Literal(_)) => false,
        Some(Tok::Punct(p)) => !matches!(p.as_str(), ")" | "]" | "}" | "++" | "--"),
        Some(Tok::Comment(..)) => true,
    }
}

const SPACED: &[&str] = &[
    "=", "==", "===", "!=", "!==", "=>", "&&", "||", "??", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "&&=", "||=",
    "??=", "<", ">", "<=", ">=", "?", "**=", "<<=", ">>=", ">>>=",
];

pub fn js(src: &str) -> String {
    let toks = js_tokens(src);
    let mut out = String::with_capacity(src.len() * 2);
    let mut depth = 0usize;
    // Open brackets: '(' and '[' (for `;` in `for(;;)`), and '{'.
    let mut stack: Vec<char> = Vec::new();
    // A `case`/`default` waits for its `:` (ternaries inside add to this).
    let mut pending_case = 0usize;
    let mut ternaries = 0usize;
    let line_start = |out: &String| out.is_empty() || out.ends_with('\n') || out.trim_end_matches(' ').ends_with('\n');
    for (ix, (tok, nl_before)) in toks.iter().enumerate() {
        let next = toks.get(ix + 1).map(|t| &t.0);
        let prev = ix.checked_sub(1).map(|p| &toks[p].0);
        let in_parens = matches!(stack.last(), Some('(') | Some('['));
        // Keep the author's line breaks between statements (ASI depends on them).
        let continues = matches!(prev, Some(Tok::Punct(p)) if p == "," || p == "(" || p == "[" || p == "." || SPACED.contains(&p.as_str()));
        if *nl_before
            && !in_parens
            && !continues
            && !line_start(&out)
            && !matches!(tok, Tok::Punct(p) if p == ")" || p == "]" || p == "." || p == "?.")
        {
            newline(&mut out, depth);
        }
        match tok {
            Tok::Comment(text, line) => {
                if !line_start(&out) {
                    out.push(' ');
                }
                out.push_str(text);
                if *line {
                    newline(&mut out, depth);
                }
            }
            Tok::Word(w) | Tok::Literal(w) => {
                let needs_space = match prev {
                    Some(Tok::Word(_)) | Some(Tok::Literal(_)) => true,
                    // `for (...) body`: nothing else puts a word right after `)`.
                    Some(Tok::Punct(p)) => SPACED.contains(&p.as_str()) || p == "," || p == ")" || p == ":" && !in_parens || (p == "}" && matches!(tok, Tok::Word(_))),
                    _ => false,
                };
                if needs_space && !line_start(&out) && !out.ends_with(' ') {
                    out.push(' ');
                }
                if w == "case" || w == "default" && matches!(next, Some(Tok::Punct(p)) if p == ":") {
                    pending_case += 1;
                }
                out.push_str(w);
            }
            Tok::Punct(p) => match p.as_str() {
                "{" => {
                    if !line_start(&out) && !out.ends_with(['(', '[', ' ']) {
                        out.push(' ');
                    }
                    out.push('{');
                    stack.push('{');
                    if matches!(next, Some(Tok::Punct(n)) if n == "}") {
                        continue;
                    }
                    depth += 1;
                    newline(&mut out, depth);
                }
                "}" => {
                    if stack.pop() == Some('{') && !out.ends_with('{') {
                        depth = depth.saturating_sub(1);
                        newline(&mut out, depth);
                    }
                    out.push('}');
                    let glued = matches!(next, Some(Tok::Punct(n)) if matches!(n.as_str(), ";" | "," | ")" | "]" | "." | "?." ))
                        || matches!(next, Some(Tok::Word(n)) if matches!(n.as_str(), "else" | "catch" | "finally" | "while"));
                    if !glued && !matches!(stack.last(), Some('(') | Some('[')) {
                        newline(&mut out, depth);
                    }
                }
                "(" | "[" => {
                    let spaced = matches!(prev, Some(Tok::Word(w)) if KEYWORDS_BEFORE_PAREN.contains(&w.as_str()))
                        || matches!(prev, Some(Tok::Punct(q)) if SPACED.contains(&q.as_str()) || q == ",");
                    if spaced && !out.ends_with(' ') && !line_start(&out) {
                        out.push(' ');
                    }
                    out.push_str(p);
                    stack.push(p.chars().next().unwrap_or('('));
                }
                ")" | "]" => {
                    stack.pop();
                    out.push_str(p);
                }
                ";" => {
                    out.push(';');
                    if in_parens {
                        out.push(' ');
                    } else {
                        newline(&mut out, depth);
                    }
                }
                "," => {
                    out.push(',');
                    // Top-level object literal entries and declarations get their own lines.
                    if matches!(stack.last(), Some('{')) {
                        newline(&mut out, depth);
                    }
                }
                "?" => {
                    ternaries += 1;
                    out.push_str(" ? ");
                }
                ":" => {
                    if pending_case > 0 && ternaries == 0 {
                        pending_case -= 1;
                        out.push(':');
                        newline(&mut out, depth);
                    } else if ternaries > 0 {
                        ternaries -= 1;
                        out.push_str(" : ");
                    } else {
                        out.push_str(": ");
                    }
                }
                p if SPACED.contains(&p) => {
                    if !out.ends_with(' ') {
                        out.push(' ');
                    }
                    out.push_str(p);
                    out.push(' ');
                }
                p => {
                    // `+`/`-` binary operators get spaces; unary ones don't.
                    let binary = matches!(p, "+" | "-" | "*" | "/" | "%" | "|" | "&" | "^" | "**" | "<<" | ">>" | ">>>" | "instanceof")
                        && matches!(prev, Some(Tok::Word(_)) | Some(Tok::Literal(_)) | Some(Tok::Punct(_)))
                        && !matches!(prev, Some(Tok::Punct(q)) if !matches!(q.as_str(), ")" | "]" | "}" | "++" | "--"));
                    if binary {
                        if !out.ends_with(' ') {
                            out.push(' ');
                        }
                        out.push_str(p);
                        out.push(' ');
                    } else {
                        out.push_str(p);
                    }
                }
            },
        }
        if out.ends_with(" ;") {
            out.truncate(out.len() - 2);
            out.push(';');
        }
    }
    let mut out: String = out.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    out.push('\n');
    out
}

// ---- HTML ----

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];

pub fn html(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len() * 2);
    let mut depth = 0usize;
    let mut i = 0;
    let lower_name = |tag: &str| -> String {
        tag.trim_start_matches(['<', '/'])
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-')
            .collect::<String>()
            .to_ascii_lowercase()
    };
    let push_line = |out: &mut String, depth: usize, text: &str| {
        if !out.is_empty() {
            newline(out, depth);
        }
        out.push_str(text);
    };
    while i < chars.len() {
        if chars[i] == '<' {
            let start = i;
            if chars[i..].starts_with(&['<', '!', '-', '-']) {
                let end = (i + 4..chars.len().saturating_sub(2)).find(|&j| chars[j] == '-' && chars[j + 1] == '-' && chars[j + 2] == '>');
                i = end.map_or(chars.len(), |e| e + 3);
                push_line(&mut out, depth, &chars[start..i].iter().collect::<String>());
                continue;
            }
            // A tag: to the `>` outside quotes.
            let mut quote = None;
            while i < chars.len() {
                let c = chars[i];
                match quote {
                    Some(q) if c == q => quote = None,
                    Some(_) => {}
                    None if c == '"' || c == '\'' => quote = Some(c),
                    None if c == '>' => {
                        i += 1;
                        break;
                    }
                    None => {}
                }
                i += 1;
            }
            let tag: String = chars[start..i.min(chars.len())].iter().collect();
            let name = lower_name(&tag);
            let closing = tag.starts_with("</");
            let self_closing = tag.ends_with("/>") || VOID.contains(&name.as_str()) || tag.starts_with("<!");
            if closing {
                depth = depth.saturating_sub(1);
                push_line(&mut out, depth, &tag);
                continue;
            }
            push_line(&mut out, depth, &tag);
            if self_closing {
                continue;
            }
            // Raw text elements: their content until the close tag.
            if matches!(name.as_str(), "script" | "style" | "textarea" | "pre" | "title" | "xmp") {
                let close = format!("</{name}");
                let rest: String = chars[i..].iter().collect();
                let end = rest.to_ascii_lowercase().find(&close).unwrap_or(rest.len());
                let body = &rest[..end];
                let body_chars = body.chars().count();
                let formatted = match name.as_str() {
                    "script" if !body.trim().is_empty() => Some(js(body)),
                    "style" if !body.trim().is_empty() => Some(css(body)),
                    _ => None,
                };
                match formatted {
                    Some(f) => {
                        for line in f.lines() {
                            push_line(&mut out, depth + 1, line);
                        }
                        i += body_chars;
                    }
                    None => {
                        // Kept exactly (pre, textarea, title).
                        out.push_str(body);
                        i += body_chars;
                        let close_end = rest[end..].find('>').map_or(rest.len(), |e| end + e + 1);
                        out.push_str(&rest[end..close_end]);
                        i += rest[end..close_end].chars().count();
                        continue;
                    }
                }
            }
            depth += 1;
        } else {
            let start = i;
            while i < chars.len() && chars[i] != '<' {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !text.is_empty() {
                push_line(&mut out, depth, &text);
            }
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_unminifies() {
        assert_eq!(json(r#"{"a":[1,2],"b":{},"c":"x,{y}"}"#), "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {},\n  \"c\": \"x,{y}\"\n}\n");
    }

    #[test]
    fn css_unminifies() {
        assert_eq!(
            css("a:hover,b{color:red;background:url(a;b.png)}@media (min-width:9px){p{margin:0}}"),
            "a:hover,\nb {\n  color: red;\n  background: url(a;b.png)\n}\n\n@media (min-width:9px) {\n  p {\n    margin: 0\n  }\n}\n"
        );
    }

    #[test]
    fn js_unminifies() {
        let out = js("function f(a,b){if(a){return a/b}else{for(var i=0;i<3;i++)x(i)}return /x[/]y/g.test(\"a;b\")}");
        assert_eq!(
            out,
            "function f(a, b) {\n  if (a) {\n    return a / b\n  } else {\n    for (var i = 0; i < 3; i++) x(i)\n  }\n  return /x[/]y/g.test(\"a;b\")\n}\n"
        );
        // Template literals and comments survive.
        let out = js("let s=`a${b+`c`}d`;// hi\nx()");
        assert!(out.contains("`a${b+`c`}d`"), "{out}");
        assert!(out.contains("// hi\nx()"), "{out}");
        // switch/case
        assert_eq!(js("var a=[1,2]"), "var a = [1, 2]\n");
        let out = js("switch(a){case 1:b();break;default:c()}");
        assert!(out.contains("case 1:\n"), "{out}");
    }

    #[test]
    fn html_unminifies() {
        let out = html("<!doctype html><html><head><style>a{color:red}</style></head><body><p>Hi <b>there</b></p><br><pre>  keep\n me</pre></body></html>");
        assert_eq!(
            out,
            "<!doctype html>\n<html>\n  <head>\n    <style>\n      a {\n        color: red\n      }\n    </style>\n  </head>\n  <body>\n    <p>\n      Hi\n      <b>\n        there\n      </b>\n    </p>\n    <br>\n    <pre>  keep\n me</pre>\n  </body>\n</html>\n"
        );
    }

    #[test]
    fn minified_detection() {
        assert!(looks_minified(&"a".repeat(1000)));
        assert!(!looks_minified("a\nb\nc\n"));
    }
}
