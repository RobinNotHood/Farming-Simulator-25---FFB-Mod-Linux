//! Ring-buffer log sink for the GUI.
//!
//! tracing_subscriber writes to this TeeWriter; the egui "Logs" tab reads
//! the ring and displays it. We also forward to stderr for systemd journal.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::io::Write;

const MAX_LINES: usize = 1000;

pub static RING: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::with_capacity(MAX_LINES)));

pub struct TeeWriter<W: Write> {
    inner: W,
}

impl<W: Write> TeeWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }
}

impl<W: Write + Send + 'static> tracing_subscriber::fmt::MakeWriter<'_> for TeeWriter<W>
where
    W: Clone,
{
    type Writer = TeeLine<W>;
    fn make_writer(&self) -> Self::Writer {
        TeeLine {
            inner: self.inner.clone(),
            pending: Vec::new(),
        }
    }
}

pub struct TeeLine<W: Write> {
    inner: W,
    pending: Vec<u8>,
}

impl<W: Write> Write for TeeLine<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buf);
        self.inner.write_all(buf)?;
        // Flush to ring at line boundaries.
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let line = self.pending.drain(..=pos).collect::<Vec<u8>>();
            let s = String::from_utf8_lossy(&line).trim_end_matches('\n').to_string();
            if !s.is_empty() {
                push_line(s);
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub fn push_line(line: String) {
    let mut r = RING.lock();
    if r.len() >= MAX_LINES {
        r.remove(0);
    }
    r.push(line);
}

pub fn snapshot() -> Vec<String> {
    RING.lock().clone()
}
