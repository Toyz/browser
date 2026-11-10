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

fn find(doc: &page::Document, id: &str) -> page::NodeId {
    doc.descendants(doc.root())
        .find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some(id))
        .unwrap_or_else(|| panic!("no #{id}"))
}

/// The computed style of `#id`, as the page renders.
fn computed(doc: &page::Document, id: &str) -> style::ComputedStyle {
    let styler = style::StyleData::for_document(doc, &|_| None).styler(style::MediaEnv::default());
    let mut chain = Vec::new();
    let mut node = Some(find(doc, id));
    while let Some(n) = node {
        if doc.element(n).is_some() {
            chain.push(n);
        }
        node = doc.parent(n);
    }
    let mut style = style::ComputedStyle::initial();
    for &n in chain.iter().rev() {
        style = styler.compute(doc, n, &style);
    }
    style
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
        <p class="t" id="outside">outside</p><b id="red" style="color: red">r</b>
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
    // Its adopted sheet styles its own tree, its host (`:host`) and what's
    // slotted into it (`::slotted`); nothing outside.
    let red = computed(&doc, "red").color;
    assert_eq!(computed(&doc, "h").color, red);
    assert_ne!(computed(&doc, "outside").color, red);
    assert_eq!(computed(&doc, "c").display, style::Display::Block);
    assert_eq!(computed(&doc, "body").margin.top, style::Length::Px(0.0));
    assert_ne!(computed(&doc, "outside").margin.top, style::Length::Px(0.0));
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
        let wait = std::env::var("PROBE_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
        let until = std::time::Instant::now() + Duration::from_secs(wait);
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
    // PROBE_OUT=file.html: the flattened page as static HTML (to compare
    // renderings of exactly what the tab gets).
    if let Ok(path) = std::env::var("PROBE_OUT") {
        fn ser(doc: &page::Document, n: page::NodeId, out: &mut String) {
            match &doc.node(n).data {
                page::NodeData::Text(t) => {
                    let raw = doc.parent(n).and_then(|p| doc.element(p)).is_some_and(|e| matches!(&*e.name.local, "style" | "script"));
                    if raw { out.push_str(t) } else { out.push_str(&t.replace('&', "&amp;").replace('<', "&lt;")) }
                }
                page::NodeData::Element(e) => {
                    if &*e.name.local == "script" {
                        return;
                    }
                    out.push('<');
                    out.push_str(&e.name.local);
                    for a in &e.attrs {
                        out.push_str(&format!(" {}=\"{}\"", a.name.local, a.value.replace('&', "&amp;").replace('"', "&quot;")));
                    }
                    out.push('>');
                    for &c in doc.children(n) {
                        ser(doc, c, out);
                    }
                    out.push_str(&format!("</{}>", e.name.local));
                }
                _ => {
                    for &c in doc.children(n) {
                        ser(doc, c, out);
                    }
                }
            }
        }
        let mut html = String::from("<!doctype html>");
        ser(&doc, doc.root(), &mut html);
        std::fs::write(path, html).unwrap();
    }
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
        document.getElementById("o").textContent = [typeof __listeningIds, typeof __adoptedSheets, typeof crypto.subtle.digest, typeof WebSocket, typeof fetch].join(" ");
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

/// Real network: `cargo test -p js streaming -- --ignored`.
#[test]
#[ignore]
fn streaming_ndjson_and_event_source() {
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p><script>
        const out = [];
        const log = (s) => { out.push(s); document.getElementById("o").textContent = out.join(" | "); };
        (async () => {
          const res = await fetch("https://httpbin.org/stream/5");
          const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
          let buf = "", lines = 0;
          for (;;) {
            const { value, done } = await reader.read();
            if (done) break;
            buf += value;
            const parts = buf.split("\n");
            buf = parts.pop();
            for (const p of parts) if (p.trim()) { JSON.parse(p); lines++; }
          }
          log("ndjson " + lines);
          const text = await (await fetch("https://httpbin.org/stream/3")).text();
          log("text " + text.trim().split("\n").length);
          const es = new EventSource("https://stream.wikimedia.org/v2/stream/recentchange");
          let n = 0;
          es.onopen = () => log("open");
          es.onmessage = (e) => { if (++n === 2) { log("sse " + typeof e.data); es.close(); log("closed"); } };
        })();
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(10), debug: true, session: 0 };
    let (doc, host) = js::start(doc, opts);
    let mut text = text_of(&doc, "o");
    if let Some(host) = &host {
        let until = std::time::Instant::now() + Duration::from_secs(20);
        while !text.contains("closed") && std::time::Instant::now() < until {
            if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
                text = text_of(&d, "o");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    assert_eq!(text, "ndjson 5 | text 3 | open | sse string | closed");
}

#[test]
fn images_load_with_their_size() {
    // data: URLs load without the network.
    let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAYAAACddGYaAAAAEUlEQVR4nGNgYGD4z8DAwMAAAA0AAQ5/vWQAAAAASUVORK5CYII=";
    let doc = run(&format!(
        r##"<body><p id="o"></p><script>
        const out = [];
        const img = new Image();
        img.onload = () => {{ out.push("load " + img.naturalWidth + "x" + img.naturalHeight + " " + img.complete); document.getElementById("o").textContent = out.join(" | "); }};
        out.push("pending " + img.complete);
        img.src = "{png}";
        const bad = document.createElement("img");
        bad.addEventListener("error", () => {{ out.push("error"); document.getElementById("o").textContent = out.join(" | "); }});
        bad.src = "data:text/plain,nope";
        </script></body>"##
    ));
    let text = text_of(&doc, "o");
    assert!(text.contains("pending true"), "{text}");
    assert!(text.contains("load 3x2 true"), "{text}");
    assert!(text.contains("error"), "{text}");
}

#[test]
fn slotted_clicks_reach_shadow_listeners_and_push_state() {
    let doc = html::parse_scripted(
        r##"<body><x-link><span id="label">Go</span></x-link><p id="o"></p><script>
        class XLink extends HTMLElement {
          connectedCallback() {
            const root = this.attachShadow({ mode: "open" });
            root.innerHTML = '<a href="/next"><slot></slot></a>';
            root.addEventListener("click", (e) => {
              e.preventDefault();
              history.pushState({ page: 2 }, "", "/next?x=1");
              document.getElementById("o").textContent =
                location.pathname + location.search + " " + history.length + " " + history.state.page + " " + e.target.id;
            });
          }
        }
        customElements.define("x-link", XLink);
        window.addEventListener("popstate", (e) => { document.getElementById("o").textContent = "pop " + location.pathname + " " + e.state; });
        </script></body>"##,
        Url::parse("https://example.com/start").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: true, session: 0 };
    let label = doc
        .descendants(doc.root())
        .find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some("label"))
        .unwrap();
    let (_, host) = js::start(doc, opts);
    let host = host.unwrap();
    assert!(host.click(label, Duration::from_secs(5)), "the shadow root's listener prevented the default");
    let mut changed = None;
    let mut latest = None;
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < until && (changed.is_none() || latest.is_none()) {
        match host.updates.try_recv() {
            Ok(js::FromJs::UrlChanged { url, replace }) => changed = Some((url.to_string(), replace)),
            Ok(js::FromJs::Document(d)) => latest = Some(d),
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    assert_eq!(changed, Some(("https://example.com/next?x=1".to_owned(), false)));
    // Wait for the snapshot with the handler's output.
    let mut text = latest.map(|d| text_of(&d, "o")).unwrap_or_default();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !text.starts_with("/next") && std::time::Instant::now() < until {
        if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
            text = text_of(&d, "o");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(text, "/next?x=1 2 2 label");
    host.send(js::ToJs::Traverse { delta: -1 });
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !text.starts_with("pop") && std::time::Instant::now() < until {
        if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
            text = text_of(&d, "o");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(text, "pop /start null");
}

#[test]
fn declarative_shadow_roots_render_and_reach_scripts() {
    let doc = run(r##"<body><div id="h"><template shadowrootmode="open"><style>p { color: red }</style><p id="in">shadow <slot></slot></p></template><span>light</span></div>
        <p id="o"></p><b id="red" style="color: red">r</b><script>
        const h = document.getElementById("h");
        document.getElementById("o").textContent = (h.shadowRoot ? "root " + h.shadowRoot.querySelector("#in").textContent.trim() : "none") + " / " + h.querySelectorAll("template").length;
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "root shadow / 0");
    // Rendered: the shadow tree with the light span slotted in.
    let rendered = text_of(&doc, "h").split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(rendered, "p { color: red }shadow light");
    // Its <style> styles the shadow tree's <p>, not the document's.
    let red = computed(&doc, "red").color;
    assert_eq!(computed(&doc, "in").color, red);
    assert_ne!(computed(&doc, "o").color, red);
}

#[test]
fn shadow_styles_stay_in_their_own_tree() {
    // An outer component's `.t` rule must not style the `.t` inside a
    // component nested in it (badgedirectory's dropdowns got another
    // component's `width: 48px`).
    let doc = run(r##"<body><div id="outer"></div><p id="o"></p><script>
        const outer = document.getElementById("outer").attachShadow({ mode: "open" });
        outer.innerHTML = '<style>.t { color: red }</style><span class="t" id="mine">a</span><div id="inner-host"></div>';
        const inner = outer.getElementById("inner-host").attachShadow({ mode: "open" });
        inner.innerHTML = '<style>.t { font-weight: bold }</style><span class="t" id="theirs">b</span>';
        </script></body>"##);
    let (mine, theirs) = (computed(&doc, "mine"), computed(&doc, "theirs"));
    assert_ne!(mine.color, theirs.color);
    assert!(mine.font_weight < 700, "{}", mine.font_weight);
    assert!(theirs.font_weight >= 700);
}

#[test]
fn shadow_selectors_follow_tree_boundaries() {
    let doc = run(r##"<body><div class="wrap"><div id="host" class="on"><p id="light" class="x">l</p></div></div>
        <span class="in" id="doc-span">d</span><b id="red" style="color: red">r</b><script>
        const root = document.getElementById("host").attachShadow({ mode: "open" });
        root.innerHTML = `<style>
            :host(.on) { font-weight: 700 }
            :host(.off) { color: red }
            .wrap .in { color: red }
            :host .in { margin-top: 7px }
            div > .in { margin-left: 3px }
            ::slotted(.x) { color: red }
            .x { margin-top: 9px }
          </style><div><span class="in" id="in">s</span></div><slot></slot>`;
        </script></body>"##);
    let red = computed(&doc, "red").color;
    let host = computed(&doc, "host");
    assert_eq!(host.font_weight, 700);
    assert_ne!(host.color, red);
    let inside = computed(&doc, "in");
    // `.wrap` is outside the tree: descendant selectors stop at the root.
    assert_ne!(inside.color, red);
    assert_eq!(inside.margin.top, style::Length::Px(7.0));
    assert_eq!(inside.margin.left, style::Length::Px(3.0));
    // The document's `.in` isn't the tree's.
    assert_eq!(computed(&doc, "doc-span").margin.top, style::Length::Px(0.0));
    // Slotted: `::slotted(.x)` reaches it, the tree's plain `.x` doesn't.
    let light = computed(&doc, "light");
    assert_eq!(light.color, red);
    assert_ne!(light.margin.top, style::Length::Px(9.0));
}

#[test]
fn cssom_rules_render() {
    let doc = run(r##"<head><style id="s"></style></head><body><p id="o"></p><script>
        const sheet = document.getElementById("s").sheet;
        const parsed = new CSSStyleSheet();
        parsed.replaceSync("/* lead */ :host { a: b } /* x */ nav { c: d } .t { e: f }");
        sheet.insertRule(".a { color: red }", 0);
        sheet.insertRule("@media (min-width: 1px) { .b { color: blue } }", sheet.cssRules.length);
        sheet.insertRule(".c { color: green }", 1);
        sheet.deleteRule(1);
        document.getElementById("o").textContent = sheet.cssRules.length + " " + sheet.cssRules[0].selectorText + " " + document.styleSheets.length + " " + parsed.cssRules.map((r) => r.selectorText).join(",");
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "2 .a 1 :host,nav,.t");
    let css = text_of(&doc, "s");
    assert!(css.contains(".a { color: red }") && css.contains("@media (min-width: 1px) { .b { color: blue } }") && !css.contains(".c"), "{css}");
}

#[test]
fn relative_time_and_date_constructor() {
    let doc = run(r##"<body><p id="o"></p><script>
        const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });
        const d = new Date(0);
        document.getElementById("o").textContent = [rtf.format(-1, "day"), rtf.format(3, "weeks"), new Intl.RelativeTimeFormat("en").format(-2, "year"), String(d.constructor === Date), String(new d.constructor(5).getTime())].join(" | ");
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "yesterday | in 3 weeks | 2 years ago | true | 5");
}

#[test]
fn canvas_drawing_reaches_the_snapshot() {
    let doc = run(r##"<body><canvas id="c" width="40" height="20"></canvas><p id="o"></p><script>
        const c = document.getElementById("c"), ctx = c.getContext("2d");
        const g = ctx.createLinearGradient(0, 0, 40, 0);
        g.addColorStop(0, "red"); g.addColorStop(1, "blue");
        ctx.fillStyle = g;
        ctx.fillRect(0, 0, 40, 20);
        ctx.beginPath(); ctx.arc(20, 10, 5, 0, Math.PI * 2); ctx.fillStyle = "#0f0"; ctx.fill();
        document.getElementById("o").textContent = c.toDataURL().slice(0, 30);
        </script></body>"##);
    // The canvas shows its pixels under a stable address (they go to the
    // tab directly, not through the DOM)...
    let c = doc.descendants(doc.root()).find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some("c")).unwrap();
    assert_eq!(doc.element(c).unwrap().attr("src"), Some(js::canvas_url(c).as_str()));
    // ...and `toDataURL()` encodes what was drawn.
    assert_eq!(text_of(&doc, "o"), "data:image/png;base64,iVBORw0K");
}

#[test]
fn layout_reaches_scripts() {
    let doc = html::parse_scripted(
        r##"<body><div id="box"></div><p id="o"></p><script>
        const box = document.getElementById("box");
        const seen = [];
        new ResizeObserver((entries) => {
          for (const e of entries) seen.push(e.contentRect.width + "x" + e.contentRect.height);
          const r = box.getBoundingClientRect();
          document.getElementById("o").textContent = seen.join(",") + " | " + r.left + "," + r.top + " " + box.offsetWidth + " " + innerWidth;
        }).observe(box);
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: true, session: 0 };
    let b = doc.descendants(doc.root()).find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some("box")).unwrap();
    let (_, host) = js::start(doc, opts);
    let host = host.unwrap();
    // The script asked about the box.
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !host.measured.read().unwrap().contains(&b) && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(host.measured.read().unwrap().contains(&b));
    host.send(js::ToJs::Layout { rects: vec![(b, [10.0, 20.0, 300.0, 150.0])], gone: Vec::new(), viewport: (1024.0, 700.0), scroll: (0.0, 0.0) });
    let mut text = String::new();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !text.contains('|') && std::time::Instant::now() < until {
        if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
            text = text_of(&d, "o");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Laid out after observe(): reported as the initial size, then again as a
    // change from the 0x0 the script saw (pages often skip the first callback).
    std::thread::sleep(Duration::from_millis(100));
    while let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
        text = text_of(&d, "o");
    }
    assert_eq!(text, "300x150,300x150 | 10,20 300 1024");
}

#[test]
fn geometry_before_layout_is_estimated_natively() {
    let doc = html::parse_scripted(
        r##"<body><div id="d"><span id="s">x</span></div><div style="display:flex"><div id="f"></div></div><p id="o"></p><script>
        const d = document.getElementById("d"), s = document.getElementById("s"), f = document.getElementById("f");
        const r = d.getBoundingClientRect();
        document.getElementById("o").textContent = [
          r instanceof DOMRect, r.width, r.right, d.offsetWidth, d.clientHeight, s.offsetWidth, f.offsetWidth,
          document.documentElement.clientWidth, document.documentElement.clientHeight, document.body.offsetWidth,
          getComputedStyle(d).width, innerWidth, innerHeight, d.getClientRects().length, typeof Element.prototype.getBoundingClientRect,
        ].join(" ");
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: false, session: 0 };
    let (doc, _host) = js::start(doc, opts);
    assert_eq!(text_of(&doc, "o"), "true 1280 1280 1280 0 0 0 1280 800 1280 1280px 1280 800 0 function");
}

#[test]
fn observers_are_native_classes() {
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p><script>
        const io = new IntersectionObserver(() => {}, { rootMargin: "10px 20%", threshold: [0, 0.5] });
        let threw = false;
        try { new IntersectionObserver(); } catch (e) { threw = e instanceof TypeError; }
        document.getElementById("o").textContent = [
          "isIntersecting" in IntersectionObserverEntry.prototype, io instanceof IntersectionObserver, io.rootMargin, io.thresholds.join("+"), io.root,
          typeof io.observe, io.takeRecords().length, threw, typeof ResizeObserver, typeof ResizeObserverEntry, typeof PerformanceObserver.supportedEntryTypes,
          new IntersectionObserverEntry({ isIntersecting: true }).isIntersecting,
        ].join(" ");
        io.observe(document.body); io.unobserve(document.body); io.disconnect();
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: false, session: 0 };
    let (doc, _host) = js::start(doc, opts);
    assert_eq!(text_of(&doc, "o"), "true true 10px 20% 10px 20% 0+0.5  function 0 true function function object true");
}

#[test]
fn canvas_context_is_native_with_state_and_pixels() {
    let doc = html::parse_scripted(
        r##"<body><canvas id="c" width="10" height="10"></canvas><p id="o"></p><script>
        const c = document.getElementById("c"), ctx = c.getContext("2d");
        const out = [ctx instanceof CanvasRenderingContext2D, c.getContext("2d") === ctx, ctx.canvas === c, ctx.fillStyle, ctx.lineWidth];
        ctx.lineWidth = -3; ctx.globalAlpha = 2; ctx.fillStyle = "nonsense"; ctx.lineCap = "round";
        out.push(ctx.lineWidth, ctx.globalAlpha, ctx.fillStyle, ctx.lineCap);
        ctx.save(); ctx.fillStyle = "red"; ctx.translate(2, 3); out.push(ctx.getTransform().e); ctx.restore();
        out.push(ctx.fillStyle, ctx.getTransform().isIdentity);
        ctx.fillStyle = "#00ff00"; ctx.fillRect(0, 0, 5, 5);
        const p = new Path2D(); p.rect(5, 5, 5, 5); ctx.fillStyle = "blue"; ctx.fill(p);
        const px = (x, y) => Array.from(ctx.getImageData(x, y, 1, 1).data).join(",");
        out.push(px(1, 1), px(7, 7), px(7, 1));
        const img = ctx.createImageData(1, 1); img.data.set([255, 0, 0, 255]); ctx.putImageData(img, 7, 1);
        out.push(px(7, 1));
        const g = ctx.createLinearGradient(0, 0, 10, 0); g.addColorStop(0, "red"); g.addColorStop(1, "blue"); ctx.fillStyle = g;
        out.push(ctx.fillStyle === g, g instanceof CanvasGradient);
        c.width = 10; out.push(px(1, 1), ctx.fillStyle);
        ctx.setLineDash([2, 3]); out.push(ctx.getLineDash().join("+"), c.toDataURL().slice(0, 15));
        document.getElementById("o").textContent = out.join(" ");
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: false, session: 0 };
    let (doc, _host) = js::start(doc, opts);
    assert_eq!(
        text_of(&doc, "o"),
        "true true true #000000 1 1 1 #000000 round 2 #000000 true 0,255,0,255 0,0,255,255 0,0,0,0 255,0,0,255 true true 0,0,0,0 #000000 2+3 data:image/png;"
    );
}

#[test]
fn mouse_events_reach_scripts() {
    let doc = html::parse_scripted(
        r##"<body><canvas id="c"></canvas><p id="o"></p><script>
        const c = document.getElementById("c");
        c.getBoundingClientRect();
        c.addEventListener("mousedown", (e) => { document.getElementById("o").textContent = e.type + " " + e.clientX + "," + e.clientY + " " + e.offsetX + "," + e.offsetY + " " + e.buttons; });
        window.addEventListener("wheel", (e) => { document.getElementById("o").textContent += " wheel " + e.deltaY; });
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: true, session: 0 };
    let c = doc.descendants(doc.root()).find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some("c")).unwrap();
    let root = doc.root();
    let (_, host) = js::start(doc, opts);
    let host = host.unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !host.hears_mouse(&[c]) && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(host.hears_mouse(&[c]) && host.hears_mouse(&[root]));
    host.send(js::ToJs::Layout { rects: vec![(c, [10.0, 20.0, 300.0, 150.0])], gone: Vec::new(), viewport: (1000.0, 700.0), scroll: (0.0, 0.0) });
    host.send(js::ToJs::Mouse { kind: "mousedown", node: c, x: 50.0, y: 60.0, button: 0, buttons: 1, delta: (0.0, 0.0) });
    host.send(js::ToJs::Mouse { kind: "wheel", node: c, x: 50.0, y: 60.0, button: 0, buttons: 0, delta: (0.0, 120.0) });
    let mut text = String::new();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !text.contains("wheel") && std::time::Instant::now() < until {
        if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
            text = text_of(&d, "o");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(text, "mousedown 50,60 40,40 1 wheel 120");
}

#[test]
fn keys_reach_scripts() {
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p><script>
        document.addEventListener("keydown", (e) => { document.getElementById("o").textContent = e.key + " " + e.code + " " + e.keyCode + " " + e.ctrlKey + " " + (e.target === document.body); });
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: true, session: 0 };
    let (_, host) = js::start(doc, opts);
    let host = host.unwrap();
    host.send(js::ToJs::Key { kind: "keydown", key: "Enter".into(), code: "Enter".into(), target: None, modifiers: [true, false, false, false] });
    let mut text = String::new();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while text.is_empty() && std::time::Instant::now() < until {
        if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
            text = text_of(&d, "o");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(text, "Enter Enter 13 true true");
}

#[test]
fn tree_walker_and_create_event() {
    let doc = run(r##"<body><div id="r"><p>a<b>b</b></p><!--c--><i>d</i></div><p id="o"></p><script>
        const w = document.createTreeWalker(document.getElementById("r"), NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
        const seen = [];
        while (w.nextNode()) seen.push(w.currentNode.nodeType === 3 ? w.currentNode.data : w.currentNode.localName);
        const ev = document.createEvent("CustomEvent");
        ev.initCustomEvent("x", true, false, 42);
        let got = 0;
        document.body.addEventListener("x", (e) => { got = e.detail; });
        document.getElementById("r").dispatchEvent(ev);
        document.getElementById("o").textContent = seen.join(",") + " " + got;
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "p,a,b,b,i,d 42");
}

#[test]
fn platform_extras() {
    let doc = run(r##"<body><div id="d"></div><p id="o"></p><script>
        const out = [];
        out.push(document.getElementById("d") instanceof HTMLDivElement);
        const c = structuredClone({ a: [1, { b: new Date(5) }], m: new Map([[1, 2]]) });
        out.push(c.a[1].b.getTime(), c.m.get(1));
        out.push(new DOMMatrix([2, 0, 0, 2, 10, 0]).transformPoint({ x: 1, y: 1 }).x);
        const ch = new MessageChannel();
        ch.port2.onmessage = (e) => { out.push("msg " + e.data); document.getElementById("o").textContent = out.join(" "); };
        ch.port1.postMessage("hi");
        out.push(CSS.escape("a.b"), new XMLSerializer().serializeToString(document.getElementById("d")));
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), r#"true 5 2 12 a\.b <div id="d"></div> msg hi"#);
}

#[test]
fn document_all_is_falsy_but_not_undefined() {
    let doc = run(r##"<body><div id="d"></div><p id="o"></p><script>
        let u;
        const all = document.all;
        const out = [!!all, typeof all, all == undefined, u === all, all.length > 2, all.namedItem("d").id];
        document.getElementById("o").textContent = out.join(" ");
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "false undefined true false true d");
}

#[test]
fn intersection_observer_follows_layout() {
    let doc = html::parse_scripted(
        r##"<body><div id="a"></div><div id="b"></div><div id="c"></div><p id="o"></p><script>
        const seen = {};
        const show = () => { document.getElementById("o").textContent = ["a", "b", "c"].map((k) => k + "=" + seen[k]).join(" ") + " |"; };
        const io = new IntersectionObserver((entries) => {
          for (const e of entries) seen[e.target.id] = e.isIntersecting + (e.isIntersecting ? ":" + (e.intersectionRect.height > 0) : "");
          show();
        }, { rootMargin: "0px 0px 100px 0px" });
        for (const id of ["a", "b", "c"]) io.observe(document.getElementById(id));
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: true, session: 0 };
    let id = |doc: &page::Document, name: &str| doc.descendants(doc.root()).find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some(name)).unwrap();
    let (a, b, c) = (id(&doc, "a"), id(&doc, "b"), id(&doc, "c"));
    let (_, host) = js::start(doc, opts);
    let host = host.unwrap();
    // `a` is in view, `b` far below it (even with the margin), `c` not
    // painted (the tab says so: its first entry comes at once, not after a
    // timer guessing it).
    host.send(js::ToJs::Layout { rects: vec![(a, [0.0, 10.0, 300.0, 50.0]), (b, [0.0, 2000.0, 300.0, 50.0])], gone: vec![c], viewport: (1024.0, 700.0), scroll: (0.0, 0.0) });
    let mut text = String::new();
    let until = std::time::Instant::now() + Duration::from_secs(1);
    while !text.contains("c=false") && std::time::Instant::now() < until {
        if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
            text = text_of(&d, "o");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(text, "a=true:true b=false c=false |");
}

#[test]
fn mutation_observers() {
    let doc = run(r##"<body><div id="d" class="a"><p>x</p></div><p id="o"></p><script>
        const out = [];
        const d = document.getElementById("d");
        new MutationObserver((records) => {
            for (const r of records) out.push(r.type + ":" + (r.attributeName || r.addedNodes.length + "/" + r.removedNodes.length) + (r.oldValue !== null ? "=" + r.oldValue : ""));
            document.getElementById("o").textContent = out.join(" ");
        }).observe(d, { childList: true, attributes: true, attributeOldValue: true, subtree: true, characterData: true });
        d.className = "b";
        d.appendChild(document.createElement("span"));
        d.firstChild.remove();
        d.querySelector("span").setAttribute("x", "1");
        // Polymer's microtask trick: a text node's data changing.
        const t = document.createTextNode("");
        let ticks = 0;
        new MutationObserver(() => { ticks++; out.push("tick" + ticks); document.getElementById("o").textContent = out.join(" "); }).observe(t, { characterData: true });
        t.textContent = "1";
        out.push("sync");
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "sync attributes:class=a childList:1/0 childList:0/1 attributes:x tick1");
}

#[test]
fn nested_template_contents_survive_copies() {
    let doc = run(r##"<body><p id="o"></p><script>
        const t = document.createElement("template");
        t.innerHTML = '<div><template id="inner"><span>[[x]]</span></template></div>';
        const inner = t.content.querySelector("#inner");
        const copy = document.importNode(t.content, true).querySelector("#inner");
        const out = [inner.content.firstChild.localName, copy.content.firstChild.localName, copy.content.textContent];
        const s = document.createElement("template");
        s.content.appendChild(document.createElement("b"));
        out.push(s.cloneNode(true).content.firstChild.localName, s.cloneNode(false).content.childNodes.length);
        document.getElementById("o").textContent = out.join(" ");
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "span span [[x]] b 0");
}

#[test]
fn private_getters_from_arrows() {
    let doc = run(r##"<body><p id="o"></p><script>
        const out = [];
        try {
          class O {
            #h; #f;
            get #g() { return this.#f ? this.#f() : this.#h; }
            constructor(e) { this.#h = e; this.q = this.#v([1, 2]); }
            #v(list) { return list.map((n) => { const o = () => this.#g + n; return { o, ...{ k: n } }; }); }
          }
          const x = new O("a");
          out.push(x.q[0].o(), x.q[1].o());
          class P extends O { constructor() { super("b"); } }
          out.push(new P().q[0].o());
        } catch (e) { out.push("ERR " + e); }
        document.getElementById("o").textContent = out.join(" ");
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "a1 a2 b1");
}

#[test]
fn text_decoder_replaces_malformed_bytes() {
    let doc = run(r##"<body><p id="o"></p><script>
        const d = new TextDecoder();
        const s = d.decode(new Uint8Array([0x1f, 0x8b, 0xff, 0xf8, 0x41, 0xe2, 0x82, 0xac, 0xc3]));
        document.getElementById("o").textContent = Array.from(s).map((c) => c.codePointAt(0).toString(16)).join(" ");
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "1f fffd fffd fffd 41 20ac fffd");
}

#[test]
fn cloned_responses_read_the_body() {
    let doc = run(r##"<body><p id="o"></p><script>
        let resolve;
        const r = new Response("");
        r._full = new Promise((res) => { resolve = res; });
        const c = r.clone();
        Promise.all([r.text(), c.text()]).then(([a, b]) => { document.getElementById("o").textContent = a + "|" + b; });
        resolve("body");
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "body|body");
}

#[test]
fn offset_parent_chains_end() {
    let doc = run(r##"<body><div><span id="s">x</span></div><p id="o"></p><script>
        let n = 0;
        for (let e = document.getElementById("s"); e && n < 10; e = e.offsetParent) n++;
        const d = document.createElement("div");
        d.innerHTML = '<a style="top:1px;float:left;opacity:.55">a</a>';
        document.getElementById("o").textContent = n + " " + document.body.offsetParent + " " + document.createElement("i").offsetParent + " " + document.ownerDocument + " " + d.firstChild.style.opacity;
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "2 null null null 0.55");
}

#[test]
fn template_contents_are_inert() {
    let doc = run(r##"<body><p id="o"></p><script>
        let made = 0;
        customElements.define("x-rep", class extends HTMLElement { constructor() { super(); made++; } });
        const t = document.createElement("template");
        t.innerHTML = '<div><x-rep items="[[list]]"></x-rep></div>';
        const inside = t.content.querySelector("x-rep");
        inside.removeAttribute("items");
        // Made by the content's own document (as Polymer does for <template is>): not upgraded either.
        t.content.firstChild.appendChild(t.content.ownerDocument.createElement("x-rep"));
        const before = made;
        const copy = document.importNode(t.content, true);
        const stamped = copy.querySelector("x-rep");
        document.body.appendChild(copy);
        document.getElementById("o").textContent = [before, made, stamped.hasAttribute("items"), stamped instanceof HTMLElement].join(" ");
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "0 2 false true");
}

#[test]
fn template_content_persists_and_attributes_are_seen() {
    let doc = run(r##"<body><p id="o"></p><script>
        const t = document.createElement("template");
        t.innerHTML = '<div id="a" class="x">hi</div>';
        const same = t.content === t.content;
        const div = t.content.firstChild;
        const out = [same, div.hasAttributes(), document.createElement("b").hasAttributes(), typeof performance.timing.responseStart, document.importNode(t.content, true).firstChild.id];
        document.getElementById("o").textContent = out.join(" ");
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "true true false number a");
}

#[test]
fn bare_global_event_methods() {
    let doc = run(r##"<body><p id="o"></p><script>
        "use strict";
        let hit = 0;
        const f = () => { hit++; };
        addEventListener("ping", f, true);
        window.dispatchEvent(new Event("ping"));
        dispatchEvent(new Event("ping"));
        removeEventListener("ping", f, true);
        window.dispatchEvent(new Event("ping"));
        document.getElementById("o").textContent = String(hit);
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "2");
}

#[test]
fn svg_elements_have_style() {
    let doc = run(r##"<body><svg id="s"><path id="p"></path></svg><p id="o"></p><script>
        const p = document.getElementById("p");
        p.style.fill = "red";
        document.getElementById("o").textContent = [p instanceof SVGElement, p instanceof HTMLElement, typeof p.dataset, p.getAttribute("style")].join(" ");
        </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "true false object fill: red");
}

#[test]
fn focus_is_shared_with_the_tab() {
    let doc = html::parse_scripted(
        r##"<body><input id="a"><input id="b"><p id="o"></p><script>
        const log = [];
        const say = () => { document.getElementById("o").textContent = log.join(" ") + " |"; };
        for (const id of ["a", "b"]) {
          const el = document.getElementById(id);
          el.addEventListener("focus", () => { log.push("focus:" + id); say(); });
          el.addEventListener("blur", () => { log.push("blur:" + id); say(); });
        }
        // The tab focusing `a` reaches scripts as events; scripts moving
        // focus to `b` and then away reaches the tab.
        document.getElementById("a").addEventListener("focus", () => {
          setTimeout(() => { document.getElementById("b").focus(); document.getElementById("b").blur(); }, 0);
        });
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: true, session: 0 };
    let id = |doc: &page::Document, name: &str| doc.descendants(doc.root()).find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some(name)).unwrap();
    let (a, b) = (id(&doc, "a"), id(&doc, "b"));
    let (_, host) = js::start(doc, opts);
    let host = host.unwrap();
    host.send(js::ToJs::Focus { node: Some(a) });
    let mut text = String::new();
    let mut focus_moves = Vec::new();
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while !text.contains("blur:b") && std::time::Instant::now() < until {
        match host.updates.try_recv() {
            Ok(js::FromJs::Document(d)) => text = text_of(&d, "o"),
            Ok(js::FromJs::Focus(n)) => focus_moves.push(n),
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    assert_eq!(text, "focus:a blur:a focus:b blur:b |");
    // The tab hears where scripts left focus (nowhere), not the tab's own move.
    assert_eq!(focus_moves.last(), Some(&None));
    assert!(!focus_moves.contains(&Some(a)));
    let _ = b;
}

/// Computed styles on a live page: `PAGE=file.html BASE=url PROBE_MATCH=tag-or-.class
/// cargo test --release -p js --test dom probe_styles -- --ignored --nocapture`.
#[test]
#[ignore]
fn probe_styles() {
    let html = std::fs::read_to_string(std::env::var("PAGE").unwrap()).unwrap();
    let base = Url::parse(&std::env::var("BASE").unwrap_or("https://example.com/".into())).unwrap();
    let want = std::env::var("PROBE_MATCH").unwrap_or_default();
    let doc = html::parse_scripted(&html, base, true);
    let opts = js::Options { fetch_text: Arc::new(net::fetch_text_cached), dark: false, budget: Duration::from_secs(5), debug: false, session: 0 };
    let (mut doc, host) = js::start(doc, opts);
    if let Some(host) = &host {
        let until = std::time::Instant::now() + Duration::from_secs(8);
        while std::time::Instant::now() < until {
            if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
                doc = (*d).clone();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let styler = style::StyleData::for_document(&doc, &|u| net::fetch_text_cached(u)).styler(style::MediaEnv { width: 1280.0, height: 900.0, scripting: true, ..Default::default() });
    let depth: usize = std::env::var("PROBE_DEPTH").ok().and_then(|d| d.parse().ok()).unwrap_or(0);
    fn walk(doc: &page::Document, styler: &style::Styler, n: page::NodeId, parent: &style::ComputedStyle, want: &str, path: &str, depth: usize, level: usize) {
        let Some(e) = doc.element(n) else { return };
        let s = styler.compute(doc, n, parent);
        let class = e.attr("class").unwrap_or("");
        let path = format!("{path} > {}{}", e.name.local, class.split_whitespace().next().map(|c| format!(".{c}")).unwrap_or_default());
        if level <= depth && depth > 0 {
            println!("{}{}.{} display={:?} vis={} opacity={} h={:?}", "  ".repeat(level), e.name.local, class.split_whitespace().next().unwrap_or(""), s.display, s.visible, s.opacity, s.height);
        }
        if &*e.name.local == want || class.split_whitespace().any(|c| want.strip_prefix('.') == Some(c)) {
            println!("{path}\n   tree={:?} display={:?} width={:?} max_width={:?} margin={:?} padding={:?} bg={:?} color={:?} vis={} pos={:?} z={:?} op={} grad={} inset={:?} h={:?} ov={:?} font={:?} weight={}", doc.tree_of(n), s.display, s.width, s.max_width, s.margin, s.padding, s.background, s.color, s.visible, s.position, s.z_index, s.opacity, s.gradients.is_some(), s.inset, s.height, s.overflow, s.font_names, s.font_weight);
        }
        for &c in doc.children(n) {
            walk(doc, styler, c, &s, want, &path, depth, level + 1);
        }
    }
    let html_el = doc.children(doc.root()).iter().copied().find(|&n| doc.element(n).is_some()).unwrap();
    walk(&doc, &styler, html_el, &style::ComputedStyle::initial(), &want, "", depth, 0);
}

#[test]
fn inline_styles_with_semicolons_in_values() {
    // `;` inside a string or url() is part of the value: scripts editing
    // another property keep it whole.
    let doc = run(r##"<body><div id="d" style='background: url("data:image/svg+xml;utf8,<svg/>") no-repeat; content: ";"'></div><p id="o"></p><script>
        const d = document.getElementById("d");
        d.style.color = "red";
        d.style.setProperty("width", "10px", "important");
        document.getElementById("o").textContent = [d.style.background, d.style.content, d.style.color, d.style.length, d.style.getPropertyValue("width")].join(" | ");
    </script></body>"##);
    assert_eq!(text_of(&doc, "o"), r#"url("data:image/svg+xml;utf8,<svg/>") no-repeat | ";" | red | 4 | 10px"#);
}

#[test]
fn media_elements_have_their_api() {
    let doc = run(r##"<body><video id="v" src="a.mp4" muted></video><p id="o"></p><script>
        const v = document.getElementById("v");
        const added = [];
        v.textTracks.addEventListener("addtrack", (e) => added.push(e.track.kind));
        v.addTextTrack("captions", "English", "en");
        const out = [v instanceof HTMLVideoElement, v instanceof HTMLMediaElement, v.textTracks.length, v.textTracks[0].kind, added.join(),
          v.paused, v.readyState, isNaN(v.duration), v.muted, v.buffered.length, v.videoWidth];
        v.play().catch((e) => { out.push(e.name); document.getElementById("o").textContent = out.join(" "); });
    </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "true true 1 captions captions true 0 true true 0 0 NotSupportedError");
}

#[test]
fn script_cookies_with_domain_and_equals_in_value() {
    // YouTube saves its preferences like this (the theme among them).
    let doc = run(r##"<body><p id="o"></p><script>
        document.cookie = "PREF=f6=40000000&tz=America.Chicago; expires=" + new Date(Date.now() + 864e5).toUTCString() + "; path=/; domain=.example.com";
        document.getElementById("o").textContent = document.cookie;
    </script></body>"##);
    assert!(text_of(&doc, "o").contains("PREF=f6=40000000&tz=America.Chicago"), "{}", text_of(&doc, "o"));
}

#[test]
fn match_media_evaluates_real_queries() {
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p><script>
        const q = (s) => matchMedia(s).matches ? 1 : 0;
        const wide = matchMedia("(min-width: 1000px)");
        const seen = [];
        wide.addEventListener("change", (e) => { seen.push(e.matches); show(); });
        const show = () => document.getElementById("o").textContent = [
          q("(min-width: 1000px)"), q("(display-mode: browser)"), q("(hover: hover)"), q("(orientation: landscape)"),
          q("(max-width: 600px), (min-width: 1200px)"), q("screen and (min-width: 600px) and (max-width: 1300px)"),
          q("(prefers-color-scheme: dark)"), q("print"), "|", seen.join()].join(" ");
        show();
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: true, budget: Duration::from_secs(5), debug: true, session: 0 };
    let (doc, host) = js::start(doc, opts);
    assert_eq!(text_of(&doc, "o"), "1 1 1 1 1 1 1 0 | ");
    // Narrower: the list's answer flips and it says so.
    let host = host.unwrap();
    host.send(js::ToJs::Layout { rects: Vec::new(), gone: Vec::new(), viewport: (800.0, 700.0), scroll: (0.0, 0.0) });
    let mut text = String::new();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !text.ends_with("false") && std::time::Instant::now() < until {
        if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
            text = text_of(&d, "o");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(text, "0 1 1 1 0 1 1 0 | false");
}

#[test]
fn css_supports_answers_for_this_engine() {
    let doc = run(r##"<body><p id="o"></p><script>
        document.getElementById("o").textContent = [CSS.supports("display", "grid"), CSS.supports("display", "bogus"),
          CSS.supports("-webkit-touch-callout", "none"), CSS.supports("(gap: 1px) and (display: flex)"), CSS.supports("display: flex"),
          CSS.supports("selector(:has(a))")].join(" ");
    </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "true false false true true true");
}

#[test]
fn unlaid_out_blocks_are_as_wide_as_their_container() {
    // Before any layout: a block fills the viewport, inline boxes are 0.
    let doc = run(r##"<body><div id="d"><p id="p">x</p><span id="s">y</span></div><p id="o"></p><script>
        const w = (id) => document.getElementById(id).offsetWidth;
        document.getElementById("o").textContent = [w("d") > 0, w("p") === w("d"), w("s"), document.getElementById("p").getBoundingClientRect().width > 0].join(" ");
    </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "true true 0 true");
}

#[test]
fn get_computed_style_reads_the_cascade() {
    let doc = run(r##"<head><style>
        .box { display: flex; color: rgb(255, 0, 0); padding: 0 10px; --gap: 4px }
        .box.on { display: none }
        span { font-weight: 700 }
    </style></head><body><div class="box" id="b"><span id="s">x</span></div><p id="o"></p><script>
        const b = document.getElementById("b"), cs = getComputedStyle(b);
        const out = [cs.display, cs.color, cs.paddingLeft, cs.getPropertyValue("--gap").trim(), getComputedStyle(document.getElementById("s")).fontWeight, parseFloat(cs.width) > 0];
        b.classList.add("on");
        out.push(getComputedStyle(b).display);
        document.getElementById("o").textContent = out.join(" ");
    </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "flex rgb(255, 0, 0) 10px 4px 700 true none");
}

#[test]
fn url_follows_the_standard() {
    let doc = run(r##"<body><p id="o"></p><script>
        const out = [];
        const u = new URL("../x/./y?a=1&b=2#h", "HTTPS://Example.COM:443/p/q/r");
        out.push(u.href, u.origin, u.host, u.pathname);
        u.searchParams.append("c", "3 4");
        out.push(u.search);
        u.search = "?z=9";
        out.push(u.searchParams.get("z"), u.searchParams.size);
        u.hostname = "other.test"; u.port = "8080"; u.hash = "";
        out.push(u.href);
        out.push(URL.canParse("nope"), URL.parse("nope"), String(new URLSearchParams({ a: "1", b: "x y" })));
        out.push([...new URLSearchParams("q=1&q=2&r")].map((p) => p.join("=")).join(","));
        try { new URL("not a url"); } catch (e) { out.push(e instanceof TypeError); }
        document.getElementById("o").textContent = out.join(" | ");
    </script></body>"##);
    assert_eq!(
        text_of(&doc, "o"),
        "https://example.com/p/x/y?a=1&b=2#h | https://example.com | example.com | /p/x/y | ?a=1&b=2&c=3+4 | 9 | 1 | https://other.test:8080/p/x/y?z=9 | false |  | a=1&b=x+y | q=1,q=2,r= | true"
    );
}

#[test]
fn text_codecs_and_base64_are_standard() {
    let doc = run(r##"<body><p id="o"></p><script>
        const out = [];
        out.push(new TextDecoder("windows-1252").decode(new Uint8Array([0x93, 0x68, 0x69, 0x94])));
        const d = new TextDecoder(), bytes = new TextEncoder().encode("é€");
        out.push(d.decode(bytes.subarray(0, 3), { stream: true }) + "|" + d.decode(bytes.subarray(3)));
        out.push(new TextDecoder("latin1").encoding, btoa("hiÿ"), atob(" aGk= "));
        try { btoa("€"); } catch (e) { out.push(e.name); }
        try { new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array([0xff])); } catch (e) { out.push(e instanceof TypeError); }
        document.getElementById("o").textContent = out.join(" ");
    </script></body>"##);
    assert_eq!(text_of(&doc, "o"), "\u{201c}hi\u{201d} é|€ windows-1252 aGn/ hi InvalidCharacterError true");
}

#[test]
fn window_scrolling_goes_to_the_tab_and_back() {
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p><script>
        window.scrollTo({ top: 500, behavior: "smooth" });
        window.scrollBy(0, 20);
        addEventListener("scroll", () => document.getElementById("o").textContent = "scrolled " + scrollY);
        document.getElementById("o").textContent = [scrollY, pageYOffset, scrollX].join(" ");
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: false, session: 0 };
    let (doc, host) = js::start(doc, opts);
    // Scripts see where they scrolled at once...
    assert_eq!(text_of(&doc, "o"), "520 520 0");
    let host = host.unwrap();
    // ...the tab was asked to scroll there...
    let mut asked = Vec::new();
    while let Ok(msg) = host.updates.try_recv() {
        if let js::FromJs::ScrollTo { y, smooth, .. } = msg {
            asked.push((y, smooth));
        }
    }
    assert_eq!(asked, vec![(500.0, true), (520.0, false)]);
    // ...and where the reader scrolls comes back, with a `scroll` event.
    host.send(js::ToJs::Layout { rects: Vec::new(), gone: Vec::new(), viewport: (800.0, 700.0), scroll: (0.0, 1200.0) });
    let mut text = String::new();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while text != "scrolled 1200" && std::time::Instant::now() < until {
        if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
            text = text_of(&d, "o");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(text, "scrolled 1200");
}

#[test]
fn location_navigator_and_console_are_native() {
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p><script>
        console.log("x", { a: 1 }, document.body, new Error("e"));
        console.group(); console.groupEnd(); console.table([]);
        document.getElementById("o").textContent = [
          location.href, location.protocol, location.host, location.pathname, location.search, location.hash, location.origin,
          String(location), navigator.language, navigator.languages.join("+"), typeof navigator.sendBeacon, navigator.onLine,
        ].join(" ");
        </script></body>"##,
        Url::parse("https://example.com:8080/a/b?q=1#top").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: false, session: 0 };
    let (doc, _host) = js::start(doc, opts);
    assert_eq!(
        text_of(&doc, "o"),
        "https://example.com:8080/a/b?q=1#top https: example.com:8080 /a/b ?q=1 #top https://example.com:8080 https://example.com:8080/a/b?q=1#top en-US en-US+en function true"
    );
}

#[test]
fn web_storage_is_native_with_named_properties() {
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p><script>
        localStorage.clear();
        localStorage.setItem("a", 1);
        localStorage.b = "two";
        sessionStorage["c"] = "three";
        const before = [localStorage.length, localStorage.getItem("a"), localStorage.b, localStorage.key(0), "a" in localStorage, "zz" in localStorage, Object.keys(localStorage).join("+"), localStorage.getItem("zz")];
        delete localStorage.a;
        localStorage.removeItem("b");
        document.getElementById("o").textContent = [...before, localStorage.length, sessionStorage.getItem("c"), typeof localStorage.setItem, JSON.stringify(Object.entries(sessionStorage))].join(" ");
        </script></body>"##,
        Url::parse("https://storage.example/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: false, session: 77 };
    let (doc, _host) = js::start(doc, opts);
    assert_eq!(text_of(&doc, "o"), r#"2 1 two a true false a+b  0 three function [["c","three"]]"#);
}

#[test]
fn history_is_native_with_state_and_popstate() {
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p><script>
        const seen = [];
        addEventListener("popstate", (e) => { seen.push("pop:" + JSON.stringify(e.state) + "@" + location.pathname); show(); });
        addEventListener("hashchange", (e) => { seen.push("hash:" + e.newURL.split("#")[1]); show(); });
        history.pushState({ page: 2 }, "", "/two");
        history.pushState({ page: 3 }, "", "/three#x");
        history.replaceState({ page: 33 }, "");
        const show = () => document.getElementById("o").textContent = [history.length, JSON.stringify(history.state), location.pathname, seen.join(",")].join(" ");
        show();
        </script></body>"##,
        Url::parse("https://example.com/one").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_secs(5), debug: false, session: 0 };
    let (doc, host) = js::start(doc, opts);
    assert_eq!(text_of(&doc, "o"), r#"3 {"page":33} /three "#);
    // The tab goes back one: the entry before is current again.
    let host = host.unwrap();
    host.send(js::ToJs::Traverse { delta: -1 });
    let mut text = String::new();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !text.contains("pop:") && std::time::Instant::now() < until {
        if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
            text = text_of(&d, "o");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(text, r#"3 {"page":2} /two pop:{"page":2}@/two"#);
}

#[test]
fn native_timers_call_back_in_order() {
    let doc = html::parse_scripted(
        r##"<body><p id="o"></p><script>
        const seen = [];
        const show = () => document.getElementById("o").textContent = seen.join(",");
        setTimeout((a, b) => { seen.push("t" + a + b); show(); }, 20, 1, 2);
        let n = 0;
        const iv = setInterval(() => { seen.push("i" + (++n)); if (n === 2) clearInterval(iv); show(); }, 5);
        const gone = setTimeout(() => seen.push("never"), 10); clearTimeout(gone);
        requestAnimationFrame((t) => { seen.push(typeof t === "number" && t > 0 ? "raf" : "bad"); show(); });
        window.__s = (x) => { seen.push(x); show(); };
        setTimeout("__s('str')", 30);
        </script></body>"##,
        Url::parse("https://example.com/").unwrap(),
        true,
    );
    let opts = js::Options { fetch_text: Arc::new(|_| None), dark: false, budget: Duration::from_millis(200), debug: false, session: 0 };
    let (doc, host) = js::start(doc, opts);
    let host = host.unwrap();
    let mut text = text_of(&doc, "o");
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while !text.ends_with("str") && std::time::Instant::now() < until {
        if let Ok(js::FromJs::Document(d)) = host.updates.try_recv() {
            text = text_of(&d, "o");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(text, "i1,i2,raf,t12,str");
}
