//! Bytes still arriving: a download the demuxer reads while it runs
//! (progressive playback), blocking for what hasn't come yet.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
struct Buf {
    data: Vec<u8>,
    /// All of it is here.
    done: bool,
    /// The download failed, or the reader was let go of.
    closed: bool,
    /// From `Content-Length`, before it's all here.
    expected: Option<u64>,
}

#[derive(Default)]
struct Shared {
    buf: Mutex<Buf>,
    more: Condvar,
}

/// The download's end: hands bytes over as they come.
#[derive(Clone)]
pub struct Feeder(Arc<Shared>);

/// The demuxer's end.
pub struct Growing {
    shared: Arc<Shared>,
    pos: u64,
}

/// A source with nothing yet, and its feeder.
pub fn growing(expected: Option<u64>) -> (Feeder, Growing) {
    let shared = Arc::new(Shared::default());
    shared.buf.lock().unwrap_or_else(|e| e.into_inner()).expected = expected;
    (Feeder(shared.clone()), Growing { shared, pos: 0 })
}

impl Feeder {
    pub fn push(&self, bytes: &[u8]) {
        self.0.buf.lock().unwrap_or_else(|e| e.into_inner()).data.extend_from_slice(bytes);
        self.0.more.notify_all();
    }

    pub fn expect(&self, len: u64) {
        self.0.buf.lock().unwrap_or_else(|e| e.into_inner()).expected = Some(len);
    }

    /// The last bytes came.
    pub fn finish(&self) {
        self.0.buf.lock().unwrap_or_else(|e| e.into_inner()).done = true;
        self.0.more.notify_all();
    }

    /// No more is coming (failed, or nobody wants it): reads past what's
    /// here fail instead of waiting.
    pub fn close(&self) {
        self.0.buf.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        self.0.more.notify_all();
    }

    /// Whether the reader is gone (stop downloading).
    pub fn abandoned(&self) -> bool {
        Arc::strong_count(&self.0) == 1 || self.0.buf.lock().is_ok_and(|b| b.closed)
    }

    /// (Received, total if known.)
    pub fn progress(&self) -> (u64, Option<u64>) {
        let b = self.0.buf.lock().unwrap_or_else(|e| e.into_inner());
        (b.data.len() as u64, if b.done { Some(b.data.len() as u64) } else { b.expected })
    }
}

impl Growing {
    /// Bytes already all here.
    pub fn whole(bytes: Vec<u8>) -> Growing {
        let (feeder, g) = growing(Some(bytes.len() as u64));
        feeder.0.buf.lock().unwrap_or_else(|e| e.into_inner()).data = bytes;
        feeder.finish();
        g
    }

    /// A handle to stop waiting (when the player stops).
    pub fn feeder(&self) -> Feeder {
        Feeder(self.shared.clone())
    }
}

impl Drop for Growing {
    fn drop(&mut self) {
        self.feeder().close();
    }
}

impl Read for Growing {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut b = self.shared.buf.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let have = b.data.len() as u64;
            if self.pos < have {
                let at = self.pos as usize;
                let n = out.len().min(b.data.len() - at);
                out[..n].copy_from_slice(&b.data[at..at + n]);
                self.pos += n as u64;
                return Ok(n);
            }
            if b.done {
                return Ok(0);
            }
            if b.closed {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "download stopped"));
            }
            b = self.shared.more.wait(b).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Seek for Growing {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => {
                // The end isn't known until it's all here (or the length is).
                let mut b = self.shared.buf.lock().unwrap_or_else(|e| e.into_inner());
                let len = loop {
                    if b.done {
                        break b.data.len() as u64;
                    }
                    if let Some(n) = b.expected {
                        break n;
                    }
                    if b.closed {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "download stopped"));
                    }
                    b = self.shared.more.wait(b).unwrap_or_else(|e| e.into_inner());
                };
                len.checked_add_signed(d)
            }
        };
        self.pos = pos.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        Ok(self.pos)
    }
}

impl symphonia::core::io::MediaSource for Growing {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        let b = self.shared.buf.lock().unwrap_or_else(|e| e.into_inner());
        if b.done { Some(b.data.len() as u64) } else { b.expected }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_wait_for_bytes() {
        let (feeder, mut g) = growing(None);
        let t = std::thread::spawn(move || {
            let mut all = Vec::new();
            g.read_to_end(&mut all).unwrap();
            all
        });
        feeder.push(b"hel");
        std::thread::sleep(std::time::Duration::from_millis(10));
        feeder.push(b"lo");
        feeder.finish();
        assert_eq!(t.join().unwrap(), b"hello");
    }
}
