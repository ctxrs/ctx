// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Bounded stdin filter for native shell pipelines. It never starts a command.
use crate::{
    observation::*,
    observe::{self, Observed},
};
use anyhow::Result;
use sift::{CompactResult, Compactor};
use std::io::{self, Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

const LIMIT: usize = 8 * 1024 * 1024;
const WAIT: Duration = Duration::from_millis(250);

pub struct Measurement {
    pub original: Option<Vec<u8>>,
    pub compacted: Option<CompactResult>,
    pub input_bytes: u64,
    pub output_bytes: u64,
}

#[cfg(test)]
pub fn filter(
    input: impl Read + Send + 'static,
    output: impl Write,
    complete: bool,
) -> io::Result<Measurement> {
    filter_observed(input, output, complete, &mut Observed::new())
}

pub(crate) fn filter_observed(
    input: impl Read + Send + 'static,
    output: impl Write,
    complete: bool,
    observed: &mut Observed,
) -> io::Result<Measurement> {
    let mut output = observe::Writer::new(output);
    let mut input_bytes = 0u64;
    observed.facts.streams[0] = Some(StreamFacts::default());
    observed.facts.delivery = Delivery::NotAttempted;
    let result = (|| {
        let (send, receive) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut input = input;
            loop {
                let mut bytes = vec![0; 8192];
                match input.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => {
                        bytes.truncate(n);
                        if send.send(Ok(bytes)).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        let _ = send.send(Err(e));
                        break;
                    }
                }
            }
        });
        let mut buffer = Vec::new();
        let mut first = None;
        let mut streaming = false;
        loop {
            let received = match (streaming || complete, first) {
                (false, Some(at)) => {
                    receive.recv_timeout(WAIT.saturating_sub(Instant::now().duration_since(at)))
                }
                _ => receive.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(Ok(bytes)) => {
                    first.get_or_insert_with(Instant::now);
                    input_bytes = input_bytes.saturating_add(bytes.len() as u64);
                    if !streaming && buffer.len() + bytes.len() > LIMIT {
                        observed.skip(SkipReason::CaptureLimit);
                        output.write_all(&buffer)?;
                        buffer.clear();
                        streaming = true;
                    }
                    if streaming {
                        output.write_all(&bytes)?;
                        output.flush()?;
                    } else {
                        buffer.extend_from_slice(&bytes);
                    }
                }
                Ok(Err(e)) => {
                    output.write_all(&buffer)?;
                    output.flush()?;
                    return Err(e);
                }
                Err(RecvTimeoutError::Timeout) => {
                    observed.skip(SkipReason::StreamingDeadline);
                    output.write_all(&buffer)?;
                    output.flush()?;
                    buffer.clear();
                    streaming = true;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        observed.facts.streams[0]
            .get_or_insert_default()
            .input_complete = true;
        if streaming {
            let stream = observed.facts.streams[0].get_or_insert_default();
            stream.missing = Some(Missingness::Streaming);
            stream.skip = observed.facts.skip;
            return Ok(Measurement {
                original: None,
                compacted: None,
                input_bytes,
                output_bytes: input_bytes,
            });
        }
        let start = Instant::now();
        let compacted = if buffer.len() >= 256 {
            match std::str::from_utf8(&buffer) {
                Ok(text) => match Compactor::new() {
                    Ok(c) => Some(c.compact(text)),
                    Err(error) => {
                        observed.skip(SkipReason::TokenizerUnavailable);
                        observed.facts.outcome = Outcome::FailOpen;
                        observed.facts.failure = Some(observe::failure(Phase::Codec, &error));
                        None
                    }
                },
                Err(_) => {
                    observed.skip(SkipReason::Binary);
                    None
                }
            }
        } else {
            observed.skip(SkipReason::Small);
            None
        };
        observed.facts.transform_duration = Some(start.elapsed());
        let stream = observed.facts.streams[0].get_or_insert_default();
        if let Some(result) = &compacted {
            *stream = observe::compacted(result, buffer.len());
        } else {
            stream.skip = observed.facts.skip;
            stream.missing = Some(match observed.facts.skip {
                Some(SkipReason::Binary) => Missingness::Binary,
                Some(SkipReason::TokenizerUnavailable) => Missingness::TokenizerUnavailable,
                _ => Missingness::Small,
            });
            if buffer.is_empty() {
                stream.tokens = Some(TokenCounts {
                    input: 0,
                    emitted: 0,
                });
                stream.missing = None;
            }
        }
        let bytes = compacted
            .as_ref()
            .map_or(buffer.as_slice(), |c| c.text.as_bytes());
        let start = Instant::now();
        let write = output.write_all(bytes).and_then(|()| output.flush());
        observed.facts.output_duration = Some(start.elapsed());
        write?;
        let output_bytes = bytes.len() as u64;
        Ok(Measurement {
            original: Some(buffer),
            compacted,
            input_bytes,
            output_bytes,
        })
    })();
    let stream = observed.facts.streams[0].get_or_insert_default();
    stream.input_bytes = Some(input_bytes);
    stream.emitted_bytes = Some(output.written);
    stream.output_complete = result.is_ok();
    if result.is_err() {
        stream.tokens = None;
        stream.missing = Some(Missingness::Incomplete);
    }
    observed.phase = if output.failed {
        Phase::Output
    } else {
        Phase::Input
    };
    observed.facts.delivery = if result.is_ok() {
        Delivery::Flushed
    } else {
        Delivery::Failed
    };
    result
}

pub fn run(complete: bool, observed: &mut Observed) -> Result<()> {
    let started = Instant::now();
    observed.facts.mode = if complete {
        Mode::Capture
    } else {
        Mode::Automatic
    };
    let settings = crate::state::Settings::load().ok();
    if settings.as_ref().is_none_or(|s| !s.enabled) {
        observed.skip(if settings.is_none() {
            SkipReason::SettingsUnavailable
        } else {
            SkipReason::Disabled
        });
        if settings.is_none() {
            observed.facts.outcome = Outcome::FailOpen;
        }
        let mut output = observe::Writer::new(io::stdout().lock());
        let result = io::copy(&mut io::stdin().lock(), &mut output);
        observed.phase = if output.failed {
            Phase::Output
        } else {
            Phase::Input
        };
        observed.facts.streams[0] = Some(StreamFacts {
            input_bytes: result.as_ref().ok().copied(),
            emitted_bytes: Some(output.written),
            input_complete: result.is_ok(),
            missing: Some(Missingness::NotApplicable),
            skip: observed.facts.skip,
            ..Default::default()
        });
        result?;
        observed.phase = Phase::Output;
        output.flush()?;
        observed.facts.streams[0]
            .get_or_insert_default()
            .output_complete = true;
        observed.facts.delivery = Delivery::Flushed;
        return Ok(());
    }
    let measured = filter_observed(io::stdin(), io::stdout().lock(), complete, observed)?;
    let event = crate::state::Event {
        unix_millis: crate::state::unix_millis(),
        command: "filter".into(),
        input_tokens: measured.compacted.as_ref().map(|r| r.input_tokens as u64),
        output_tokens: measured.compacted.as_ref().map(|r| r.output_tokens as u64),
        input_bytes: measured.input_bytes,
        output_bytes: measured.output_bytes,
        duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        exit_code: None,
        source: Some("filter".into()),
        original_id: None,
    };
    observed.facts.local_record = Some(
        crate::state::record_observed(
            event,
            measured.original.as_deref().map(|bytes| (bytes, &[][..])),
        )
        .unwrap_or(LocalRecordOutcome::Unavailable),
    );
    Ok(())
}
