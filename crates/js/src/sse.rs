//! `EventSource`: server-sent events over a streamed GET, reconnecting
//! after drops like browsers do.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use url::Url;

use crate::ToJs;
use crate::cors::{ScriptRequest, fetch_streaming};

pub fn open(id: u32, page: Url, url: Url, credentials: bool, closed: Arc<AtomicBool>, tx: mpsc::Sender<ToJs>) {
    std::thread::Builder::new()
        .name("feather-eventsource".into())
        .spawn(move || {
            let send = |kind: &'static str, event: String, data: String, last_id: String| {
                tx.send(ToJs::Sse { id, kind, event, data, last_id }).is_ok()
            };
            let mut retry = Duration::from_secs(3);
            let mut last_id = String::new();
            while !closed.load(Ordering::Relaxed) {
                // (No Cache-Control header: browsers express that as a request
                // mode, and the header would cost a CORS preflight.)
                let mut headers = vec![("accept".to_owned(), "text/event-stream".to_owned())];
                if !last_id.is_empty() {
                    headers.push(("last-event-id".to_owned(), last_id.clone()));
                }
                let req = ScriptRequest {
                    method: "GET".into(),
                    url: url.clone(),
                    headers,
                    body: None,
                    mode: "cors".into(),
                    credentials: if credentials { "include".into() } else { "same-origin".into() },
                };
                let fatal = std::cell::Cell::new(false);
                let mut parser = Parser::default();
                let result = fetch_streaming(
                    &page,
                    req,
                    |head| {
                        let event_stream = head.headers.iter().any(|(k, v)| k == "content-type" && v.to_ascii_lowercase().starts_with("text/event-stream"));
                        if head.status != 200 || !event_stream {
                            fatal.set(true);
                        } else {
                            send("open", String::new(), String::new(), String::new());
                        }
                    },
                    |chunk| {
                        if fatal.get() || closed.load(Ordering::Relaxed) {
                            return false;
                        }
                        for e in parser.feed(chunk) {
                            match e {
                                Parsed::Retry(ms) => retry = Duration::from_millis(ms),
                                Parsed::Event { name, data, id } => {
                                    if let Some(id) = id {
                                        last_id = id;
                                    }
                                    if !send("message", name, data, last_id.clone()) {
                                        return false;
                                    }
                                }
                            }
                        }
                        true
                    },
                );
                if closed.load(Ordering::Relaxed) {
                    return;
                }
                // A wrong answer (or one CORS refuses) ends it; a dropped
                // connection is retried.
                let refused = matches!(&result, Err(e) if e.starts_with("CORS"));
                if fatal.get() || refused {
                    send("close", String::new(), String::new(), String::new());
                    return;
                }
                if !send("error", String::new(), String::new(), String::new()) {
                    return;
                }
                std::thread::sleep(retry);
            }
        })
        .ok();
}

enum Parsed {
    Event { name: String, data: String, id: Option<String> },
    Retry(u64),
}

/// The `text/event-stream` format, fed in pieces.
#[derive(Default)]
struct Parser {
    pending: Vec<u8>,
    data: Vec<String>,
    name: String,
    id: Option<String>,
}

impl Parser {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Parsed> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                // Dispatch.
                if !self.data.is_empty() {
                    out.push(Parsed::Event {
                        name: if self.name.is_empty() { "message".into() } else { std::mem::take(&mut self.name) },
                        data: self.data.join("\n"),
                        id: self.id.take(),
                    });
                }
                self.data.clear();
                self.name.clear();
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line, ""),
            };
            match field {
                "data" => self.data.push(value.to_owned()),
                "event" => self.name = value.to_owned(),
                "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
                "retry" => {
                    if let Ok(ms) = value.parse() {
                        out.push(Parsed::Retry(ms));
                    }
                }
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_across_chunks() {
        let mut p = Parser::default();
        let mut events = p.feed(b"retry: 500\ndata: one\nda");
        events.extend(p.feed(b"ta: two\n\n: comment\nevent: tick\nid: 7\ndata: x\r\n\r\n"));
        let got: Vec<String> = events
            .into_iter()
            .map(|e| match e {
                Parsed::Retry(ms) => format!("retry {ms}"),
                Parsed::Event { name, data, id } => format!("{name}:{data}:{id:?}"),
            })
            .collect();
        assert_eq!(got, vec!["retry 500", "message:one\ntwo:None", "tick:x:Some(\"7\")"]);
    }
}
