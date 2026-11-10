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
