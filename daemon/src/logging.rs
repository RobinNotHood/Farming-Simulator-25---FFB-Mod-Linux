//! Ring-buffer log sink for the GUI.
//!
//! tracing_subscriber writes to this `MakeWriter`; the GUI's "Logs" tab
//! reads the ring. We also forward to stderr for the systemd journal.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::io::{self, Write};
use tracing_subscriber::fmt::MakeWriter;

const MAX_LINES: usize = 1000;

static RING: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::with_capacity(MAX_LINES)));

/// Factory for log writers: each emitted event gets a fresh `TeeLine`
/// which accumulates bytes and, on flush / newline, writes to both
/// stderr and the GUI ring buffer.
///
/// We don't require `Clone` on the underlying writer because we take a
/// new `io::stderr()` each call — `io::stderr()` is cheap and buffered
/// internally by the standard library.
#[derive(Default, Clone, Copy)]
pub struct RingTee;

impl RingTee {
    pub fn new() -> Self {
        Self
    }
}

impl<'a> MakeWriter<'a> for RingTee {
    type Writer = TeeLine;
    fn make_writer(&'a self) -> Self::Writer {
        TeeLine {
            pending: Vec::with_capacity(256),
        }
    }
}

pub struct TeeLine {
    pending: Vec<u8>,
}

impl Write for TeeLine {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        io::stderr().write_all(buf)?;
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            let s = String::from_utf8_lossy(&line)
                .trim_end_matches('\n')
                .to_string();
            if !s.is_empty() {
                push_line(s);
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
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
