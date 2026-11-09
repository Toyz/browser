//! Image loads a script can see: `new Image()`, `img.src = ...`. A few
//! worker threads fetch through the shared subresource cache (so the page
//! paints them without fetching again) and report the size back.

use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};

use url::Url;

use crate::ToJs;

struct Job {
    id: u32,
    url: Url,
    /// Any resource (a <link>'s): success is just fetching it.
    any: bool,
    reply: mpsc::Sender<ToJs>,
}

const WORKERS: usize = 6;

fn queue() -> &'static Mutex<mpsc::Sender<Job>> {
    static QUEUE: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = std::sync::Arc::new(Mutex::new(rx));
        for _ in 0..WORKERS {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name("feather-js-image".into())
                .spawn(move || {
                    loop {
                        let job = match rx.lock() {
                            Ok(rx) => rx.recv(),
                            Err(_) => return,
                        };
                        let Ok(job) = job else { return };
                        let fetched = net::fetch_bytes(&job.url).ok();
                        let size = if job.any { fetched.map(|_| (0, 0)) } else { fetched.and_then(|bytes| size_of(&bytes)) };
                        job.reply.send(ToJs::Image { id: job.id, size }).ok();
                    }
                })
                .ok();
        }
        Mutex::new(tx)
    })
}

/// Width and height, if the bytes are an image we know.
fn size_of(bytes: &[u8]) -> Option<(u32, u32)> {
    if let Ok(s) = imagesize::blob_size(bytes) {
        return Some((s.width as u32, s.height as u32));
    }
    // SVG: its width/height attributes, else viewBox, else 300x150.
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let start = head.find("<svg")?;
    let tag = &head[start..head[start..].find('>').map_or(head.len(), |e| start + e)];
    let attr = |name: &str| -> Option<f32> {
        let i = tag.find(&format!(" {name}=\""))? + name.len() + 3;
        let v = &tag[i..i + tag[i..].find('"')?];
        v.trim_end_matches("px").parse().ok()
    };
    if let (Some(w), Some(h)) = (attr("width"), attr("height")) {
        return Some((w as u32, h as u32));
    }
    if let Some(i) = tag.find(" viewBox=\"") {
        let v: Vec<f32> = tag[i + 10..].split('"').next()?.split([' ', ',']).filter_map(|n| n.parse().ok()).collect();
        if v.len() == 4 {
            return Some((v[2] as u32, v[3] as u32));
        }
    }
    Some((300, 150))
}

/// Start loading `url`; `ToJs::Image { id, .. }` arrives when it's done
/// (`any`: not necessarily an image; its size is 0x0).
pub fn load(id: u32, url: Url, any: bool, reply: mpsc::Sender<ToJs>) {
    if let Ok(q) = queue().lock() {
        q.send(Job { id, url, any, reply }).ok();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn svg_sizes() {
        assert_eq!(super::size_of(br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="16"></svg>"#), Some((24, 16)));
        assert_eq!(super::size_of(br#"<svg viewBox="0 0 10 20"></svg>"#), Some((10, 20)));
        assert_eq!(super::size_of(b"nope"), None);
    }
}
