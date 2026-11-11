//! Parse one script over and over (for profiling the parser):
//! `cargo run --release -p js --example parse_loop -- file.js 20`
use boa_engine::{Context, Script, Source};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let path = std::env::args().nth(1).expect("a .js file");
    let n: usize = std::env::args().nth(2).and_then(|n| n.parse().ok()).unwrap_or(20);
    let code = std::fs::read(&path).unwrap();
    let mut cx = Context::default();
    let t = std::time::Instant::now();
    for _ in 0..n {
        Script::parse(Source::from_bytes(&code), None, &mut cx).expect("parse");
    }
    println!("{:?} per parse", t.elapsed() / n as u32);
}
