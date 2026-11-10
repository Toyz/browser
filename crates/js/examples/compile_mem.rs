//! How much memory a script takes in Boa: its syntax tree after parsing,
//! and its compiled code after compiling (the tree dropped).
//!
//! `cargo run --release -p js --example compile_mem -- big.js`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use boa_engine::{Context, Script, Source};

struct Counting;

static HELD: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator, counting bytes held.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        HELD.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        HELD.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        HELD.fetch_add(new_size, Ordering::Relaxed);
        HELD.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn mb(n: usize) -> f64 {
    n as f64 / 1_048_576.0
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--sizes") {
        use boa_ast::*;
        println!("Expression {}", std::mem::size_of::<Expression>());
        println!("Statement {}", std::mem::size_of::<Statement>());
        println!("StatementListItem {}", std::mem::size_of::<StatementListItem>());
        println!("Declaration {}", std::mem::size_of::<Declaration>());
        println!("expression::Identifier {}", std::mem::size_of::<expression::Identifier>());
        println!("expression::Call {}", std::mem::size_of::<expression::Call>());
        println!("expression::access::PropertyAccess {}", std::mem::size_of::<expression::access::PropertyAccess>());
        println!("expression::operator::Binary {}", std::mem::size_of::<expression::operator::Binary>());
        println!("expression::literal::Literal {}", std::mem::size_of::<expression::literal::Literal>());
        println!("function::FunctionExpression {}", std::mem::size_of::<function::FunctionExpression>());
        println!("function::FunctionBody {}", std::mem::size_of::<function::FunctionBody>());
        println!("function::FormalParameterList {}", std::mem::size_of::<function::FormalParameterList>());
        println!("scope::Scope {}", std::mem::size_of::<scope::Scope>());
        println!("  S Block {}", std::mem::size_of::<statement::Block>());
        println!("  S Var {}", std::mem::size_of::<declaration::VarDeclaration>());
        println!("  S If {}", std::mem::size_of::<statement::If>());
        println!("  S DoWhileLoop {}", std::mem::size_of::<statement::iteration::DoWhileLoop>());
        println!("  S WhileLoop {}", std::mem::size_of::<statement::iteration::WhileLoop>());
        println!("  S ForLoop {}", std::mem::size_of::<statement::iteration::ForLoop>());
        println!("  S ForInLoop {}", std::mem::size_of::<statement::iteration::ForInLoop>());
        println!("  S ForOfLoop {}", std::mem::size_of::<statement::iteration::ForOfLoop>());
        println!("  S Switch {}", std::mem::size_of::<statement::Switch>());
        println!("  S Continue {}", std::mem::size_of::<statement::iteration::Continue>());
        println!("  S Break {}", std::mem::size_of::<statement::iteration::Break>());
        println!("  S Return {}", std::mem::size_of::<statement::Return>());
        println!("  S Labelled {}", std::mem::size_of::<statement::Labelled>());
        println!("  S Throw {}", std::mem::size_of::<statement::Throw>());
        println!("  S Try {}", std::mem::size_of::<statement::Try>());
        println!("  S With {}", std::mem::size_of::<statement::With>());
        println!("  D function::FunctionDeclaration {}", std::mem::size_of::<function::FunctionDeclaration>());
        println!("  D function::GeneratorDeclaration {}", std::mem::size_of::<function::GeneratorDeclaration>());
        println!("  D function::AsyncFunctionDeclaration {}", std::mem::size_of::<function::AsyncFunctionDeclaration>());
        println!("  D function::AsyncGeneratorDeclaration {}", std::mem::size_of::<function::AsyncGeneratorDeclaration>());
        println!("  D declaration::LexicalDeclaration {}", std::mem::size_of::<declaration::LexicalDeclaration>());
        println!("  This {}", std::mem::size_of::<expression::This>());
        println!("  Identifier {}", std::mem::size_of::<expression::Identifier>());
        println!("  literal::Literal {}", std::mem::size_of::<expression::literal::Literal>());
        println!("  RegExpLiteral {}", std::mem::size_of::<expression::RegExpLiteral>());
        println!("  literal::ArrayLiteral {}", std::mem::size_of::<expression::literal::ArrayLiteral>());
        println!("  literal::ObjectLiteral {}", std::mem::size_of::<expression::literal::ObjectLiteral>());
        println!("  Spread {}", std::mem::size_of::<expression::Spread>());
        println!("  function::FunctionExpression {}", std::mem::size_of::<function::FunctionExpression>());
        println!("  function::ArrowFunction {}", std::mem::size_of::<function::ArrowFunction>());
        println!("  function::AsyncArrowFunction {}", std::mem::size_of::<function::AsyncArrowFunction>());
        println!("  function::GeneratorExpression {}", std::mem::size_of::<function::GeneratorExpression>());
        println!("  function::AsyncFunctionExpression {}", std::mem::size_of::<function::AsyncFunctionExpression>());
        println!("  function::AsyncGeneratorExpression {}", std::mem::size_of::<function::AsyncGeneratorExpression>());
        println!("  literal::TemplateLiteral {}", std::mem::size_of::<expression::literal::TemplateLiteral>());
        println!("  access::PropertyAccess {}", std::mem::size_of::<expression::access::PropertyAccess>());
        println!("  New {}", std::mem::size_of::<expression::New>());
        println!("  Call {}", std::mem::size_of::<expression::Call>());
        println!("  SuperCall {}", std::mem::size_of::<expression::SuperCall>());
        println!("  ImportCall {}", std::mem::size_of::<expression::ImportCall>());
        println!("  Optional {}", std::mem::size_of::<expression::Optional>());
        println!("  TaggedTemplate {}", std::mem::size_of::<expression::TaggedTemplate>());
        println!("  NewTarget {}", std::mem::size_of::<expression::NewTarget>());
        println!("  ImportMeta {}", std::mem::size_of::<expression::ImportMeta>());
        println!("  operator::Assign {}", std::mem::size_of::<expression::operator::Assign>());
        println!("  operator::Unary {}", std::mem::size_of::<expression::operator::Unary>());
        println!("  operator::Update {}", std::mem::size_of::<expression::operator::Update>());
        println!("  operator::Binary {}", std::mem::size_of::<expression::operator::Binary>());
        println!("  operator::BinaryInPrivate {}", std::mem::size_of::<expression::operator::BinaryInPrivate>());
        println!("  operator::Conditional {}", std::mem::size_of::<expression::operator::Conditional>());
        println!("  Await {}", std::mem::size_of::<expression::Await>());
        println!("  Yield {}", std::mem::size_of::<expression::Yield>());
        println!("  Parenthesized {}", std::mem::size_of::<expression::Parenthesized>());
        return;
    }
    let path = std::env::args().nth(1).expect("a script");
    let source = std::fs::read(&path).expect("readable");
    let mut cx = Context::default();
    let base = HELD.load(Ordering::Relaxed);
    let t = std::time::Instant::now();
    let script = Script::parse(Source::from_bytes(&source), None, &mut cx).expect("parses");
    let parsed = HELD.load(Ordering::Relaxed);
    let parse_time = t.elapsed();
    let t = std::time::Instant::now();
    script.codeblock(&mut cx).expect("compiles");
    let compiled = HELD.load(Ordering::Relaxed);
    println!("source {:.1} MB", mb(source.len()));
    println!("after parse: +{:.1} MB (tree + source text) in {parse_time:?}", mb(parsed - base));
    println!("after compile: +{:.1} MB (code + source text; tree dropped) in {:?}", mb(compiled - base), t.elapsed());
}
