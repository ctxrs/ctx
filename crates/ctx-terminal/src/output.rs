use std::cell::RefCell;
#[cfg(not(test))]
use std::sync::{Mutex, OnceLock};
use std::{
    fmt,
    io::{self, Write},
    marker::PhantomData,
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use anyhow::Result;
use serde_json::Value;

use crate::ui::StreamKind;

thread_local! {
    static THREAD_MEASUREMENT: RefCell<Option<Arc<OutputByteCounter>>> =
        const { RefCell::new(None) };
}

#[cfg(not(test))]
static PROCESS_MEASUREMENT: OnceLock<Mutex<Option<Arc<OutputByteCounter>>>> = OnceLock::new();

#[derive(Default)]
struct OutputByteCounter {
    stdout: AtomicU64,
    stderr: AtomicU64,
}

/// Owns the content-free byte counter for one CLI invocation.
///
/// A production CLI process owns one process-wide measurement so output from a
/// command worker thread is included. Direct unit tests use thread-local
/// measurements, and an in-process harness can opt into the same isolation.
/// Writers receive a cloned counter when constructed, so a destination keeps
/// measuring through its final flush.
pub struct OutputMeasurement {
    counter: Arc<OutputByteCounter>,
    previous: Option<Arc<OutputByteCounter>>,
    scope: MeasurementScope,
    // A thread measurement restores creator-thread TLS on drop, so no guard
    // may cross threads even when it currently owns the process-wide scope.
    _not_send: PhantomData<Rc<()>>,
}

enum MeasurementScope {
    Thread,
    #[cfg(not(test))]
    Process,
}

impl OutputMeasurement {
    pub fn start() -> Self {
        #[cfg(test)]
        {
            Self::start_for_current_thread()
        }
        #[cfg(not(test))]
        {
            let counter = Arc::new(OutputByteCounter::default());
            let previous = replace_process_measurement(Some(counter.clone()));
            Self {
                counter,
                previous,
                scope: MeasurementScope::Process,
                _not_send: PhantomData,
            }
        }
    }

    /// Starts an in-process measurement isolated to writers created by the
    /// current thread.
    ///
    /// This keeps independent invocations in a parallel test harness from
    /// sharing one process-wide counter. Child-thread writers do not inherit
    /// this scope; the production CLI uses [`Self::start`] so worker output is
    /// included in its one invocation-wide measurement.
    pub fn start_for_current_thread() -> Self {
        let counter = Arc::new(OutputByteCounter::default());
        let previous = replace_thread_measurement(Some(counter.clone()));
        Self {
            counter,
            previous,
            scope: MeasurementScope::Thread,
            _not_send: PhantomData,
        }
    }

    pub fn stream_bytes(&self, stream: StreamKind) -> u64 {
        self.counter.stream(stream).load(Ordering::Relaxed)
    }

    pub fn total_bytes(&self) -> u64 {
        self.stream_bytes(StreamKind::Stdout)
            .saturating_add(self.stream_bytes(StreamKind::Stderr))
    }
}

impl Drop for OutputMeasurement {
    fn drop(&mut self) {
        match self.scope {
            MeasurementScope::Thread => {
                replace_thread_measurement(self.previous.take());
            }
            #[cfg(not(test))]
            MeasurementScope::Process => {
                replace_process_measurement(self.previous.take());
            }
        }
    }
}

fn replace_thread_measurement(
    replacement: Option<Arc<OutputByteCounter>>,
) -> Option<Arc<OutputByteCounter>> {
    THREAD_MEASUREMENT.with(|active| std::mem::replace(&mut *active.borrow_mut(), replacement))
}

#[cfg(not(test))]
fn replace_process_measurement(
    replacement: Option<Arc<OutputByteCounter>>,
) -> Option<Arc<OutputByteCounter>> {
    let active = PROCESS_MEASUREMENT.get_or_init(|| Mutex::new(None));
    let mut active = active
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::replace(&mut *active, replacement)
}

fn active_measurement() -> Option<Arc<OutputByteCounter>> {
    let thread_measurement = THREAD_MEASUREMENT.with(|active| active.borrow().clone());
    if thread_measurement.is_some() {
        return thread_measurement;
    }
    #[cfg(not(test))]
    {
        PROCESS_MEASUREMENT.get().and_then(|active| {
            active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        })
    }
    #[cfg(test)]
    {
        None
    }
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

pub struct MeasuredWriter<W> {
    writer: W,
    stream: StreamKind,
    counter: Option<Arc<OutputByteCounter>>,
}

impl<W> MeasuredWriter<W> {
    pub fn current(writer: W, stream: StreamKind) -> Self {
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
        StreamKind::Stdout => with_stdout_writer(|writer| {
            writer
                .write_fmt(arguments)
                .and_then(|()| newline.then(|| writer.write_all(b"\n")).transpose())
                .map(|_| ())
        }),
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

pub fn write_stdout(arguments: fmt::Arguments<'_>) {
    write_stream(StreamKind::Stdout, arguments, false);
}

pub fn write_stdout_line(arguments: fmt::Arguments<'_>) {
    write_stream(StreamKind::Stdout, arguments, true);
}

pub fn write_stderr_line(arguments: fmt::Arguments<'_>) {
    write_stream(StreamKind::Stderr, arguments, true);
}

pub fn with_stdout_writer<T>(operation: impl FnOnce(&mut dyn Write) -> T) -> T {
    let stdout = io::stdout();
    let mut writer = MeasuredWriter::current(stdout.lock(), StreamKind::Stdout);
    operation(&mut writer)
}

pub fn stderr_writer() -> impl Write {
    MeasuredWriter::current(io::stderr(), StreamKind::Stderr)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Markdown,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonOutputFormat {
    Text,
    Json,
}

impl JsonOutputFormat {
    pub const fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}

impl OutputFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
        }
    }
}

pub fn compact_json(mut value: Value) -> Value {
    prune_null_json(&mut value);
    value
}
pub fn prune_null_json(value: &mut Value) {
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
/// Writes one pretty JSON document and its trailing newline through the active
/// measured stdout authority.
pub fn print_json(value: Value) -> Result<()> {
    let rendered = serde_json::to_string_pretty(&value)?;
    write_stdout_line(format_args!("{rendered}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{print_json, OutputMeasurement};
    use crate::ui::StreamKind;

    #[test]
    fn print_json_measures_the_exact_pretty_document_and_trailing_newline() {
        let expected = b"{\n  \"answer\": 42,\n  \"message\": \"measured\"\n}\n";
        let value = json!({"answer": 42, "message": "measured"});
        let expected_document = expected
            .strip_suffix(b"\n")
            .expect("expected JSON output ends in exactly one newline");
        assert_eq!(
            serde_json::to_string_pretty(&value).unwrap().as_bytes(),
            expected_document
        );
        let measurement = OutputMeasurement::start();

        assert_eq!(measurement.total_bytes(), 0);
        print_json(value).unwrap();

        let expected_bytes = u64::try_from(expected.len()).unwrap();
        assert_eq!(
            measurement.stream_bytes(StreamKind::Stdout),
            expected_bytes,
            "zero or partial accounting must fail, including a missing newline byte"
        );
        assert_eq!(measurement.stream_bytes(StreamKind::Stderr), 0);
        assert_eq!(measurement.total_bytes(), expected_bytes);
    }
}
