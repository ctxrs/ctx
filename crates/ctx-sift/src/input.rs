//! Direct compact/restore I/O, with observations at the actual writer boundary.
use crate::{
    observation::*,
    observe::{self, Observed},
};
use anyhow::{Context, Result, bail};
use sift::{Compactor, Encoding};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, BufReader, Read, Write},
    time::Instant,
};

pub(crate) fn run(
    file: Option<OsString>,
    encoding: Option<Encoding>,
    observed: &mut Observed,
) -> Result<()> {
    observed.facts.mode = if encoding.is_some() {
        Mode::Restore
    } else {
        Mode::Automatic
    };
    observed.phase = Phase::Input;
    let mut input: Box<dyn Read> = match file {
        Some(path) if path != "-" => Box::new(BufReader::new(
            File::open(path).context("cannot open input file")?,
        )),
        _ => Box::new(io::stdin().lock()),
    };
    if encoding == Some(Encoding::Raw) {
        observed.phase = Phase::Input;
        let mut output = observe::Writer::new(io::stdout().lock());
        let result = io::copy(&mut input, &mut output);
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
            presentation: Presentation::Restored,
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
    let mut bytes = Vec::new();
    let read = input.read_to_end(&mut bytes);
    observed.facts.streams[0] = Some(StreamFacts {
        input_bytes: Some(bytes.len() as u64),
        input_complete: read.is_ok(),
        ..Default::default()
    });
    read.context("cannot read input")?;
    observed.phase = Phase::Codec;
    let start = Instant::now();
    let rendered;
    let output = match (encoding, std::str::from_utf8(&bytes)) {
        (None, Err(_)) => {
            observed.skip(SkipReason::Binary);
            let facts = observed.facts.streams[0].get_or_insert_default();
            facts.skip = Some(SkipReason::Binary);
            facts.missing = Some(Missingness::Binary);
            bytes.as_slice()
        }
        (None, Ok(text)) => {
            let compactor = Compactor::new().inspect_err(|_| {
                let stream = observed.facts.streams[0].get_or_insert_default();
                stream.missing = Some(Missingness::TokenizerUnavailable);
                stream.skip = Some(SkipReason::TokenizerUnavailable);
            })?;
            let result = compactor.compact(text);
            observed.facts.streams[0] = Some(observe::compacted(&result, bytes.len()));
            rendered = result.text;
            rendered.as_bytes()
        }
        (Some(encoding), Ok(text)) => {
            observed.phase = Phase::Render;
            rendered = sift::restore(encoding, text)?;
            let facts = observed.facts.streams[0].get_or_insert_default();
            facts.presentation = Presentation::Restored;
            facts.encoding = Some(observe::encoding(encoding));
            facts.missing = Some(Missingness::NotApplicable);
            rendered.as_bytes()
        }
        (Some(_), Err(_)) => {
            observed.phase = Phase::Input;
            bail!("encoded input must be UTF-8; raw mode accepts arbitrary bytes")
        }
    };
    observed.facts.transform_duration = Some(start.elapsed());
    observed.phase = Phase::Output;
    let start = Instant::now();
    let result = observe::write_stream(
        &mut io::stdout().lock(),
        output,
        observed.facts.streams[0].get_or_insert_default(),
    );
    observed.facts.output_duration = Some(start.elapsed());
    observed.facts.delivery = if result.is_ok() {
        Delivery::Flushed
    } else {
        Delivery::Failed
    };
    result?;
    Ok(())
}
