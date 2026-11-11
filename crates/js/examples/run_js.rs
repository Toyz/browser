//! Run scripts in the bare engine, the same way Node would run them (for
//! comparing the two on one program):
//! `cargo run --release -p js --example run_js -- a.js b.js`
//! `print(s)` writes a line to stdout. After the files, a global
//! `__drain()` (if the scripts define one) is called, with promise jobs run
//! between calls, until it returns false: the scripts' own timer queue.
use boa_engine::{Context, JsValue, NativeFunction, Source, js_string};

fn main() {
    // On a big stack, with the page's limits (`dom.rs`), as Feather runs pages.
    std::thread::Builder::new().stack_size(256 << 20).spawn(run).expect("thread").join().ok();
}

fn run() {
    let mut cx = Context::default();
    cx.runtime_limits_mut().set_loop_iteration_limit(200_000_000);
    cx.runtime_limits_mut().set_recursion_limit(10_000);
    cx.runtime_limits_mut().set_stack_size_limit(1 << 20);
    let print = NativeFunction::from_fn_ptr(|_, a, cx| {
        let s = a.first().cloned().unwrap_or_default().to_string(cx)?.to_std_string_escaped();
        println!("{s}");
        Ok(JsValue::undefined())
    });
    cx.register_global_builtin_callable(js_string!("print"), 1, print).expect("print");
    for path in std::env::args().skip(1) {
        let code = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        if let Err(e) = cx.eval(Source::from_bytes(&code)) {
            eprintln!("{path}: {e}");
        }
        cx.run_jobs().ok();
    }
    for _ in 0..100_000 {
        let drain = cx.global_object().get(js_string!("__drain"), &mut cx).ok().and_then(|f| f.as_callable());
        let Some(drain) = drain else { break };
        let more = drain.call(&JsValue::undefined(), &[], &mut cx).map(|v| v.to_boolean()).unwrap_or(false);
        cx.run_jobs().ok();
        if !more {
            break;
        }
    }
}
