//! Invocation-local bookkeeping. No storage, environment, consent or transport.
use crate::observation::*;
use std::{
    io::{self, Write},
    time::Instant,
};

pub(crate) struct Observed {
    pub facts: SiftObservation,
    pub phase: Phase,
    started: Instant,
}

impl Observed {
    pub fn new() -> Self {
        Self {
            facts: SiftObservation::default(),
            phase: Phase::Arguments,
            started: Instant::now(),
        }
    }

    pub fn request(&self) -> Self {
        let mut next = Self::new();
        next.facts.operation = self.facts.operation;
        next.facts.entry = self.facts.entry;
        next.facts.host = self.facts.host;
        next.facts.terminal = Terminal::ProtocolRequest;
        next
    }

    pub fn skip(&mut self, reason: SkipReason) {
        self.facts.skip = Some(reason);
        self.facts.outcome = Outcome::Skipped;
    }

    pub fn fail(&mut self, error: &anyhow::Error) {
        self.facts.failure = Some(failure(self.phase, error));
        self.facts.outcome = Outcome::Failure;
        if self.phase == Phase::Output {
            self.facts.delivery = Delivery::Failed;
        }
    }

    pub fn finish(
        &mut self,
        error: Option<&anyhow::Error>,
        callback: &mut dyn FnMut(SiftObservation),
    ) {
        if let Some(error) = error {
            self.fail(error);
        }
        self.facts.duration = self.started.elapsed();
        callback(self.facts);
    }
}

pub(crate) fn failure(phase: Phase, error: &anyhow::Error) -> Failure {
    let kind = error.chain().find_map(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .map(|e| e.kind())
            .or_else(|| {
                cause
                    .downcast_ref::<serde_json::Error>()
                    .and_then(|e| e.io_error_kind())
            })
    });
    Failure {
        phase,
        kind: match kind {
            Some(io::ErrorKind::NotFound) => FailureKind::NotFound,
            Some(io::ErrorKind::PermissionDenied) => FailureKind::PermissionDenied,
            Some(io::ErrorKind::BrokenPipe) => FailureKind::BrokenPipe,
            Some(io::ErrorKind::Interrupted) => FailureKind::Interrupted,
            Some(io::ErrorKind::TimedOut) => FailureKind::TimedOut,
            Some(io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput) => {
                FailureKind::InvalidInput
            }
            Some(_) => FailureKind::Io,
            None if phase == Phase::Codec => FailureKind::Tokenizer,
            None if matches!(phase, Phase::Arguments | Phase::Protocol) => {
                FailureKind::InvalidInput
            }
            None => FailureKind::Other,
        },
    }
}

pub(crate) fn host(value: &str) -> Host {
    match value {
        "claude" => Host::Claude,
        "copilot" => Host::Copilot,
        "hermes" => Host::Hermes,
        "codex" => Host::Codex,
        "cursor" => Host::Cursor,
        "gemini" => Host::Gemini,
        "vscode" => Host::VsCode,
        "droid" => Host::Droid,
        "vibe" => Host::Vibe,
        "pi" => Host::Pi,
        "omp" => Host::Omp,
        "opencode" => Host::OpenCode,
        "kilo" => Host::Kilo,
        _ => Host::Unknown,
    }
}

pub(crate) fn encoding(value: sift::Encoding) -> Encoding {
    match value {
        sift::Encoding::Raw => Encoding::Raw,
        sift::Encoding::JsonV1 => Encoding::Json,
        sift::Encoding::JsonRowsV1 => Encoding::JsonRows,
        sift::Encoding::JsonMinV1 => Encoding::JsonMin,
        sift::Encoding::JsonColumnsV1 => Encoding::JsonColumns,
        sift::Encoding::TextRunsV1 => Encoding::TextRuns,
        sift::Encoding::TextPrefixesV1 => Encoding::TextPrefixes,
        sift::Encoding::TextRefsV1 => Encoding::TextRefs,
        sift::Encoding::TextLinesV1 => Encoding::TextLines,
        sift::Encoding::TextSymbolsV1 => Encoding::TextSymbols,
    }
}

pub(crate) fn compacted(result: &sift::CompactResult, input_bytes: usize) -> StreamFacts {
    StreamFacts {
        input_bytes: Some(input_bytes as u64),
        emitted_bytes: None,
        input_complete: true,
        tokens: Some(TokenCounts {
            input: result.input_tokens as u64,
            emitted: result.output_tokens as u64,
        }),
        missing: None,
        encoding: Some(encoding(result.encoding)),
        presentation: if result.encoding == sift::Encoding::Raw {
            Presentation::Raw
        } else {
            Presentation::Codec
        },
        skip: (result.output_tokens >= result.input_tokens).then_some(SkipReason::NotSmaller),
        ..Default::default()
    }
}

/// Count accepted writes rather than the length of a proposed output buffer.
pub(crate) struct Writer<W> {
    pub inner: W,
    pub written: u64,
    pub failed: bool,
}
impl<W> Writer<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            written: 0,
            failed: false,
        }
    }
}
impl<W: Write> Write for Writer<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = self.inner.write(bytes);
        self.failed |= result.is_err();
        let n = result?;
        self.written = self.written.saturating_add(n as u64);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        self.failed |= result.is_err();
        result
    }
}

pub(crate) fn write_stream(
    output: &mut impl Write,
    bytes: &[u8],
    stream: &mut StreamFacts,
) -> io::Result<()> {
    let mut measured = Writer::new(output);
    let result = measured.write_all(bytes).and_then(|()| measured.flush());
    stream.emitted_bytes = Some(measured.written);
    stream.output_complete = result.is_ok();
    if result.is_err() {
        stream.tokens = None;
        stream.missing = Some(Missingness::Incomplete);
    }
    result
}

pub(crate) fn envelope_delivery(observed: &mut Observed, result: &io::Result<()>, replaced: bool) {
    observed.facts.delivery = if result.is_err() {
        Delivery::Failed
    } else if replaced {
        Delivery::Flushed
    } else {
        Delivery::Unchanged
    };
    if let Err(error) = result {
        observed.facts.failure = Some(failure(
            Phase::Output,
            &io::Error::from(error.kind()).into(),
        ));
        observed.facts.outcome = Outcome::Failure;
    }
    for stream in observed.facts.streams.iter_mut().flatten() {
        stream.output_complete = result.is_ok();
        if result.is_err() {
            stream.emitted_bytes = None;
            stream.tokens = None;
            stream.missing = Some(Missingness::Incomplete);
        } else if !replaced {
            // The no-op envelope retained host-owned text, but did not write it.
            stream.output_complete = false;
            stream.emitted_bytes = None;
            stream.tokens = None;
            stream.presentation = Presentation::Raw;
            stream.missing = Some(Missingness::NotApplicable);
        }
    }
}
