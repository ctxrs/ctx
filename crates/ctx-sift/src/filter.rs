// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Bounded stdin filter for native shell pipelines. It never starts a command.
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

pub fn filter(
    input: impl Read + Send + 'static,
    mut output: impl Write,
    complete: bool,
) -> io::Result<Measurement> {
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
    let mut input_bytes = 0;
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
                input_bytes += bytes.len() as u64;
                if !streaming && buffer.len() + bytes.len() > LIMIT {
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
                // Partial input is not a complete codec candidate. Preserve
                // bytes already read before reporting the source failure.
                output.write_all(&buffer)?;
                output.flush()?;
                return Err(e);
            }
            Err(RecvTimeoutError::Timeout) => {
                output.write_all(&buffer)?;
                output.flush()?;
                buffer.clear();
                streaming = true;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    if streaming {
        return Ok(Measurement {
            original: None,
            compacted: None,
            input_bytes,
            output_bytes: input_bytes,
        });
    }
    let compacted = if buffer.len() >= 256 {
        std::str::from_utf8(&buffer)
            .ok()
            .and_then(|text| Compactor::new().ok().map(|c| c.compact(text)))
    } else {
        None
    };
    let bytes = compacted
        .as_ref()
        .map_or(buffer.as_slice(), |c| c.text.as_bytes());
    output.write_all(bytes)?;
    output.flush()?;
    let output_bytes = bytes.len() as u64;
    Ok(Measurement {
        original: Some(buffer),
        compacted,
        input_bytes,
        output_bytes,
    })
}

pub fn run(complete: bool) -> Result<()> {
    let started = Instant::now();
    let settings = crate::state::Settings::load().ok();
    if settings.as_ref().is_none_or(|s| !s.enabled) {
        io::copy(&mut io::stdin().lock(), &mut io::stdout().lock())?;
        return Ok(());
    }
    let measured = filter(io::stdin(), io::stdout().lock(), complete)?;
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
    let _ = crate::state::record(
        event,
        measured.original.as_deref().map(|bytes| (bytes, &[][..])),
    );
    Ok(())
}
