//! `WebSocket`: one thread per connection (tungstenite over rustls),
//! talking to the script thread through its queue.

use std::net::TcpStream;
use std::sync::mpsc;
use std::time::Duration;

use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, protocol::CloseFrame, protocol::frame::coding::CloseCode};

use crate::ToJs;

/// What the page sends down a socket.
pub enum Outgoing {
    Text(String),
    Binary(Vec<u8>),
    Close(u16, String),
}

/// What happened on a socket.
#[derive(Debug)]
pub enum SocketEvent {
    Open { protocol: String },
    Text(String),
    Binary(Vec<u8>),
    Error(String),
    Close { code: u16, reason: String, clean: bool },
}

/// Connect in the background; returns where to send the page's frames.
pub fn open(id: u32, url: String, protocols: Vec<String>, origin: String, events: mpsc::Sender<ToJs>) -> mpsc::Sender<Outgoing> {
    let (tx, rx) = mpsc::channel::<Outgoing>();
    std::thread::Builder::new()
        .name("feather-websocket".into())
        .spawn(move || {
            let send = |event: SocketEvent| {
                events.send(ToJs::Socket { id, event }).ok();
            };
            if let Err(e) = run(&url, &protocols, &origin, &rx, &send) {
                send(SocketEvent::Error(e));
                send(SocketEvent::Close { code: 1006, reason: String::new(), clean: false });
            }
        })
        .ok();
    tx
}

fn run(url: &str, protocols: &[String], origin: &str, rx: &mpsc::Receiver<Outgoing>, send: &dyn Fn(SocketEvent)) -> Result<(), String> {
    let mut request = url.into_client_request().map_err(|e| e.to_string())?;
    let headers = request.headers_mut();
    if let Ok(v) = origin.parse() {
        headers.insert("origin", v);
    }
    if !protocols.is_empty()
        && let Ok(v) = protocols.join(", ").parse()
    {
        headers.insert("sec-websocket-protocol", v);
    }
    let (mut socket, response) = tungstenite::connect(request).map_err(|e| format!("couldn't connect to {url}: {e}"))?;
    let protocol = response.headers().get("sec-websocket-protocol").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    // Short reads, so the page's frames go out promptly.
    let tcp: &TcpStream = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => s.get_ref(),
        _ => return Err("unsupported stream".into()),
    };
    tcp.set_read_timeout(Some(Duration::from_millis(30))).ok();
    send(SocketEvent::Open { protocol });
    loop {
        // The page's frames.
        loop {
            match rx.try_recv() {
                Ok(Outgoing::Text(t)) => socket.send(Message::text(t)).map_err(|e| e.to_string())?,
                Ok(Outgoing::Binary(b)) => socket.send(Message::binary(b)).map_err(|e| e.to_string())?,
                Ok(Outgoing::Close(code, reason)) => {
                    socket
                        .close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.into() }))
                        .ok();
                }
                Err(mpsc::TryRecvError::Empty) => break,
                // The page is gone.
                Err(mpsc::TryRecvError::Disconnected) => {
                    socket.close(None).ok();
                    return Ok(());
                }
            }
        }
        // The server's.
        match socket.read() {
            Ok(Message::Text(t)) => send(SocketEvent::Text(t.to_string())),
            Ok(Message::Binary(b)) => send(SocketEvent::Binary(b.to_vec())),
            Ok(Message::Close(frame)) => {
                let (code, reason) = frame.map_or((1005, String::new()), |f| (u16::from(f.code), f.reason.to_string()));
                // Finish the closing handshake.
                while socket.read().is_ok() {}
                send(SocketEvent::Close { code, reason, clean: true });
                return Ok(());
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                send(SocketEvent::Close { code: 1000, reason: String::new(), clean: true });
                return Ok(());
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}
