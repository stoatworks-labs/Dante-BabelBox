//! An in-memory ring of the most recent log lines.
//!
//! A crash report is only useful if it says what the program was doing on the
//! way down. Re-reading the log file from the panic hook is not good enough:
//! the file writer is non-blocking, so the lines that matter most — the last
//! ones — are still in its queue when the process dies.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// Number of lines kept. Enough to cover the run-up to a fault without making
/// the report too big to read.
pub const CAPACITY: usize = 500;

#[derive(Debug)]
pub struct Ring {
    buf: Mutex<VecDeque<String>>,
    cap: usize,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: Mutex::new(VecDeque::with_capacity(cap)),
            cap,
        }
    }

    pub fn push(&self, line: String) {
        let mut buf = self.lock();
        if buf.len() == self.cap {
            buf.pop_front();
        }
        buf.push_back(line);
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.lock().iter().cloned().collect()
    }

    /// Recover from poisoning rather than panicking.
    ///
    /// This lock is read from the panic hook. If a thread panicked while
    /// holding it, unwrapping here would panic inside the panic handler and
    /// abort the process before the report is written — losing exactly the
    /// evidence we came for.
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<String>> {
        self.buf.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A `tracing` layer that copies every event into the ring.
pub struct RingLayer {
    ring: Arc<Ring>,
}

impl RingLayer {
    pub fn new(ring: Arc<Ring>) -> Self {
        Self { ring }
    }
}

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = LineVisitor::default();
        event.record(&mut visitor);
        self.ring.push(format!(
            "{} {:<5} {}: {}{}",
            crate::stamp_rfc3339(),
            meta.level(),
            meta.target(),
            visitor.message,
            visitor.fields
        ));
    }
}

#[derive(Default)]
struct LineVisitor {
    message: String,
    fields: String,
}

impl Visit for LineVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        } else {
            self.push_field(field.name(), value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        if field.name() == "message" {
            self.message = rendered;
        } else {
            self.push_field(field.name(), &rendered);
        }
    }
}

impl LineVisitor {
    fn push_field(&mut self, name: &str, value: &str) {
        use std::fmt::Write;
        let _ = write!(self.fields, " {name}={value}");
    }
}
