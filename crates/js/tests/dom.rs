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
    let opts = js::Options { fetch_text: Arc::new(net::fetch_text_cached), dark: false, budget: Duration::from_secs(20), debug: true };
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
    let doc = run(&html);
    println!("{}", text_of(&doc, "o"));
}
