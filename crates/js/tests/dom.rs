use std::sync::Arc;
use std::time::Duration;

use url::Url;

fn run(html: &str) -> page::Document {
    let doc = html::parse_scripted(html, Url::parse("https://example.com/a/b.html").unwrap(), true);
    let opts = js::Options {
        fetch_text: Arc::new(|_| None),
        dark: false,
        budget: Duration::from_secs(5),
        debug: true,
        session: 0,
    };
    js::start(doc, opts).0
}

fn text_of(doc: &page::Document, id: &str) -> String {
    let n = doc
        .descendants(doc.root())
        .find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some(id))
        .unwrap_or_else(|| panic!("no #{id}"));
    doc.text_content(n)
}

#[test]
fn scripts_build_and_change_the_dom() {
    let doc = run(r##"<html class="client-nojs"><body><p id="x">old</p><ul id="list"></ul>
        <script>
          document.documentElement.className = document.documentElement.className.replace("client-nojs", "client-js");
          document.getElementById("x").textContent = "new";
          const ul = document.querySelector("#list");
          for (const name of ["a", "b", "c"]) {
            const li = document.createElement("li");
            li.classList.add("item", name);
            li.append(name.toUpperCase());
            ul.appendChild(li);
          }
          ul.insertAdjacentHTML("beforeend", "<li class=item>d <b>bold</b></li>");
          document.body.dataset.count = document.querySelectorAll(".item").length;
        </script></body></html>"##);
    let html = doc.descendants(doc.root()).find(|&n| doc.element(n).is_some_and(|e| &*e.name.local == "html")).unwrap();
    assert_eq!(doc.element(html).unwrap().attr("class"), Some("client-js"));
    assert_eq!(text_of(&doc, "x"), "new");
    assert_eq!(text_of(&doc, "list"), "ABCd bold");
    let body = doc.body().unwrap();
    assert_eq!(doc.element(body).unwrap().attr("data-count"), Some("4"));
}

#[test]
fn timers_promises_async_and_events() {
    let doc = run(r##"<body><p id="out"></p><button id="b">go</button>
        <script>
          const out = document.getElementById("out");
          const log = (s) => { out.textContent += s; };
          setTimeout(() => log("T"), 0);
          Promise.resolve().then(() => log("P"));
          (async () => { await null; log("A"); })();
          document.addEventListener("DOMContentLoaded", () => log("D"));
          window.addEventListener("load", () => log("L"));
          document.getElementById("b").addEventListener("click", (e) => { log("C"); e.preventDefault(); });
          document.getElementById("b").click();
          log("S");
        </script></body>"##);
    // Sync first, then microtasks, then DOMContentLoaded, load, and the 0ms timer.
    assert_eq!(text_of(&doc, "out"), "CSPADLT");
}

#[test]
fn errors_dont_stop_the_page() {
    let doc = run(r##"<body><p id="x">1</p>
        <script>throw new Error("boom")</script>
        <script>document.getElementById("x").textContent = "2"</script></body>"##);
    assert_eq!(text_of(&doc, "x"), "2");
}

/// `PAGE=file.html cargo test -p js probe -- --ignored --nocapture`: run a
/// saved page's inline scripts and describe the DOM after.
#[test]
#[ignore]
fn probe() {
    let path = std::env::var("PAGE").unwrap();
    let html = std::fs::read_to_string(&path).unwrap();
    let base = Url::parse(&std::env::var("BASE").unwrap_or("https://example.com/".into())).unwrap();
    let doc = html::parse_scripted(&html, base, true);
    let opts = js::Options { fetch_text: Arc::new(net::fetch_text_cached), dark: false, budget: Duration::from_secs(20), debug: true, session: 0 };
    let doc = js::start(doc, opts).0;
    let root_el = doc.children(doc.root()).iter().copied().find(|&n| doc.element(n).is_some()).unwrap();
    println!("html class: {:?}", doc.element(root_el).unwrap().attr("class"));
    let body = doc.body();
    println!("body: {:?}", body.map(|b| doc.children(b).len()));
    for &c in doc.children(root_el) {
        if let Some(e) = doc.element(c) {
            if &*e.name.local == "body" {
                for &k in doc.children(c) {
                    if let Some(ke) = doc.element(k) {
                        println!("   body > <{}> {:?} {:?}", ke.name.local, ke.attr("class").map(|c| c.chars().take(50).collect::<String>()), ke.attr("style"));
                    }
                }
            }
            println!("<{}> {} children, attrs {:?}", e.name.local, doc.children(c).len(), e.attrs.iter().map(|a| (a.name.local.to_string(), a.value.chars().take(60).collect::<String>())).collect::<Vec<_>>());
        }
    }
}

#[test]
fn intl_works() {
    let doc = run(r##"<body><p id="d"></p><p id="n"></p><p id="z"></p><script>
        const f = new Intl.DateTimeFormat("en-US", { hour12: false, timeZone: "UTC", year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit" });
        const parts = f.formatToParts(Date.UTC(2024, 0, 2, 3, 4, 5));
        document.getElementById("d").textContent = parts.filter((p) => p.type !== "literal").map((p) => p.type + "=" + p.value).join(" ");
        document.getElementById("n").textContent = (1234567.891).toLocaleString("en-US");
        document.getElementById("z").textContent = Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", dateStyle: "full", hour: "numeric", minute: "2-digit", timeZoneName: "short" }).format(new Date(Date.UTC(2024, 6, 2, 3, 4, 5)));
    </script></body>"##);
    assert_eq!(text_of(&doc, "d"), "month=01 day=02 year=2024 hour=03 minute=04 second=05");
    assert_eq!(text_of(&doc, "z"), "Monday, July 1, 2024, 11:04 PM EDT");
    assert_eq!(text_of(&doc, "n"), "1,234,567.891");
}

#[test]
#[ignore]
fn probe_text() {
    let html = std::fs::read_to_string(std::env::var("PAGE").unwrap()).unwrap();
    let base = Url::parse(&std::env::var("BASE").unwrap_or("https://example.com/".into())).unwrap();
    let doc = html::parse_scripted(&html, base, true);
    let opts = js::Options { fetch_text: Arc::new(net::fetch_text_cached), dark: false, budget: Duration::from_secs(20), debug: true, session: 0 };
    let doc = js::start(doc, opts).0;
    println!("{}", text_of(&doc, "o"));
}

/// Real network: `cargo test -p js cors -- --ignored`.
#[test]
#[ignore]
fn cors_fetch_and_xhr() {
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p><script>
        const out = [];
        const log = (s) => { out.push(s); document.getElementById("o").textContent = out.join(" | "); };
        (async () => {
          const r = await fetch("https://httpbin.org/get?x=1");
          const j = await r.json();
          log("get " + r.status + " " + r.type + " " + j.args.x);
          const p = await fetch("https://httpbin.org/anything", { method: "PUT", headers: { "X-Test": "yes", "Content-Type": "application/json" }, body: JSON.stringify({ a: 1 }) });
          const pj = await p.json();
          log("preflighted " + p.status + " " + pj.headers["X-Test"] + " " + pj.json.a);
          const c = await fetch("https://httpbin.org/cookies", { credentials: "include" });
          log("credentials " + c.status);
          try { await fetch("https://news.ycombinator.com/"); log("blocked? no"); } catch (e) { log("blocked " + e.name); }
          const xhr = new XMLHttpRequest();
          xhr.open("POST", "https://httpbin.org/post");
          xhr.responseType = "json";
          xhr.onload = () => log("xhr " + xhr.status + " " + xhr.response.form.k);
          const f = new FormData(); f.append("k", "v");
          xhr.send(new URLSearchParams(f));
        })();
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(30), debug: true, session: 0 };
    let (doc, host) = js::start(doc, opts);
    // The first paint waits for quick requests; the rest come as updates.
    let mut text = text_of(&doc, "o");
    if let Some(host) = &host {
        while !text.contains("xhr") {
            match host.updates.recv_blocking() {
                Ok(js::FromJs::Document(d)) => text = text_of(&d, "o"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    println!("{text}");
    assert_eq!(text, "get 200 cors 1 | preflighted 200 yes 1 | credentials 200 | blocked TypeError | xhr 200 v");
}

#[test]
fn modules_static_dynamic_and_top_level_await() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/modules/");
    let base = Url::from_directory_path(&dir).unwrap().join("page.html").unwrap();
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p>
        <script type="module">
          import { greet, meta } from "./lib.js";
          const lazy = await import("./lazy.js");
          document.getElementById("o").textContent = greet("modules") + " " + meta + " " + lazy.default;
        </script>
        <script nomodule>document.getElementById("o").textContent = "nomodule ran";</script></body>"##,
        base,
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(net::fetch_text_cached), dark: false, budget: Duration::from_secs(5), debug: true, session: 0 };
    let doc = js::start(doc, opts).0;
    assert_eq!(text_of(&doc, "o"), "hello modules true 42");
}

#[test]
fn custom_elements_and_shadow_dom() {
    let doc = run(r##"<body><x-card id="c"><span slot="title">Hello</span><p id="body">light body</p></x-card>
        <script>
          const log = [];
          class Card extends HTMLElement {
            static observedAttributes = ["tone"];
            constructor() { super(); this.attachShadow({ mode: "open" }); log.push("ctor"); }
            connectedCallback() {
              log.push("connected");
              const sheet = new CSSStyleSheet();
              sheet.replaceSync(":host { display: block } .t { color: red } ::slotted(p) { margin: 0 }");
              this.shadowRoot.adoptedStyleSheets = [sheet];
              this.shadowRoot.innerHTML = '<h2 class="t" id="h"><slot name="title">untitled</slot></h2><div id="inner"><slot></slot></div><slot name="none">fallback</slot>';
            }
            attributeChangedCallback(n, o, v) { log.push(n + "=" + v); }
          }
          customElements.define("x-card", Card);
          document.getElementById("c").setAttribute("tone", "warm");
          const made = document.createElement("x-card");
          document.body.appendChild(made);
          document.body.dataset.log = log.join(",") + "|" + (made instanceof Card) + "|" + document.getElementById("c").shadowRoot.querySelector("#h").textContent;
        </script></body>"##);
    let body = doc.body().unwrap();
    assert_eq!(doc.element(body).unwrap().attr("data-log"), Some("ctor,connected,tone=warm,ctor,connected|true|untitled"));
    // Flattened: the host renders its shadow tree, slots hold the light nodes.
    let card = doc.descendants(doc.root()).find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some("c")).unwrap();
    assert_eq!(text_of(&doc, "h"), "Hello");
    assert_eq!(text_of(&doc, "inner"), "light body");
    assert!(doc.text_content(card).contains("fallback"));
    let css: String = doc
        .descendants(card)
        .filter(|&n| doc.element(n).is_some_and(|e| &*e.name.local == "style"))
        .map(|n| doc.text_content(n))
        .collect();
    let host = doc.element(card).unwrap().attr("data-feather-host").unwrap().to_owned();
    assert!(css.contains(&format!("[data-feather-host=\"{host}\"] .t")), "{css}");
    assert!(css.contains(&format!("[data-feather-host=\"{host}\"]{{")) || css.contains(&format!("[data-feather-host=\"{host}\"] {{")), "{css}");
    assert!(css.contains(&format!("[data-feather-host=\"{host}\"]  p")) || css.contains(&format!("[data-feather-host=\"{host}\"] p")), "{css}");
}

/// Real network: `cargo test -p js websocket -- --ignored`.
#[test]
#[ignore]
fn websocket_echo() {
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p><script>
        const out = [];
        const log = (s) => { out.push(s); document.getElementById("o").textContent = out.join(" | "); };
        const ws = new WebSocket("wss://echo.websocket.org/");
        ws.onopen = () => { log("open"); ws.send("ping from feather"); };
        ws.onmessage = (e) => { if (String(e.data).includes("feather")) { log("echo " + e.data); ws.close(1000, "bye"); } };
        ws.onclose = (e) => log("close " + e.code + " " + e.wasClean);
        ws.onerror = () => log("error");
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(10), debug: true, session: 0 };
    let (doc, host) = js::start(doc, opts);
    let mut text = text_of(&doc, "o");
    if let Some(host) = &host {
        while !text.contains("close") {
            match host.updates.recv_blocking() {
                Ok(js::FromJs::Document(d)) => text = text_of(&d, "o"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    assert_eq!(text, "open | echo ping from feather | close 1000 true");
}

/// `PAGE=… BASE=… cargo test -p js probe_tree -- --ignored --nocapture`:
/// run a page for a few seconds and print the DOM as it would render.
#[test]
#[ignore]
fn probe_tree() {
    let html = std::fs::read_to_string(std::env::var("PAGE").unwrap()).unwrap();
    let base = Url::parse(&std::env::var("BASE").unwrap_or("https://example.com/".into())).unwrap();
    let doc = html::parse_scripted(&html, base, true);
    let opts = js::Options { fetch_text: Arc::new(net::fetch_text_cached), dark: false, budget: Duration::from_secs(5), debug: true, session: 0 };
    let (mut doc, host) = js::start(doc, opts);
    if let Some(host) = &host {
        let until = std::time::Instant::now() + Duration::from_secs(6);
        while std::time::Instant::now() < until {
            if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
                doc = (*d).clone();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn show(doc: &page::Document, n: page::NodeId, depth: usize, out: &mut String) {
        if depth > 14 || out.len() > 12000 {
            return;
        }
        match doc.element(n) {
            Some(e) => {
                let attr = |k: &str| e.attr(k).map(|v| format!(" {k}={:?}", v.chars().take(40).collect::<String>())).unwrap_or_default();
                out.push_str(&format!("{}<{}{}{}>\n", "  ".repeat(depth), e.name.local, attr("id"), attr("class")));
                if &*e.name.local == "style" {
                    out.push_str(&format!("{}  [css {} bytes]\n", "  ".repeat(depth), doc.text_content(n).len()));
                    return;
                }
                for &c in doc.children(n) {
                    show(doc, c, depth + 1, out);
                }
            }
            None => {
                if let Some(t) = doc.node(n).text() {
                    let t = t.trim();
                    if !t.is_empty() {
                        out.push_str(&format!("{}\"{}\"\n", "  ".repeat(depth), t.chars().take(60).collect::<String>()));
                    }
                } else {
                    for &c in doc.children(n) {
                        show(doc, c, depth, out);
                    }
                }
            }
        }
    }
    let mut out = String::new();
    show(&doc, doc.body().unwrap(), 0, &mut out);
    println!("{out}");
}

#[test]
fn subtle_crypto() {
    let doc = run(r##"<body><p id="o"></p><script>
        (async () => {
          const hex = (b) => Array.from(new Uint8Array(b), (x) => x.toString(16).padStart(2, "0")).join("");
          const sha = hex(await crypto.subtle.digest("SHA-256", new TextEncoder().encode("abc")));
          const { privateKey, publicKey } = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
          const msg = new TextEncoder().encode("feather");
          const sig = await crypto.subtle.sign("Ed25519", privateKey, msg);
          const ok = await crypto.subtle.verify("Ed25519", publicKey, sig, msg);
          const bad = await crypto.subtle.verify("Ed25519", publicKey, sig, new TextEncoder().encode("other"));
          const raw = await crypto.subtle.exportKey("raw", publicKey);
          const key = await crypto.subtle.importKey("raw", new TextEncoder().encode("key"), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
          const mac = hex(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode("The quick brown fox jumps over the lazy dog")));
          const r = crypto.getRandomValues(new Uint8Array(8));
          document.getElementById("o").textContent = [sha, ok, bad, raw.byteLength, sig.byteLength, mac, r.length, crypto.randomUUID().length].join(" ");
        })();
    </script></body>"##);
    assert_eq!(
        text_of(&doc, "o"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad true false 32 64 f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8 8 36"
    );
}

#[test]
fn prelude_loads_completely() {
    // The last things the prelude defines exist (a prelude that throws
    // halfway leaves pages half a DOM).
    let doc = run(r##"<body><p id="o"></p><script>
        document.getElementById("o").textContent = [typeof __listeningIds, typeof __shadowStyles, typeof crypto.subtle.digest, typeof WebSocket, typeof fetch].join(" ");
    </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "function function function function function");
}

#[test]
fn storage_and_cookies() {
    let doc = run(r##"<body><p id="o"></p><script>
        localStorage.setItem("a", "1");
        localStorage.b = "2";
        sessionStorage.setItem("s", "x");
        const keys = Object.keys(localStorage).sort().join(",");
        localStorage.removeItem("a");
        document.cookie = "flavor=oatmeal; path=/";
        document.cookie = "secret=1; HttpOnly";
        document.getElementById("o").textContent = [keys, localStorage.length, localStorage.getItem("b"), sessionStorage.s, document.cookie.includes("flavor=oatmeal")].join(" ");
    </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "a,b 1 2 x true");
}
