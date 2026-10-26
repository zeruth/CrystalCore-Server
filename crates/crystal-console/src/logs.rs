//! Logging into the console: a `tracing` fmt layer whose writer appends to a ring buffer the UI
//! draws from, behind a filter that can be changed while the server runs.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::Mutex;
use tracing::{Level, Metadata};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, reload};

/// Lines kept for scrolling back.
pub const SCROLLBACK: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Log(Level),
    /// A command typed at the console, echoed.
    Input,
    /// What a console command answered.
    Output,
    /// A console command failed.
    Error,
}

#[derive(Debug, Clone)]
pub struct Line {
    /// Local time, `HH:MM:SS`.
    pub time: String,
    pub kind: Kind,
    pub text: String,
}

#[derive(Default)]
struct Inner {
    lines: Mutex<VecDeque<Line>>,
    /// Bumped on every line, so the UI redraws only when something came in.
    version: AtomicU64,
    /// Also print to stdout (after the UI has closed, for the shutdown's lines).
    passthrough: AtomicBool,
}

/// The log buffer, shared by the `tracing` writer and the UI.
#[derive(Clone, Default)]
pub struct Sink(Arc<Inner>);

impl Sink {
    pub fn push(&self, kind: Kind, text: impl Into<String>) {
        let text = text.into();
        if self.0.passthrough.load(Ordering::Relaxed) {
            println!("{text}");
        }
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        let mut lines = self.0.lines.lock();
        for part in text.split('\n') {
            if lines.len() == SCROLLBACK {
                lines.pop_front();
            }
            lines.push_back(Line { time: time.clone(), kind, text: part.trim_end().to_owned() });
        }
        self.0.version.fetch_add(1, Ordering::Release);
    }

    pub fn version(&self) -> u64 {
        self.0.version.load(Ordering::Acquire)
    }

    /// Runs `f` over the buffered lines, oldest first.
    pub fn with_lines<T>(&self, f: impl FnOnce(&VecDeque<Line>) -> T) -> T {
        f(&self.0.lines.lock())
    }

    pub fn clear(&self) {
        self.0.lines.lock().clear();
        self.0.version.fetch_add(1, Ordering::Release);
    }

    pub fn set_passthrough(&self, on: bool) {
        self.0.passthrough.store(on, Ordering::Relaxed);
    }
}

/// One event's text, handed to the sink when the fmt layer is done writing it.
pub struct EventWriter {
    sink: Sink,
    level: Level,
    buf: Vec<u8>,
}

impl Write for EventWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for EventWriter {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            let text = String::from_utf8_lossy(&self.buf);
            self.sink.push(Kind::Log(self.level), text.trim_end());
        }
    }
}

impl<'a> MakeWriter<'a> for Sink {
    type Writer = EventWriter;

    fn make_writer(&'a self) -> EventWriter {
        EventWriter { sink: self.clone(), level: Level::INFO, buf: Vec::new() }
    }

    fn make_writer_for(&'a self, meta: &Metadata<'_>) -> EventWriter {
        EventWriter { sink: self.clone(), level: *meta.level(), buf: Vec::new() }
    }
}

/// The installed logging: the buffer and the handle that changes the filter.
pub struct Logging {
    pub sink: Sink,
    filter: reload::Handle<EnvFilter, Registry>,
    /// The filter in force, as given.
    current: Mutex<String>,
}

impl Logging {
    /// Installs the global subscriber, writing into a new [`Sink`]. `RUST_LOG`, when set,
    /// overrides `filter`.
    pub fn install(filter: &str) -> anyhow::Result<Self> {
        let directives = std::env::var("RUST_LOG").unwrap_or_else(|_| filter.to_owned());
        let (layer, handle) = reload::Layer::new(EnvFilter::try_new(&directives)?);
        let sink = Sink::default();
        let fmt = tracing_subscriber::fmt::layer()
            .with_writer(sink.clone())
            .with_ansi(false)
            .without_time()
            .with_level(false)
            .with_target(true);
        tracing_subscriber::registry().with(layer).with(fmt).try_init()?;
        Ok(Self { sink, filter: handle, current: Mutex::new(directives) })
    }

    /// Changes the filter (`tracing` directives, as in `RUST_LOG`).
    pub fn set_filter(&self, directives: &str) -> anyhow::Result<()> {
        self.filter.reload(EnvFilter::try_new(directives)?)?;
        *self.current.lock() = directives.to_owned();
        Ok(())
    }

    pub fn filter(&self) -> String {
        self.current.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_lines_and_splits_them() {
        let sink = Sink::default();
        let v = sink.version();
        sink.push(Kind::Output, "one\ntwo  ");
        assert!(sink.version() > v);
        let texts = sink.with_lines(|l| l.iter().map(|l| l.text.clone()).collect::<Vec<_>>());
        assert_eq!(texts, ["one", "two"]);
        sink.clear();
        assert_eq!(sink.with_lines(|l| l.len()), 0);
    }

    #[test]
    fn writer_hands_whole_events_over() {
        let sink = Sink::default();
        {
            let mut w = sink.make_writer();
            w.write_all(b"crystal_zone: ").unwrap();
            w.write_all(b"connected\n").unwrap();
        }
        sink.with_lines(|l| {
            assert_eq!(l.len(), 1);
            assert_eq!(l[0].text, "crystal_zone: connected");
            assert_eq!(l[0].kind, Kind::Log(Level::INFO));
        });
    }
}
