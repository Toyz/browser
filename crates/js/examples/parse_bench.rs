//! How long Boa takes to parse, compile and run a script on its own:
//! `cargo run --release -p js --example parse_bench -- file.js`
use boa_engine::{Context, Script, Source};
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("a .js file");
    let code = std::fs::read(&path).unwrap();
    let mut cx = Context::default();
    let t = Instant::now();
    let script = Script::parse(Source::from_bytes(&code), None, &mut cx).expect("parse");
    println!("parse   {:?}", t.elapsed());
    let t = Instant::now();
    script.codeblock(&mut cx).expect("compile");
    println!("compile {:?}", t.elapsed());
    let t = Instant::now();
    let r = script.evaluate(&mut cx);
    println!("run     {:?} ({})", t.elapsed(), if r.is_ok() { "ok" } else { "threw" });
}
