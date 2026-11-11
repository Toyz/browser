//! XPath 1.0 (`document.evaluate`), over the page's tree: location paths
//! (every axis), predicates, unions, the operators and the core function
//! library. Not yet: variables, namespaces (prefixes are ignored), the
//! namespace axis. Pages use it to find things (Google Maps looks up its
//! preload links with `//link[@as='fetch' and @rel='preload']`).

use std::collections::HashMap;

use page::{Document, NodeData, NodeId};

/// What an expression evaluates to.
#[derive(Debug, Clone)]
pub enum Value {
    /// Nodes, in document order.
    Nodes(Vec<Item>),
    Str(String),
    Num(f64),
    Bool(bool),
}

/// A node of the result: a tree node, or an element's attribute.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Item {
    Node(NodeId),
    Attr(NodeId, String),
}

/// Evaluate `expr` with `context` as the context node.
pub fn evaluate(doc: &Document, context: NodeId, expr: &str) -> Result<Value, String> {
    let tokens = lex(expr)?;
    let mut p = Parser { tokens, at: 0 };
    let ast = p.expr()?;
    if p.at != p.tokens.len() {
        return Err(format!("unexpected {:?}", p.tokens[p.at]));
    }
    let mut ev = Eval { doc, order: None };
    let mut value = ev.eval(&ast, &Ctx { item: Item::Node(context), pos: 1, size: 1 })?;
    if let Value::Nodes(items) = &mut value {
        ev.sort(items);
    }
    Ok(value)
}

// ---- Lexing ----

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Slash,
    DoubleSlash,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    DotDot,
    At,
    Comma,
    Colons,
    Pipe,
    Plus,
    Minus,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Star,
    /// `*` as multiplication, and the operator names.
    Mul,
    And,
    Or,
    Div,
    Mod,
    Name(String),
    Str(String),
    Num(f64),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    // XPath's rule: after a token that ends an operand, `*` and the names
    // `and`, `or`, `div`, `mod` are operators.
    let operand_before = |out: &Vec<Tok>| {
        !matches!(
            out.last(),
            None | Some(
                Tok::At
                    | Tok::Colons
                    | Tok::LParen
                    | Tok::LBracket
                    | Tok::Comma
                    | Tok::Slash
                    | Tok::DoubleSlash
                    | Tok::Pipe
                    | Tok::Plus
                    | Tok::Minus
                    | Tok::Eq
                    | Tok::Ne
                    | Tok::Lt
                    | Tok::Le
                    | Tok::Gt
                    | Tok::Ge
                    | Tok::Mul
                    | Tok::And
                    | Tok::Or
                    | Tok::Div
                    | Tok::Mod
            )
        )
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let tok = match c {
            ' ' | '\t' | '\n' | '\r' => {
                i += 1;
                continue;
            }
            '/' if next == Some('/') => {
                i += 1;
                Tok::DoubleSlash
            }
            '/' => Tok::Slash,
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '[' => Tok::LBracket,
            ']' => Tok::RBracket,
            '.' if next == Some('.') => {
                i += 1;
                Tok::DotDot
            }
            '.' if !next.is_some_and(|n| n.is_ascii_digit()) => Tok::Dot,
            '@' => Tok::At,
            ',' => Tok::Comma,
            ':' if next == Some(':') => {
                i += 1;
                Tok::Colons
            }
            '|' => Tok::Pipe,
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '=' => Tok::Eq,
            '!' if next == Some('=') => {
                i += 1;
                Tok::Ne
            }
            '<' if next == Some('=') => {
                i += 1;
                Tok::Le
            }
            '<' => Tok::Lt,
            '>' if next == Some('=') => {
                i += 1;
                Tok::Ge
            }
            '>' => Tok::Gt,
            '*' if operand_before(&out) => Tok::Mul,
            '*' => Tok::Star,
            '"' | '\'' => {
                let end = chars[i + 1..].iter().position(|&x| x == c).ok_or("unterminated string")?;
                let text: String = chars[i + 1..i + 1 + end].iter().collect();
                i += end + 1;
                Tok::Str(text)
            }
            '$' => return Err("variables aren't supported".into()),
            c if c.is_ascii_digit() || c == '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                out.push(Tok::Num(text.parse().map_err(|_| format!("bad number {text}"))?));
                continue;
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '-' | '.')) {
                    i += 1;
                }
                // A prefix (`svg:path`): the local name is what's matched.
                if i + 1 < chars.len() && chars[i] == ':' && chars[i + 1] != ':' {
                    i += 1;
                    if chars[i] == '*' {
                        i += 1;
                        out.push(Tok::Star);
                        continue;
                    }
                    let local = i;
                    while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '-' | '.')) {
                        i += 1;
                    }
                    out.push(Tok::Name(chars[local..i].iter().collect()));
                    continue;
                }
                let name: String = chars[start..i].iter().collect();
                let tok = match name.as_str() {
                    "and" if operand_before(&out) => Tok::And,
                    "or" if operand_before(&out) => Tok::Or,
                    "div" if operand_before(&out) => Tok::Div,
                    "mod" if operand_before(&out) => Tok::Mod,
                    _ => Tok::Name(name),
                };
                out.push(tok);
                continue;
            }
            c => return Err(format!("unexpected '{c}'")),
        };
        out.push(tok);
        i += 1;
    }
    Ok(out)
}

// ---- Parsing ----

#[derive(Debug, Clone, Copy, PartialEq)]
enum Axis {
    Child,
    Descendant,
    DescendantOrSelf,
    Parent,
    Ancestor,
    AncestorOrSelf,
    FollowingSibling,
    PrecedingSibling,
    Following,
    Preceding,
    Attribute,
    SelfAxis,
}

#[derive(Debug, Clone)]
enum Test {
    /// A name (`*`: any) of the axis's principal kind (attributes on the
    /// attribute axis, elements elsewhere).
    Name(Option<String>),
    Node,
    Text,
    Comment,
    Pi,
}

#[derive(Debug, Clone)]
struct Step {
    axis: Axis,
    test: Test,
    preds: Vec<Expr>,
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone)]
enum Expr {
    Binary(Op, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Union(Box<Expr>, Box<Expr>),
    /// From the root (`/...`) or the context node.
    Path { absolute: bool, steps: Vec<Step> },
    /// A primary expression, filtered, then a path from its nodes.
    Filter { primary: Box<Expr>, preds: Vec<Expr>, steps: Vec<Step> },
    Lit(String),
    Num(f64),
    Call(String, Vec<Expr>),
}

struct Parser {
    tokens: Vec<Tok>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &Tok) -> Result<(), String> {
        if self.eat(t) { Ok(()) } else { Err(format!("expected {t:?}, found {:?}", self.peek())) }
    }

    fn expr(&mut self) -> Result<Expr, String> {
        self.binary(0)
    }

    /// Binary operators by precedence: or, and, equality, relational,
    /// additive, multiplicative.
    fn binary(&mut self, level: usize) -> Result<Expr, String> {
        const LEVELS: &[&[(Tok, Op)]] = &[
            &[(Tok::Or, Op::Or)],
            &[(Tok::And, Op::And)],
            &[(Tok::Eq, Op::Eq), (Tok::Ne, Op::Ne)],
            &[(Tok::Lt, Op::Lt), (Tok::Le, Op::Le), (Tok::Gt, Op::Gt), (Tok::Ge, Op::Ge)],
            &[(Tok::Plus, Op::Add), (Tok::Minus, Op::Sub)],
            &[(Tok::Mul, Op::Mul), (Tok::Div, Op::Div), (Tok::Mod, Op::Mod)],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        'outer: loop {
            for (tok, op) in LEVELS[level] {
                if self.eat(tok) {
                    let right = self.binary(level + 1)?;
                    left = Expr::Binary(*op, Box::new(left), Box::new(right));
                    continue 'outer;
                }
            }
            return Ok(left);
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat(&Tok::Minus) {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        let mut left = self.path()?;
        while self.eat(&Tok::Pipe) {
            left = Expr::Union(Box::new(left), Box::new(self.path()?));
        }
        Ok(left)
    }

    fn starts_primary(&self) -> bool {
        match self.peek() {
            Some(Tok::LParen | Tok::Str(_) | Tok::Num(_)) => true,
            // A function call (not a node type test, not an axis).
            Some(Tok::Name(n)) => {
                self.tokens.get(self.at + 1) == Some(&Tok::LParen) && !matches!(n.as_str(), "node" | "text" | "comment" | "processing-instruction")
            }
            _ => false,
        }
    }

    fn path(&mut self) -> Result<Expr, String> {
        if self.starts_primary() {
            let primary = self.primary()?;
            let mut preds = Vec::new();
            while self.peek() == Some(&Tok::LBracket) {
                preds.push(self.predicate()?);
            }
            let mut steps = Vec::new();
            if matches!(self.peek(), Some(Tok::Slash | Tok::DoubleSlash)) {
                self.relative(&mut steps)?;
            }
            if preds.is_empty() && steps.is_empty() {
                return Ok(primary);
            }
            return Ok(Expr::Filter { primary: Box::new(primary), preds, steps });
        }
        let mut steps = Vec::new();
        let absolute = matches!(self.peek(), Some(Tok::Slash | Tok::DoubleSlash));
        if absolute {
            if self.eat(&Tok::Slash) {
                // `/` alone: the root.
                if !self.starts_step() {
                    return Ok(Expr::Path { absolute: true, steps });
                }
            } else {
                self.at += 1;
                steps.push(descendant_or_self());
            }
        }
        steps.push(self.step()?);
        self.relative(&mut steps)?;
        Ok(Expr::Path { absolute, steps })
    }

    fn starts_step(&self) -> bool {
        matches!(self.peek(), Some(Tok::Name(_) | Tok::Star | Tok::At | Tok::Dot | Tok::DotDot))
    }

    /// More steps, each after `/` or `//`.
    fn relative(&mut self, steps: &mut Vec<Step>) -> Result<(), String> {
        loop {
            if self.eat(&Tok::Slash) {
                steps.push(self.step()?);
            } else if self.eat(&Tok::DoubleSlash) {
                steps.push(descendant_or_self());
                steps.push(self.step()?);
            } else {
                return Ok(());
            }
        }
    }

    fn step(&mut self) -> Result<Step, String> {
        if self.eat(&Tok::Dot) {
            return Ok(Step { axis: Axis::SelfAxis, test: Test::Node, preds: Vec::new() });
        }
        if self.eat(&Tok::DotDot) {
            return Ok(Step { axis: Axis::Parent, test: Test::Node, preds: Vec::new() });
        }
        let mut axis = Axis::Child;
        if self.eat(&Tok::At) {
            axis = Axis::Attribute;
        } else if let Some(Tok::Name(name)) = self.peek().cloned()
            && self.tokens.get(self.at + 1) == Some(&Tok::Colons)
        {
            axis = match name.as_str() {
                "child" => Axis::Child,
                "descendant" => Axis::Descendant,
                "descendant-or-self" => Axis::DescendantOrSelf,
                "parent" => Axis::Parent,
                "ancestor" => Axis::Ancestor,
                "ancestor-or-self" => Axis::AncestorOrSelf,
                "following-sibling" => Axis::FollowingSibling,
                "preceding-sibling" => Axis::PrecedingSibling,
                "following" => Axis::Following,
                "preceding" => Axis::Preceding,
                "attribute" => Axis::Attribute,
                "self" => Axis::SelfAxis,
                other => return Err(format!("unsupported axis {other}")),
            };
            self.at += 2;
        }
        let test = match self.peek().cloned() {
            Some(Tok::Star) => {
                self.at += 1;
                Test::Name(None)
            }
            Some(Tok::Name(name)) => {
                self.at += 1;
                if self.eat(&Tok::LParen) {
                    // (A processing instruction's target, if given, isn't checked.)
                    if let Some(Tok::Str(_)) = self.peek() {
                        self.at += 1;
                    }
                    self.expect(&Tok::RParen)?;
                    match name.as_str() {
                        "node" => Test::Node,
                        "text" => Test::Text,
                        "comment" => Test::Comment,
                        "processing-instruction" => Test::Pi,
                        other => return Err(format!("unknown node test {other}()")),
                    }
                } else {
                    Test::Name(Some(name))
                }
            }
            other => return Err(format!("expected a step, found {other:?}")),
        };
        let mut preds = Vec::new();
        while self.peek() == Some(&Tok::LBracket) {
            preds.push(self.predicate()?);
        }
        Ok(Step { axis, test, preds })
    }

    fn predicate(&mut self) -> Result<Expr, String> {
        self.expect(&Tok::LBracket)?;
        let e = self.expr()?;
        self.expect(&Tok::RBracket)?;
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek().cloned() {
            Some(Tok::LParen) => {
                self.at += 1;
                let e = self.expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Some(Tok::Str(s)) => {
                self.at += 1;
                Ok(Expr::Lit(s))
            }
            Some(Tok::Num(n)) => {
                self.at += 1;
                Ok(Expr::Num(n))
            }
            Some(Tok::Name(name)) => {
                self.at += 2;
                let mut args = Vec::new();
                if !self.eat(&Tok::RParen) {
                    loop {
                        args.push(self.expr()?);
                        if self.eat(&Tok::RParen) {
                            break;
                        }
                        self.expect(&Tok::Comma)?;
                    }
                }
                Ok(Expr::Call(name, args))
            }
            other => Err(format!("unexpected {other:?}")),
        }
    }
}

fn descendant_or_self() -> Step {
    Step { axis: Axis::DescendantOrSelf, test: Test::Node, preds: Vec::new() }
}

// ---- Evaluation ----

struct Ctx {
    item: Item,
    pos: usize,
    size: usize,
}

struct Eval<'a> {
    doc: &'a Document,
    /// Each node's place in document order (made when first needed).
    order: Option<HashMap<NodeId, usize>>,
}

impl Eval<'_> {
    fn sort(&mut self, items: &mut Vec<Item>) {
        if items.len() < 2 {
            return;
        }
        let doc = self.doc;
        let order = self.order.get_or_insert_with(|| {
            // From each tree's top (a detached subtree has its own).
            let mut tops: Vec<NodeId> = Vec::new();
            let mut map = HashMap::new();
            for item in items.iter() {
                let (Item::Node(n) | Item::Attr(n, _)) = item;
                let mut top = *n;
                while let Some(p) = doc.parent(top) {
                    top = p;
                }
                if !tops.contains(&top) {
                    tops.push(top);
                }
            }
            for top in tops {
                let base = map.len();
                for (i, n) in doc.descendants(top).enumerate() {
                    map.insert(n, base + i);
                }
            }
            map
        });
        let key = |item: &Item| match item {
            Item::Node(n) => (order.get(n).copied().unwrap_or(usize::MAX), 0, String::new()),
            Item::Attr(n, name) => (order.get(n).copied().unwrap_or(usize::MAX), 1, name.clone()),
        };
        items.sort_by_key(key);
        items.dedup();
    }

    fn eval(&mut self, e: &Expr, cx: &Ctx) -> Result<Value, String> {
        Ok(match e {
            Expr::Lit(s) => Value::Str(s.clone()),
            Expr::Num(n) => Value::Num(*n),
            Expr::Neg(e) => {
                let v = self.eval(e, cx)?;
                Value::Num(-self.number(&v))
            }
            Expr::Union(a, b) => {
                let (Value::Nodes(mut a), Value::Nodes(b)) = (self.eval(a, cx)?, self.eval(b, cx)?) else {
                    return Err("a union of things not nodes".into());
                };
                a.extend(b);
                self.sort(&mut a);
                Value::Nodes(a)
            }
            Expr::Binary(op, a, b) => match op {
                Op::Or => {
                    let a = self.eval(a, cx)?;
                    if self.boolean(&a) {
                        Value::Bool(true)
                    } else {
                        let b = self.eval(b, cx)?;
                        Value::Bool(self.boolean(&b))
                    }
                }
                Op::And => {
                    let a = self.eval(a, cx)?;
                    if !self.boolean(&a) {
                        Value::Bool(false)
                    } else {
                        let b = self.eval(b, cx)?;
                        Value::Bool(self.boolean(&b))
                    }
                }
                Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                    let (a, b) = (self.eval(a, cx)?, self.eval(b, cx)?);
                    Value::Bool(self.compare(*op, &a, &b))
                }
                _ => {
                    let (a, b) = (self.eval(a, cx)?, self.eval(b, cx)?);
                    let (a, b) = (self.number(&a), self.number(&b));
                    Value::Num(match op {
                        Op::Add => a + b,
                        Op::Sub => a - b,
                        Op::Mul => a * b,
                        Op::Div => a / b,
                        _ => a % b,
                    })
                }
            },
            Expr::Path { absolute, steps } => {
                let start = if *absolute {
                    let (Item::Node(n) | Item::Attr(n, _)) = &cx.item;
                    let mut top = *n;
                    while let Some(p) = self.doc.parent(top) {
                        top = p;
                    }
                    Item::Node(top)
                } else {
                    cx.item.clone()
                };
                Value::Nodes(self.steps(vec![start], steps)?)
            }
            Expr::Filter { primary, preds, steps } => {
                let Value::Nodes(mut items) = self.eval(primary, cx)? else {
                    return Err("a filter of something not nodes".into());
                };
                self.sort(&mut items);
                for p in preds {
                    items = self.filter(items, p)?;
                }
                Value::Nodes(self.steps(items, steps)?)
            }
            Expr::Call(name, args) => self.call(name, args, cx)?,
        })
    }

    fn steps(&mut self, mut items: Vec<Item>, steps: &[Step]) -> Result<Vec<Item>, String> {
        for step in steps {
            let mut out = Vec::new();
            for item in &items {
                let mut found = self.axis(item, step);
                for p in &step.preds {
                    found = self.filter(found, p)?;
                }
                out.extend(found);
            }
            self.sort(&mut out);
            items = out;
        }
        Ok(items)
    }

    /// Keep the items `pred` holds for (a number: the one at that position).
    fn filter(&mut self, items: Vec<Item>, pred: &Expr) -> Result<Vec<Item>, String> {
        let size = items.len();
        let mut out = Vec::new();
        for (i, item) in items.into_iter().enumerate() {
            let cx = Ctx { item: item.clone(), pos: i + 1, size };
            let keep = match self.eval(pred, &cx)? {
                Value::Num(n) => n == (i + 1) as f64,
                v => self.boolean(&v),
            };
            if keep {
                out.push(item);
            }
        }
        Ok(out)
    }

    /// The nodes along `step`'s axis from `item` that pass its test, in the
    /// axis's order (reverse axes nearest first, for positions).
    fn axis(&self, item: &Item, step: &Step) -> Vec<Item> {
        let doc = self.doc;
        let node = match item {
            Item::Node(n) => *n,
            // An attribute's only neighbours: its element (parent), itself.
            Item::Attr(n, _) => {
                return match step.axis {
                    Axis::Parent | Axis::Ancestor | Axis::AncestorOrSelf => {
                        let mut out = Vec::new();
                        if step.axis == Axis::AncestorOrSelf && self.passes(item, step) {
                            out.push(item.clone());
                        }
                        let mut at = Some(*n);
                        while let Some(a) = at {
                            let it = Item::Node(a);
                            if self.passes(&it, step) {
                                out.push(it);
                            }
                            at = if step.axis == Axis::Parent { None } else { doc.parent(a) };
                        }
                        out
                    }
                    Axis::SelfAxis if self.passes(item, step) => vec![item.clone()],
                    _ => Vec::new(),
                };
            }
        };
        let nodes: Vec<NodeId> = match step.axis {
            Axis::Attribute => {
                let Some(e) = doc.element(node) else { return Vec::new() };
                return e
                    .attrs
                    .iter()
                    .map(|a| Item::Attr(node, a.name.local.to_string()))
                    .filter(|it| self.passes(it, step))
                    .collect();
            }
            Axis::Child => doc.children(node).to_vec(),
            Axis::Descendant => doc.descendants(node).skip(1).collect(),
            Axis::DescendantOrSelf => doc.descendants(node).collect(),
            Axis::SelfAxis => vec![node],
            Axis::Parent => doc.parent(node).into_iter().collect(),
            Axis::Ancestor | Axis::AncestorOrSelf => {
                let mut out = Vec::new();
                let mut at = if step.axis == Axis::Ancestor { doc.parent(node) } else { Some(node) };
                while let Some(a) = at {
                    out.push(a);
                    at = doc.parent(a);
                }
                out
            }
            Axis::FollowingSibling | Axis::PrecedingSibling => {
                let Some(parent) = doc.parent(node) else { return Vec::new() };
                let siblings = doc.children(parent);
                let Some(i) = siblings.iter().position(|&s| s == node) else { return Vec::new() };
                if step.axis == Axis::FollowingSibling {
                    siblings[i + 1..].to_vec()
                } else {
                    siblings[..i].iter().rev().copied().collect()
                }
            }
            Axis::Following => {
                let mut out = Vec::new();
                let mut at = node;
                loop {
                    let Some(parent) = doc.parent(at) else { break };
                    let siblings = doc.children(parent);
                    if let Some(i) = siblings.iter().position(|&s| s == at) {
                        for &s in &siblings[i + 1..] {
                            out.extend(doc.descendants(s));
                        }
                    }
                    at = parent;
                }
                out
            }
            Axis::Preceding => {
                let mut out = Vec::new();
                let mut at = node;
                loop {
                    let Some(parent) = doc.parent(at) else { break };
                    let siblings = doc.children(parent);
                    if let Some(i) = siblings.iter().position(|&s| s == at) {
                        for &s in siblings[..i].iter().rev() {
                            let mut sub: Vec<NodeId> = doc.descendants(s).collect();
                            sub.reverse();
                            out.extend(sub);
                        }
                    }
                    at = parent;
                }
                out
            }
        };
        nodes.into_iter().map(Item::Node).filter(|it| self.passes(it, step)).collect()
    }

    fn passes(&self, item: &Item, step: &Step) -> bool {
        match item {
            Item::Attr(_, name) => match &step.test {
                Test::Node => true,
                Test::Name(None) => step.axis == Axis::Attribute || step.axis == Axis::SelfAxis,
                Test::Name(Some(want)) => (step.axis == Axis::Attribute || step.axis == Axis::SelfAxis) && want.eq_ignore_ascii_case(name),
                _ => false,
            },
            Item::Node(n) => {
                let data = &self.doc.node(*n).data;
                match &step.test {
                    Test::Node => true,
                    Test::Text => matches!(data, NodeData::Text(_)),
                    Test::Comment => matches!(data, NodeData::Comment(_)),
                    Test::Pi => false,
                    Test::Name(want) => match data {
                        NodeData::Element(e) => match want {
                            None => true,
                            // (HTML elements' names match in any case.)
                            Some(want) => {
                                if e.is_html() {
                                    want.eq_ignore_ascii_case(e.local_name())
                                } else {
                                    want.as_str() == &**e.local_name()
                                }
                            }
                        },
                        _ => false,
                    },
                }
            }
        }
    }

    fn string_of(&self, item: &Item) -> String {
        match item {
            Item::Attr(n, name) => self.doc.element(*n).and_then(|e| e.attr(name)).unwrap_or_default().to_owned(),
            Item::Node(n) => match &self.doc.node(*n).data {
                NodeData::Text(t) | NodeData::Comment(t) => t.clone(),
                _ => self.doc.text_content(*n),
            },
        }
    }

    fn string(&self, v: &Value) -> String {
        match v {
            Value::Str(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => number_string(*n),
            Value::Nodes(items) => items.first().map(|i| self.string_of(i)).unwrap_or_default(),
        }
    }

    fn number(&self, v: &Value) -> f64 {
        match v {
            Value::Num(n) => *n,
            Value::Bool(b) => f64::from(u8::from(*b)),
            other => to_number(&self.string(other)),
        }
    }

    fn boolean(&self, v: &Value) -> bool {
        match v {
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            Value::Nodes(items) => !items.is_empty(),
        }
    }

    fn compare(&self, op: Op, a: &Value, b: &Value) -> bool {
        let cmp_num = |x: f64, y: f64| match op {
            Op::Eq => x == y,
            Op::Ne => x != y,
            Op::Lt => x < y,
            Op::Le => x <= y,
            Op::Gt => x > y,
            _ => x >= y,
        };
        let equality = matches!(op, Op::Eq | Op::Ne);
        match (a, b) {
            (Value::Nodes(xs), Value::Nodes(ys)) => xs.iter().any(|x| {
                let sx = self.string_of(x);
                ys.iter().any(|y| {
                    let sy = self.string_of(y);
                    if equality { (sx == sy) == matches!(op, Op::Eq) } else { cmp_num(to_number(&sx), to_number(&sy)) }
                })
            }),
            (Value::Nodes(xs), other) | (other, Value::Nodes(xs)) => {
                // (Keep the operands' order for the relational operators.)
                let nodes_left = matches!(a, Value::Nodes(_));
                if let Value::Bool(b) = other {
                    let n = !xs.is_empty();
                    return if nodes_left { cmp_num(f64::from(u8::from(n)), f64::from(u8::from(*b))) } else { cmp_num(f64::from(u8::from(*b)), f64::from(u8::from(n))) };
                }
                xs.iter().any(|x| {
                    let s = self.string_of(x);
                    match other {
                        Value::Num(n) => {
                            let v = to_number(&s);
                            if nodes_left { cmp_num(v, *n) } else { cmp_num(*n, v) }
                        }
                        _ if equality => (s == self.string(other)) == matches!(op, Op::Eq),
                        _ => {
                            let (v, o) = (to_number(&s), self.number(other));
                            if nodes_left { cmp_num(v, o) } else { cmp_num(o, v) }
                        }
                    }
                })
            }
            _ if equality => {
                if matches!(a, Value::Bool(_)) || matches!(b, Value::Bool(_)) {
                    (self.boolean(a) == self.boolean(b)) == matches!(op, Op::Eq)
                } else if matches!(a, Value::Num(_)) || matches!(b, Value::Num(_)) {
                    cmp_num(self.number(a), self.number(b))
                } else {
                    (self.string(a) == self.string(b)) == matches!(op, Op::Eq)
                }
            }
            _ => cmp_num(self.number(a), self.number(b)),
        }
    }

    fn call(&mut self, name: &str, args: &[Expr], cx: &Ctx) -> Result<Value, String> {
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.eval(a, cx)?);
        }
        let arg_str = |this: &Self, i: usize| vals.get(i).map(|v| this.string(v)).unwrap_or_else(|| this.string_of(&cx.item));
        let nodes_arg = |i: usize| match vals.get(i) {
            Some(Value::Nodes(items)) => Ok(items.clone()),
            Some(_) => Err(format!("{name}() wants nodes")),
            None => Ok(vec![cx.item.clone()]),
        };
        Ok(match name {
            "last" => Value::Num(cx.size as f64),
            "position" => Value::Num(cx.pos as f64),
            "count" => Value::Num(nodes_arg(0)?.len() as f64),
            "local-name" | "name" => {
                let items = nodes_arg(0)?;
                Value::Str(match items.first() {
                    Some(Item::Attr(_, n)) => n.clone(),
                    Some(Item::Node(n)) => self.doc.element(*n).map(|e| e.local_name().to_string()).unwrap_or_default(),
                    None => String::new(),
                })
            }
            "namespace-uri" => Value::Str(String::new()),
            "id" => {
                let wanted = self.string(vals.first().ok_or("id() wants an argument")?);
                let ids: Vec<&str> = wanted.split_whitespace().collect();
                let (Item::Node(n) | Item::Attr(n, _)) = &cx.item;
                let mut top = *n;
                while let Some(p) = self.doc.parent(top) {
                    top = p;
                }
                let found = self
                    .doc
                    .descendants(top)
                    .filter(|&d| self.doc.element(d).and_then(|e| e.attr("id")).is_some_and(|id| ids.contains(&id)))
                    .map(Item::Node)
                    .collect();
                Value::Nodes(found)
            }
            "string" => Value::Str(arg_str(self, 0)),
            "concat" => Value::Str(vals.iter().map(|v| self.string(v)).collect()),
            "starts-with" => Value::Bool(arg_str(self, 0).starts_with(&arg_str(self, 1))),
            "ends-with" => Value::Bool(arg_str(self, 0).ends_with(&arg_str(self, 1))),
            "contains" => Value::Bool(arg_str(self, 0).contains(&arg_str(self, 1))),
            "substring-before" => {
                let (s, t) = (arg_str(self, 0), arg_str(self, 1));
                Value::Str(s.find(&t).map(|i| s[..i].to_owned()).unwrap_or_default())
            }
            "substring-after" => {
                let (s, t) = (arg_str(self, 0), arg_str(self, 1));
                Value::Str(s.find(&t).map(|i| s[i + t.len()..].to_owned()).unwrap_or_default())
            }
            "substring" => {
                let s: Vec<char> = arg_str(self, 0).chars().collect();
                let start = vals.get(1).map(|v| self.number(v)).unwrap_or(1.0).round();
                let end = vals.get(2).map(|v| start + self.number(v).round()).unwrap_or(f64::INFINITY);
                Value::Str(
                    s.iter()
                        .enumerate()
                        .filter(|(i, _)| {
                            let p = (*i + 1) as f64;
                            p >= start && p < end
                        })
                        .map(|(_, c)| *c)
                        .collect(),
                )
            }
            "string-length" => Value::Num(arg_str(self, 0).chars().count() as f64),
            "normalize-space" => Value::Str(arg_str(self, 0).split_whitespace().collect::<Vec<_>>().join(" ")),
            "translate" => {
                let (s, from, to) = (arg_str(self, 0), arg_str(self, 1), arg_str(self, 2));
                let (from, to): (Vec<char>, Vec<char>) = (from.chars().collect(), to.chars().collect());
                Value::Str(
                    s.chars()
                        .filter_map(|c| match from.iter().position(|&f| f == c) {
                            Some(i) => to.get(i).copied(),
                            None => Some(c),
                        })
                        .collect(),
                )
            }
            "lower-case" => Value::Str(arg_str(self, 0).to_lowercase()),
            "upper-case" => Value::Str(arg_str(self, 0).to_uppercase()),
            "boolean" => Value::Bool(vals.first().is_some_and(|v| self.boolean(v))),
            "not" => Value::Bool(!vals.first().is_some_and(|v| self.boolean(v))),
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "lang" => {
                let want = arg_str(self, 0).to_ascii_lowercase();
                let (Item::Node(n) | Item::Attr(n, _)) = &cx.item;
                let mut at = Some(*n);
                let mut lang = None;
                while let Some(a) = at {
                    if let Some(l) = self.doc.element(a).and_then(|e| e.attr("lang")) {
                        lang = Some(l.to_ascii_lowercase());
                        break;
                    }
                    at = self.doc.parent(a);
                }
                Value::Bool(lang.is_some_and(|l| l == want || l.starts_with(&format!("{want}-"))))
            }
            "number" => Value::Num(match vals.first() {
                Some(v) => self.number(v),
                None => to_number(&self.string_of(&cx.item)),
            }),
            "sum" => Value::Num(nodes_arg(0)?.iter().map(|i| to_number(&self.string_of(i))).sum()),
            "floor" => Value::Num(vals.first().map(|v| self.number(v)).unwrap_or(f64::NAN).floor()),
            "ceiling" => Value::Num(vals.first().map(|v| self.number(v)).unwrap_or(f64::NAN).ceil()),
            "round" => Value::Num(vals.first().map(|v| self.number(v)).unwrap_or(f64::NAN).round()),
            other => return Err(format!("unknown function {other}()")),
        })
    }
}

fn to_number(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-') {
        return f64::NAN;
    }
    t.parse().unwrap_or(f64::NAN)
}

fn number_string(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}
