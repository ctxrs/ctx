//! Terminal facts are tested against the actual writer boundary and product output.
use crate::{
    observation::*,
    observe::{self, Observed},
};
use serde_json::{Value, json};
use std::io::{self, Cursor, Write};

fn invariant(facts: &SiftObservation) {
    for stream in facts.streams.iter().flatten() {
        if stream.tokens.is_some() {
            assert!(stream.input_complete && stream.output_complete, "{facts:?}");
            assert_eq!(stream.missing, None);
        }
    }
    assert!(!format!("{facts:?}").contains("PRIVATE_CANARY"));
}

// Runs inside the existing isolated entry-point subprocess, after its callback.
pub(crate) fn check_invocation(case: &str, status: i32, observations: &[SiftObservation]) {
    assert_eq!(observations.len(), 1, "one terminal per invocation");
    let facts = &observations[0];
    invariant(facts);
    assert_eq!(facts.terminal, Terminal::Invocation);
    match case {
        "compact" => {
            assert_eq!(status, 0);
            assert_eq!(facts.operation, Operation::Compact);
            let stream = facts.streams[0].unwrap();
            assert_eq!(stream.input_bytes, Some(6));
            assert_eq!(stream.emitted_bytes, Some(6));
            assert_eq!(
                stream.tokens.unwrap(),
                TokenCounts {
                    input: 2,
                    emitted: 2
                }
            );
            assert_eq!(facts.delivery, Delivery::Flushed);
        }
        "restore" => {
            assert_eq!(status, 0);
            assert_eq!(facts.mode, Mode::Restore);
            let stream = facts.streams[0].unwrap();
            assert_eq!(stream.input_bytes, Some(3));
            assert_eq!(stream.emitted_bytes, Some(3));
            assert!(stream.output_complete);
            assert_eq!(stream.tokens, None);
            assert_eq!(stream.presentation, Presentation::Restored);
        }
        "view" => {
            assert_eq!(status, 0);
            assert_eq!(facts.mode, Mode::ExplicitView);
            let stream = facts.streams[0].unwrap();
            assert_eq!(stream.input_bytes, Some(4));
            assert_eq!(stream.emitted_bytes, Some(2));
            assert_eq!(stream.missing, Some(Missingness::ViewNotTokenized));
            assert!(stream.output_complete);
        }
        "nonzero" => {
            assert_eq!(status, 7);
            assert_eq!(facts.child, ChildOutcome::ExitedNonzero);
            assert_eq!(facts.failure, None);
            assert_eq!(facts.outcome, Outcome::Success);
            for (index, n) in [3, 4].into_iter().enumerate() {
                let stream = facts.streams[index].unwrap();
                assert_eq!(stream.input_bytes, Some(n));
                assert_eq!(stream.emitted_bytes, Some(n));
                assert!(stream.output_complete);
                assert_eq!(stream.missing, Some(Missingness::Small));
                assert_eq!(stream.tokens, None);
            }
        }
        "raw" => {
            assert_eq!(status, 7);
            assert_eq!(facts.child, ChildOutcome::ExitedNonzero);
            assert_eq!(facts.failure, None);
            assert_eq!(facts.skip, Some(SkipReason::ExplicitRaw));
            for stream in facts.streams.iter().flatten() {
                assert_eq!(stream.input_bytes, None);
                assert_eq!(stream.emitted_bytes, None);
                assert_eq!(stream.tokens, None);
                assert_eq!(stream.missing, Some(Missingness::Inherited));
            }
        }
        "spawn" => {
            assert_eq!(status, 127);
            assert_eq!(facts.child, ChildOutcome::SpawnNotFound);
            assert_eq!(facts.failure.unwrap().phase, Phase::Spawn);
            assert_eq!(facts.outcome, Outcome::Failure);
        }
        "hook-small" | "hook-disabled" | "hook-malformed" | "hook-settings" => {
            assert_eq!(status, 0);
            assert_eq!(facts.entry, Entry::CompletionHook);
            assert_eq!(facts.host, Some(Host::Claude));
            assert_eq!(facts.delivery, Delivery::Unchanged);
            assert_eq!(
                facts.skip,
                Some(match case {
                    "hook-small" => SkipReason::Small,
                    "hook-disabled" => SkipReason::Disabled,
                    "hook-settings" => SkipReason::SettingsUnavailable,
                    _ => SkipReason::MalformedInput,
                })
            );
            assert!(facts.streams.iter().flatten().all(|s| s.tokens.is_none()));
            if matches!(case, "hook-malformed" | "hook-settings") {
                assert_eq!(facts.outcome, Outcome::FailOpen);
            }
        }
        "prehook" => {
            assert_eq!(status, 0);
            assert_eq!(facts.entry, Entry::PreHook);
            assert_eq!(facts.mode, Mode::Rewrite);
            assert_eq!(facts.delivery, Delivery::Flushed);
            assert_eq!(facts.streams, [None, None]);
        }
        "filter-disabled" => {
            assert_eq!(status, 0);
            assert_eq!(facts.skip, Some(SkipReason::Disabled));
            let stream = facts.streams[0].unwrap();
            assert_eq!(stream.emitted_bytes, Some(6));
            assert_eq!(stream.tokens, None);
            assert!(stream.output_complete);
        }
        "help" => {
            assert_eq!(status, 0);
            assert_eq!(facts.operation, Operation::Help);
            assert_eq!(facts.streams, [None, None]);
        }
        "invalid" => {
            assert_eq!(status, 1);
            assert_eq!(facts.failure.unwrap().phase, Phase::Arguments);
            assert_eq!(facts.operation, Operation::Unknown);
            assert_eq!(facts.streams, [None, None]);
        }
        _ => panic!("unknown test case"),
    }
}

fn invocation(
    case: &str,
    args: &[&str],
    input: &[u8],
    settings: Option<Value>,
) -> std::process::Output {
    use std::process::Stdio;
    let root = tempfile::tempdir().unwrap();
    if let Some(settings) = settings {
        std::fs::create_dir(root.path().join("config")).unwrap();
        std::fs::write(root.path().join("config/config.json"), settings.to_string()).unwrap();
    }
    let mut child = crate::test_support::command(args)
        .current_dir(root.path())
        .env("CTX_OUTPUT_CONFIG_DIR", root.path().join("config"))
        .env("CTX_OUTPUT_STATE_DIR", root.path().join("state"))
        .env("CTX_SIFT_OBSERVATION_CASE", case)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    crate::test_support::clean(child.wait_with_output().unwrap())
}

#[test]
fn direct_routes_observe_once_and_preserve_bytes() {
    for (case, args, input, expected) in [
        (
            "compact",
            vec!["compact"],
            b"hello\n".as_slice(),
            b"hello\n".as_slice(),
        ),
        (
            "restore",
            vec!["restore", "--encoding", "raw"],
            &[0xff, 0, 10],
            &[0xff, 0, 10],
        ),
        (
            "view",
            vec!["read", "--lines", "1"],
            b"a\nb\n".as_slice(),
            b"a\n".as_slice(),
        ),
    ] {
        let output = invocation(case, &args, input, None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, expected);
    }
    assert!(invocation("help", &["--help"], b"", None).status.success());
    assert_eq!(
        invocation("invalid", &["PRIVATE_CANARY"], b"", None)
            .status
            .code(),
        Some(1)
    );
}

#[cfg(unix)]
#[test]
fn child_failure_is_not_filter_failure_and_raw_stays_unmeasured() {
    for (case, flag) in [("nonzero", "--capture"), ("raw", "--raw")] {
        let output = invocation(
            case,
            &[
                "run",
                flag,
                "--",
                "sh",
                "-c",
                "printf out; printf warn >&2; exit 7",
            ],
            b"",
            None,
        );
        assert_eq!(
            output.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"warn");
    }
    let output = invocation(
        "spawn",
        &["run", "--capture", "--", "./PRIVATE_CANARY-missing"],
        b"",
        None,
    );
    assert_eq!(
        output.status.code(),
        Some(127),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hooks_and_disabled_filter_preserve_fail_open_contract() {
    let small = json!({"hook_event_name":"PostToolUse","tool_name":"Bash","tool_response":{"stdout":"PRIVATE_CANARY"}}).to_string();
    for (case, input, settings) in [
        ("hook-small", small.as_bytes(), None),
        (
            "hook-disabled",
            small.as_bytes(),
            Some(json!({"enabled":false})),
        ),
        ("hook-malformed", b"PRIVATE_CANARY {".as_slice(), None),
        (
            "hook-settings",
            small.as_bytes(),
            Some(json!({"enabled":"invalid"})),
        ),
    ] {
        let output = invocation(case, &["hook", "claude"], input, settings);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"{}\n");
    }
    let output = invocation(
        "filter-disabled",
        &["filter"],
        b"hello\n",
        Some(json!({"enabled":false})),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"hello\n");
    let input = json!({"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git status", "shell":"bash"}}).to_string();
    let output = invocation("prehook", &["hook", "codex"], input.as_bytes(), None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("updatedInput")
    );
}

struct BrokenWriter {
    left: usize,
    fail_flush: bool,
}
impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let n = bytes.len().min(self.left);
        self.left -= n;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn partial_write_and_failed_flush_do_not_claim_comparable_savings() {
    for (left, flush, accepted) in [(3, false, 3), (usize::MAX, true, 6)] {
        let mut stream = StreamFacts {
            tokens: Some(TokenCounts {
                input: 9,
                emitted: 2,
            }),
            ..Default::default()
        };
        let error = observe::write_stream(
            &mut BrokenWriter {
                left,
                fail_flush: flush,
            },
            b"hello\n",
            &mut stream,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(stream.emitted_bytes, Some(accepted));
        assert!(!stream.output_complete);
        assert_eq!(stream.tokens, None);
        assert_eq!(stream.missing, Some(Missingness::Incomplete));
    }
}

#[test]
fn filter_small_binary_complete_and_partial_facts_follow_output() {
    for bytes in [
        b"tiny".to_vec(),
        vec![0xff; 300],
        b"alpha beta gamma\n".repeat(300),
    ] {
        let mut output = Vec::new();
        let mut observed = Observed::new();
        crate::filter::filter_observed(
            Cursor::new(bytes.clone()),
            &mut output,
            true,
            &mut observed,
        )
        .unwrap();
        let stream = observed.facts.streams[0].unwrap();
        assert_eq!(stream.input_bytes, Some(bytes.len() as u64));
        assert_eq!(stream.emitted_bytes, Some(output.len() as u64));
        assert!(stream.input_complete && stream.output_complete);
        if bytes.len() == 4 {
            assert_eq!(stream.missing, Some(Missingness::Small));
            assert_eq!(output, bytes);
        } else if bytes[0] == 0xff {
            assert_eq!(stream.missing, Some(Missingness::Binary));
            assert_eq!(output, bytes);
        } else {
            let tokenizer = tiktoken_rs::o200k_base().unwrap();
            assert_eq!(
                stream.tokens.unwrap(),
                TokenCounts {
                    input: tokenizer
                        .encode_ordinary(std::str::from_utf8(&bytes).unwrap())
                        .len() as u64,
                    emitted: tokenizer
                        .encode_ordinary(std::str::from_utf8(&output).unwrap())
                        .len() as u64,
                }
            );
        }
        invariant(&observed.facts);
    }
    let mut observed = Observed::new();
    assert!(
        crate::filter::filter_observed(
            Cursor::new(b"abcde"),
            BrokenWriter {
                left: 2,
                fail_flush: false
            },
            true,
            &mut observed
        )
        .is_err()
    );
    assert_eq!(observed.facts.streams[0].unwrap().emitted_bytes, Some(2));
    assert_eq!(observed.facts.streams[0].unwrap().tokens, None);
    assert_eq!(observed.phase, Phase::Output);
}

fn protocol(
    input: &str,
    output: impl Write,
    session: u8,
) -> (anyhow::Result<bool>, Vec<SiftObservation>) {
    let mut observed = Observed::new();
    observed.facts.operation = Operation::Compact;
    let mut facts = Vec::new();
    let result = crate::protocol::protocol(
        Cursor::new(input.as_bytes()),
        output,
        None,
        None,
        session,
        &mut observed,
        &mut |v| facts.push(v),
    );
    observed.finish(result.as_ref().err(), &mut |v| facts.push(v));
    for value in &facts {
        invariant(value);
    }
    (result, facts)
}

#[test]
fn json_requests_have_one_terminal_each_and_no_session_savings() {
    let text = "PRIVATE_CANARY alpha beta gamma\n".repeat(100);
    let input = format!(
        "{}\n{{bad\n{}\n",
        json!({"version":1,"text":text}),
        json!({"version":1,"text":"ok"})
    );
    let mut output = Vec::new();
    let (result, facts) = protocol(&input, &mut output, 0);
    assert!(result.unwrap());
    assert_eq!(facts.len(), 4);
    assert_eq!(facts[1].outcome, Outcome::Failure);
    assert_eq!(facts[1].failure.unwrap().phase, Phase::Protocol);
    assert_eq!(facts[1].streams, [None, None]);
    assert_eq!(facts[3].terminal, Terminal::ProtocolSession);
    assert_eq!(facts[3].streams, [None, None]);
    let responses: Vec<Value> = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(|v| serde_json::from_str(v).unwrap())
        .collect();
    let selected = responses[0]["text"].as_str().unwrap();
    let stream = facts[0].streams[0].unwrap();
    assert_eq!(facts[0].terminal, Terminal::ProtocolRequest);
    assert_eq!(facts[0].delivery, Delivery::Flushed);
    assert_eq!(stream.input_bytes, Some(text.len() as u64));
    assert_eq!(stream.emitted_bytes, Some(selected.len() as u64));
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert_eq!(
        stream.tokens.unwrap(),
        TokenCounts {
            input: tokenizer.encode_ordinary(&text).len() as u64,
            emitted: tokenizer.encode_ordinary(selected).len() as u64,
        }
    );
}

#[test]
fn protocol_response_flush_failure_keeps_counts_missing() {
    for (left, flush) in [(4, false), (usize::MAX, true)] {
        let (result, facts) = protocol(
            "{\"version\":1,\"text\":\"hello\"}\n",
            BrokenWriter {
                left,
                fail_flush: flush,
            },
            0,
        );
        assert!(result.is_err());
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].delivery, Delivery::Failed);
        assert_eq!(facts[0].failure.unwrap().kind, FailureKind::BrokenPipe);
        let stream = facts[0].streams[0].unwrap();
        assert_eq!(stream.emitted_bytes, None);
        assert_eq!(stream.tokens, None);
        assert!(!stream.output_complete);
    }
}

#[test]
fn pi_duplicate_request_has_separate_failure_without_recounting_output() {
    let request = json!({"id":1,"request":{"version":1,"text":"hello"}});
    let (result, facts) = protocol(&format!("{request}\n{request}\n"), Vec::new(), 1);
    assert!(result.is_err());
    assert_eq!(facts.len(), 3);
    assert_eq!(facts[0].entry, Entry::PiSessionV1);
    assert_eq!(facts[0].delivery, Delivery::Flushed);
    assert!(facts[0].streams[0].unwrap().tokens.is_some());
    assert_eq!(facts[1].outcome, Outcome::Failure);
    assert_eq!(facts[1].streams, [None, None]);
    assert_eq!(facts[2].terminal, Terminal::ProtocolSession);
    assert_eq!(facts[2].streams, [None, None]);
}

#[test]
fn prehook_decisions_never_fabricate_stream_savings() {
    let input = json!({"tool_name":"Bash","hook_event_name":"PreToolUse","tool_input":{"command":"git status","shell":"bash"}}).to_string();
    for (host, excludes, reason) in [
        ("codex", vec![], None),
        ("codex", vec!["Bash".to_owned()], Some(SkipReason::Excluded)),
        ("cursor", vec![], Some(SkipReason::UnsupportedHost)),
    ] {
        let mut facts = Observed::new();
        let output = crate::pre_hooks::transform_observed(
            host,
            &input,
            std::path::Path::new("/opt/ctx"),
            &excludes,
            &mut facts,
        )
        .unwrap();
        assert_eq!(output.is_none(), reason.is_some());
        assert_eq!(facts.facts.skip, reason);
        assert_eq!(facts.facts.streams, [None, None]);
    }
}

#[test]
fn pi_v2_selected_command_view_counts_the_response_not_the_baseline() {
    mod fixture {
        include!("tests/fixtures/pi_delivered.rs");
    }
    let (text, _) = fixture::cargo_fixture(true, "");
    let input = format!(
        "{}\n",
        json!({"id":1,"tool":"bash","command":"cargo test","delivered_view":true,
        "request":{"version":1,"text":text,"complete":false,"is_error":true}})
    );
    let mut output = Vec::new();
    let (result, facts) = protocol(&input, &mut output, 2);
    assert!(!result.unwrap());
    assert_eq!(facts.len(), 2);
    let lines: Vec<Value> = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let emitted = lines[1]["text"].as_str().unwrap();
    assert_eq!(lines[1]["semantic"], true);
    let stream = facts[0].streams[0].unwrap();
    assert_eq!(facts[0].entry, Entry::PiSessionV2);
    assert_eq!(stream.presentation, Presentation::CommandView);
    assert_eq!(stream.emitted_bytes, Some(emitted.len() as u64));
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert_eq!(
        stream.tokens.unwrap(),
        TokenCounts {
            input: tokenizer.encode_ordinary(&text).len() as u64,
            emitted: tokenizer.encode_ordinary(emitted).len() as u64,
        }
    );
    assert_eq!(facts[0].child, ChildOutcome::NotApplicable);
    assert_eq!(facts[0].semantic, None);
}

#[test]
fn pi_done_failure_does_not_erase_an_already_flushed_response() {
    struct DoneFailure {
        flushes: usize,
    }
    impl Write for DoneFailure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes == 3 {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(())
            }
        }
    }
    let input = format!(
        "{}\n",
        json!({"id":1,"request":{"version":1,"text":"hello"}})
    );
    let (result, facts) = protocol(&input, DoneFailure { flushes: 0 }, 1);
    assert!(result.is_err());
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0].delivery, Delivery::Flushed);
    assert_eq!(facts[0].outcome, Outcome::Failure);
    assert_eq!(facts[0].failure.unwrap().phase, Phase::Protocol);
    assert!(facts[0].streams[0].unwrap().output_complete);
    assert!(facts[0].streams[0].unwrap().tokens.is_some());
    assert_eq!(facts[1].streams, [None, None]);
}

#[test]
fn filter_capture_limit_is_raw_with_unknown_tokens() {
    let bytes = vec![b'x'; 8 * 1024 * 1024 + 1];
    let mut output = Vec::new();
    let mut observed = Observed::new();
    crate::filter::filter_observed(Cursor::new(bytes.clone()), &mut output, true, &mut observed)
        .unwrap();
    assert_eq!(output, bytes);
    assert_eq!(observed.facts.skip, Some(SkipReason::CaptureLimit));
    let stream = observed.facts.streams[0].unwrap();
    assert_eq!(stream.input_bytes, Some(bytes.len() as u64));
    assert_eq!(stream.emitted_bytes, Some(bytes.len() as u64));
    assert_eq!(stream.tokens, None);
    assert_eq!(stream.missing, Some(Missingness::Streaming));
    assert!(stream.input_complete && stream.output_complete);
}
