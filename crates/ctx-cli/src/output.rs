#[cfg(test)]
use std::cell::RefCell;
#[cfg(not(test))]
use std::sync::{Mutex, OnceLock};
use std::{
    fmt,
    io::{self, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use anyhow::Result;
use clap::ValueEnum;
use serde_json::Value;

use crate::ui::StreamKind;

#[cfg(test)]
thread_local! {
    static ACTIVE_MEASUREMENT: RefCell<Option<Arc<OutputByteCounter>>> =
        const { RefCell::new(None) };
}

#[cfg(not(test))]
static ACTIVE_MEASUREMENT: OnceLock<Mutex<Option<Arc<OutputByteCounter>>>> = OnceLock::new();

#[derive(Default)]
struct OutputByteCounter {
    stdout: AtomicU64,
    stderr: AtomicU64,
}

/// Owns the content-free byte counter for one CLI invocation.
///
/// A production CLI process owns one process-wide measurement so output from a
/// command worker thread is included. Unit tests use thread-local measurements
/// to keep parallel cases independent. Writers receive a cloned counter when
/// constructed, so a destination keeps measuring through its final flush.
pub(crate) struct OutputMeasurement {
    counter: Arc<OutputByteCounter>,
    previous: Option<Arc<OutputByteCounter>>,
}

impl OutputMeasurement {
    pub(crate) fn start() -> Self {
        let counter = Arc::new(OutputByteCounter::default());
        let previous = replace_active_measurement(Some(counter.clone()));
        Self { counter, previous }
    }

    pub(crate) fn stream_bytes(&self, stream: StreamKind) -> u64 {
        self.counter.stream(stream).load(Ordering::Relaxed)
    }

    pub(crate) fn total_bytes(&self) -> u64 {
        self.stream_bytes(StreamKind::Stdout)
            .saturating_add(self.stream_bytes(StreamKind::Stderr))
    }
}

impl Drop for OutputMeasurement {
    fn drop(&mut self) {
        replace_active_measurement(self.previous.take());
    }
}

#[cfg(test)]
fn replace_active_measurement(
    replacement: Option<Arc<OutputByteCounter>>,
) -> Option<Arc<OutputByteCounter>> {
    ACTIVE_MEASUREMENT.with(|active| std::mem::replace(&mut *active.borrow_mut(), replacement))
}

#[cfg(not(test))]
fn replace_active_measurement(
    replacement: Option<Arc<OutputByteCounter>>,
) -> Option<Arc<OutputByteCounter>> {
    let active = ACTIVE_MEASUREMENT.get_or_init(|| Mutex::new(None));
    let mut active = active
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::replace(&mut *active, replacement)
}

#[cfg(test)]
fn active_measurement() -> Option<Arc<OutputByteCounter>> {
    ACTIVE_MEASUREMENT.with(|active| active.borrow().clone())
}

#[cfg(not(test))]
fn active_measurement() -> Option<Arc<OutputByteCounter>> {
    ACTIVE_MEASUREMENT.get().and_then(|active| {
        active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    })
}

impl OutputByteCounter {
    fn stream(&self, stream: StreamKind) -> &AtomicU64 {
        match stream {
            StreamKind::Stdout => &self.stdout,
            StreamKind::Stderr => &self.stderr,
        }
    }

    fn add(&self, stream: StreamKind, bytes: usize) {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        let counter = self.stream(stream);
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_add(bytes))
        });
    }
}

pub(crate) struct MeasuredWriter<W> {
    writer: W,
    stream: StreamKind,
    counter: Option<Arc<OutputByteCounter>>,
}

impl<W> MeasuredWriter<W> {
    pub(crate) fn current(writer: W, stream: StreamKind) -> Self {
        let counter = active_measurement();
        Self {
            writer,
            stream,
            counter,
        }
    }
}

impl<W: Write> Write for MeasuredWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.writer.write(buffer)?;
        if let Some(counter) = &self.counter {
            counter.add(self.stream, written);
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn write_stream(stream: StreamKind, arguments: fmt::Arguments<'_>, newline: bool) {
    let result = match stream {
        StreamKind::Stdout => {
            let stdout = io::stdout();
            let mut writer = MeasuredWriter::current(stdout.lock(), stream);
            writer
                .write_fmt(arguments)
                .and_then(|()| newline.then(|| writer.write_all(b"\n")).transpose())
                .map(|_| ())
        }
        StreamKind::Stderr => {
            let stderr = io::stderr();
            let mut writer = MeasuredWriter::current(stderr.lock(), stream);
            writer
                .write_fmt(arguments)
                .and_then(|()| newline.then(|| writer.write_all(b"\n")).transpose())
                .map(|_| ())
        }
    };
    result.unwrap_or_else(|error| panic!("failed printing to {stream:?}: {error}"));
}

pub(crate) fn write_stdout(arguments: fmt::Arguments<'_>) {
    write_stream(StreamKind::Stdout, arguments, false);
}

pub(crate) fn write_stdout_line(arguments: fmt::Arguments<'_>) {
    write_stream(StreamKind::Stdout, arguments, true);
}

pub(crate) fn write_stderr_line(arguments: fmt::Arguments<'_>) {
    write_stream(StreamKind::Stderr, arguments, true);
}

pub(crate) fn stdout_writer() -> impl Write {
    MeasuredWriter::current(io::stdout(), StreamKind::Stdout)
}

pub(crate) fn stderr_writer() -> impl Write {
    MeasuredWriter::current(io::stderr(), StreamKind::Stderr)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    Text,
    Markdown,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum JsonOutputFormat {
    Text,
    Json,
}

impl JsonOutputFormat {
    pub(crate) const fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SqlFormat {
    Table,
    Json,
    Csv,
    Raw,
}

impl OutputFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
        }
    }
}

pub(crate) fn compact_json(mut value: Value) -> Value {
    prune_null_json(&mut value);
    value
}
pub(crate) fn prune_null_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, nested| {
                prune_null_json(nested);
                !nested.is_null()
            });
        }
        Value::Array(items) => {
            for item in items {
                prune_null_json(item);
            }
        }
        _ => {}
    }
}
pub(crate) fn print_json(value: Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
