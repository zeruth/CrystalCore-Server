//! Logging into the console: a `tracing` fmt layer whose writer appends to a ring buffer the UI
//! draws from, behind a filter that can be changed while the server runs.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
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
    file: Mutex<Option<LogFile>>,
}

/// The log on disk: started fresh, moved to `<path>.1` when it reaches `max` bytes.
struct LogFile {
    path: PathBuf,
    max: u64,
    written: u64,
    out: std::fs::File,
}

impl LogFile {
    fn create(path: &Path, max: u64) -> std::io::Result<Self> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        Ok(Self { path: path.to_owned(), max, written: 0, out: std::fs::File::create(path)? })
    }

    fn write(&mut self, line: &str) {
        if self.written + line.len() as u64 > self.max {
            let mut old = self.path.clone().into_os_string();
            old.push(".1");
            // std opens files shareable for deletion, so the open file can be renamed on Windows too
            let _ = std::fs::rename(&self.path, &old);
            if let Ok(fresh) = std::fs::File::create(&self.path) {
                self.out = fresh;
                self.written = 0;
            }
        }
        if self.out.write_all(line.as_bytes()).is_ok() {
            self.written += line.len() as u64;
        }
    }
}

fn file_line(line: &Line) -> String {
    match line.kind {
        Kind::Log(level) => format!("{} {:5} {}\n", line.time, level.as_str(), line.text),
        Kind::Input => format!("{} > {}\n", line.time, line.text),
        Kind::Output => format!("{}   {}\n", line.time, line.text),
        Kind::Error => format!("{} ! {}\n", line.time, line.text),
    }
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
        let mut file = self.0.file.lock();
        for part in text.split('\n') {
            if lines.len() == SCROLLBACK {
                lines.pop_front();
            }
            let line = Line { time: time.clone(), kind, text: part.trim_end().to_owned() };
            if let Some(f) = file.as_mut() {
                f.write(&file_line(&line));
            }
            lines.push_back(line);
        }
        self.0.version.fetch_add(1, Ordering::Release);
    }

    /// Also writes every line to `path` (truncated now), keeping it under `max_bytes` by moving
    /// it to `<path>.1` when full.
    pub fn log_to_file(&self, path: &Path, max_bytes: u64) -> std::io::Result<()> {
        *self.0.file.lock() = Some(LogFile::create(path, max_bytes.max(4096))?);
        Ok(())
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
    fn the_file_starts_fresh_and_rolls_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log/server.log");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "an old run\n").unwrap();
        let sink = Sink::default();
        sink.log_to_file(&path, 4096).unwrap();
        sink.push(Kind::Input, "who");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with(" > who\n") && !text.contains("old run"), "{text}");
        for i in 0..100 {
            sink.push(Kind::Log(Level::INFO), format!("line {i} {}", "x".repeat(60)));
        }
        let rolled = std::fs::metadata(path.with_extension("log.1")).unwrap().len();
        let now = std::fs::metadata(&path).unwrap().len();
        assert!(rolled <= 4096 && now <= 4096, "{rolled} {now}");
        assert!(std::fs::read_to_string(&path).unwrap().contains("INFO  line 99"));
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
