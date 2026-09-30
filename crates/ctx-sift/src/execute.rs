// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
use crate::{command_view, runner, state, views};
use anyhow::Result;
use sift::Compactor;
use std::{
    ffi::OsString,
    io::{self, Write},
};

pub(crate) fn execute(
    args: &[OsString],
    raw: bool,
    capture: bool,
    view: Option<&views::View>,
    observed: &mut crate::observe::Observed,
) -> Result<i32> {
    use crate::observation::{Mode, Outcome, Phase, SkipReason};
    observed.facts.mode = if raw {
        Mode::Raw
    } else if view.is_some() {
        Mode::ExplicitView
    } else if capture {
        Mode::Capture
    } else {
        Mode::Automatic
    };
    observed.phase = Phase::Settings;
    let settings = match state::Settings::load() {
        Ok(settings) => settings,
        Err(error) => {
            observed.skip(SkipReason::SettingsUnavailable);
            observed.facts.outcome = Outcome::FailOpen;
            let _ = writeln!(
                io::stderr(),
                "ctx sift: {error:#}; passing command output through"
            );
            state::Settings {
                enabled: false,
                record_usage: false,
                ..Default::default()
            }
        }
    };
    let excluded = args
        .first()
        .is_some_and(|arg| settings.excludes(&arg.to_string_lossy()));
    if raw {
        observed.skip(SkipReason::ExplicitRaw);
    } else if observed.facts.skip.is_none() && !settings.enabled {
        observed.skip(SkipReason::Disabled);
    } else if observed.facts.skip.is_none() && excluded {
        observed.skip(SkipReason::Excluded);
    }
    let command = args
        .first()
        .map(|arg| {
            std::path::Path::new(arg)
                .file_name()
                .unwrap_or(arg)
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| "external".into());
    let command = if command
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
        && command.len() <= 128
    {
        command
    } else {
        "external".into()
    };
    let mut semantic_compactor = None;
    let mut local_record = None;
    let mut view_failure = None;
    let result = runner::run_presented(
        args,
        runner::Options {
            raw: raw || !settings.enabled || excluded,
            capture,
        },
        |bytes, stderr| {
            if let Some(view) = view {
                return match views::render(view, bytes) {
                    Ok(bytes) => Some(runner::Presentation::Bytes(bytes)),
                    Err(error) => {
                        view_failure = Some(crate::observe::failure(Phase::Render, &error));
                        None
                    }
                };
            }
            let text = std::str::from_utf8(bytes).ok()?;
            let proposal = command_view::candidate(args, text, stderr)?;
            let compactor = semantic_compactor
                .get_or_insert_with(Compactor::new)
                .as_ref()
                .ok()?;
            let original = compactor.compact(text);
            let selected = compactor.compact(&proposal);
            if selected.output_tokens >= original.output_tokens {
                return Some(runner::Presentation::Compacted(original));
            }
            Some(runner::Presentation::Semantic {
                bytes: selected.text.into_bytes(),
                tokens: (original.input_tokens, selected.output_tokens),
            })
        },
        |observation| {
            let (
                Some(stdout_bytes),
                Some(stderr_bytes),
                Some(stdout_emitted),
                Some(stderr_emitted),
            ) = (
                observation.stdout.read_bytes,
                observation.stderr.read_bytes,
                observation.stdout.emitted_bytes,
                observation.stderr.emitted_bytes,
            )
            else {
                return;
            };
            let semantic = observation.stdout.presented_tokens.is_some()
                || observation.stderr.presented_tokens.is_some();
            let counts = |stream: &runner::StreamObservation<'_>| {
                stream
                    .presented_tokens
                    .map(|(input, output)| (input as u64, output as u64))
                    .or_else(|| {
                        stream
                            .compacted
                            .map(|r| (r.input_tokens as u64, r.output_tokens as u64))
                    })
                    .or_else(|| (stream.read_bytes == Some(0)).then_some((0, 0)))
                    .or_else(|| {
                        // A semantic proposal already initialized the shared
                        // tokenizer. Count the other complete raw stream too,
                        // without charging startup to ordinary tiny passthrough.
                        if !semantic || view.is_some() {
                            return None;
                        }
                        let text = std::str::from_utf8(stream.original?).ok()?;
                        let tokens = Compactor::new().ok()?.count_tokens(text) as u64;
                        Some((tokens, tokens))
                    })
            };
            let tokens = counts(&observation.stdout)
                .zip(counts(&observation.stderr))
                .map(|(out, err)| (out.0 + err.0, out.1 + err.1));
            let event = state::Event {
                unix_millis: state::unix_millis(),
                command,
                input_tokens: tokens.map(|t| t.0),
                output_tokens: tokens.map(|t| t.1),
                input_bytes: stdout_bytes + stderr_bytes,
                output_bytes: stdout_emitted + stderr_emitted,
                duration_ms: observation.duration.as_millis().min(u64::MAX as u128) as u64,
                exit_code: Some(observation.status),
                source: Some(
                    if view.is_some() {
                        "view"
                    } else if semantic {
                        "run-view"
                    } else {
                        "run"
                    }
                    .into(),
                ),
                original_id: None,
            };
            let originals = observation.stdout.original.zip(observation.stderr.original);
            // Usage storage is optional and cannot turn a successful command into a failure.
            local_record = Some(
                state::record_observed(event, originals)
                    .unwrap_or(crate::observation::LocalRecordOutcome::Unavailable),
            );
        },
        observed,
    );
    observed.facts.local_record = local_record;
    if result.is_ok() && observed.facts.failure.is_none() && view_failure.is_some() {
        observed.facts.failure = view_failure;
        observed.facts.outcome = Outcome::FailOpen;
    }
    result
}
