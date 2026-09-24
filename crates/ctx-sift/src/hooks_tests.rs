// Adapted from Sift tests, MIT; see this crate’s NOTICE.
// Exercise the adapter independently of concurrent CLI/setup integration.
use crate::command_view;
use crate::hooks;
use crate::state;

use serde_json::value::RawValue;
use sift::{Encoding, restore};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

type Object = BTreeMap<String, Box<RawValue>>;

fn object(text: &str) -> Object {
    serde_json::from_str(text).unwrap()
}
fn string(value: &RawValue) -> String {
    serde_json::from_str(value.get()).unwrap()
}
fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}
fn log() -> String {
    "worker α: completed checkpoint 🦀; status remains unchanged\r\n".repeat(100)
}
fn assert_reversible(original: &str, changed: &str) {
    assert_ne!(original, changed);
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert!(tokenizer.encode_ordinary(changed).len() < tokenizer.encode_ordinary(original).len());
    assert!(
        [
            Encoding::TextRunsV1,
            Encoding::TextPrefixesV1,
            Encoding::TextRefsV1,
            Encoding::JsonV1,
            Encoding::JsonRowsV1,
        ]
        .into_iter()
        .any(|encoding| restore(encoding, changed).is_ok_and(|text| text == original))
    );
}
fn claude(response: &str) -> String {
    format!(r#"{{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_response":{response}}}"#)
}

fn claude_command(response: &str, command: &str) -> String {
    format!(
        r#"{{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{{"command":{},"timeout":123,"opaque":1e9999}},"tool_response":{response}}}"#,
        json_string(command)
    )
}

fn completion_command(host: &str, command: Option<&str>) -> String {
    let args = command.map_or_else(
        || "{}".to_owned(),
        |command| {
            format!(
                r#"{{"command":{},"opaque":1.2300e+9999}}"#,
                json_string(command)
            )
        },
    );
    let text = json_string(&log());
    match host {
        "claude" => format!(
            r#"{{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{args},"tool_response":{{"stdout":{text},"stderr":{text},"interrupted":false,"isImage":false,"noOutputExpected":false,"opaque":-0.0100e+9999}}}}"#
        ),
        "copilot" => format!(
            r#"{{"toolName":"bash","toolArgs":{},"toolResult":{{"resultType":"success","textResultForLlm":{text},"opaque":-0.0100e+9999}}}}"#,
            json_string(&args)
        ),
        "hermes" => format!(
            r#"{{"hook_event_name":"TransformToolResult","tool_name":"terminal","tool_input":{args},"tool_response":{{"output":{text},"exit_code":17,"error":null,"opaque":-0.0100e+9999}}}}"#
        ),
        _ => unreachable!(),
    }
}

#[test]
fn literal_output_wrappers_skip_entire_completion() {
    for host in ["claude", "copilot"] {
        assert!(
            hooks::transform(host, &completion_command(host, None))
                .unwrap()
                .is_some()
        );
        for command in [
            "ctx sift -- cat file",
            "ctx sift run --raw -- cat file",
            "sift proxy cat file",
            "sift run cat file",
            "sift run --capture -- cat file",
            "sift run -- cat --raw",
            "sift run cat --raw",
            "sift run -- --raw cat",
            "ctx run -- cat file",
            "ctx --color never output run --capture -- cat file",
            "sift proxy --capture -- cat file",
            "sift run --raw cat file",
            "sift run --capture --raw --capture -- cat file",
            "sift run --raw --raw -- cat file",
            "sift run --raw -- --help",
            "command sift proxy -- cat file",
            "'sift' 'run' '--raw' -- cat 'a|b;$(literal)'",
            "\"/opt/a folder/sift.exe\" run --raw -- cat \"a;|b\"",
            "'/opt/$(literal);path/sift' proxy cat 'file*?'",
        ] {
            let input = completion_command(host, Some(command));
            assert!(
                hooks::transform(host, &input).unwrap().is_none(),
                "{host}: {command}"
            );
        }
    }
}

#[test]
fn lookalikes_and_unknown_commands_keep_existing_compaction() {
    for host in ["claude", "copilot"] {
        let expected = hooks::transform(host, &completion_command(host, None)).unwrap();
        assert!(expected.is_some());
        for command in [
            "sift run --future --raw cat",
            "sift run --rawish cat",
            "sift run --raw --help",
            "sift proxy -h",
            "sift proxy",
            "sift run --raw --",
            "sift --raw run cat",
            "sift raw cat",
            "my-sift run --raw cat",
            "sift-wrapper proxy cat",
            "sift.exe.bak proxy cat",
            "echo sift proxy cat",
            "printf 'sift run --raw'",
            "command -v sift",
            "command -p sift proxy cat",
            "env sift proxy cat",
            "MODE=raw sift proxy cat",
            "sift proxy cat; echo done",
            "sift run --raw cat && echo done",
            "sift proxy cat | cat",
            "sift proxy cat\ncat file",
            "sift proxy cat > file",
            "sift proxy $FILE",
            "sift proxy \"$FILE\"",
            "sift proxy $(echo cat)",
            "sift proxy `echo cat`",
            "sift proxy cat *.rs",
            "sift proxy cat ~",
            "sift proxy 'unfinished",
            r#""re\tok" proxy cat"#,
            "sift\rproxy cat",
            "sift proxy cat 'bad\0arg'",
            "sift proxy cat # comment",
            "",
        ] {
            let actual = hooks::transform(host, &completion_command(host, Some(command))).unwrap();
            assert_eq!(actual, expected, "{host}: {command:?}");
        }
    }
}

#[test]
fn wrapper_detection_does_not_guess_missing_or_malformed_arguments() {
    for host in ["claude", "copilot"] {
        let input = completion_command(host, Some("sift proxy cat file"));
        let expected = hooks::transform(host, &completion_command(host, None)).unwrap();
        let field = if host == "copilot" {
            "toolArgs"
        } else {
            "tool_input"
        };
        for args in [
            "null",
            "[]",
            "true",
            "17",
            r#"{"command":null}"#,
            r#"{"command":17}"#,
            r#"{"command":"sift proxy cat","command":"cat"}"#,
        ] {
            let mut root = object(&input);
            let args = if host == "copilot" {
                json_string(args)
            } else {
                args.to_owned()
            };
            root.insert(field.into(), RawValue::from_string(args).unwrap());
            assert_eq!(
                hooks::transform(host, &serde_json::to_string(&root).unwrap()).unwrap(),
                expected
            );
        }
        let mut root = object(&input);
        root.remove(field);
        assert_eq!(
            hooks::transform(host, &serde_json::to_string(&root).unwrap()).unwrap(),
            expected
        );
        // Copilot's established native schema is a JSON string, not an object.
        if host == "copilot" {
            root.insert(
                field.into(),
                RawValue::from_string(r#"{"command":"sift proxy cat"}"#.into()).unwrap(),
            );
            assert_eq!(
                hooks::transform(host, &serde_json::to_string(&root).unwrap()).unwrap(),
                expected
            );
            root.insert(
                field.into(),
                RawValue::from_string(json_string("{")).unwrap(),
            );
            assert_eq!(
                hooks::transform(host, &serde_json::to_string(&root).unwrap()).unwrap(),
                expected
            );
        }
        let (from, to) = if host == "claude" {
            ("\"Bash\"", "\"PowerShell\"")
        } else {
            ("\"bash\"", "\"powershell\"")
        };
        for command in [
            "ctx run --raw -- cat file",
            "ctx output run -- cat file",
            "& 'C:\\Program Files\\ctx.exe' run --raw -- cat file",
            "& 'C:\\User''s tools\\ctx.exe' output run --capture -- cat file",
        ] {
            let input = completion_command(host, Some(command)).replace(from, to);
            assert!(
                hooks::transform(host, &input).unwrap().is_none(),
                "{command}"
            );
        }
        let counterpart = completion_command(host, None).replace(from, to);
        let expected = hooks::transform(host, &counterpart).unwrap();
        assert!(expected.is_some());
        for command in [
            "cat file",
            "Write-Output 'ctx run --raw'",
            "& $program run --raw cat",
            "ctx run --raw cat; Get-Date",
            "ctx run --raw cat | Measure-Object",
            "ctx run --raw `cat",
            "ctx --future run --raw cat",
        ] {
            let input = completion_command(host, Some(command)).replace(from, to);
            assert_eq!(
                hooks::transform(host, &input).unwrap(),
                expected,
                "{command}"
            );
        }
    }
}

fn claude_output(output: &str) -> Object {
    let envelope = object(output);
    assert_eq!(envelope.len(), 1);
    let specific = object(envelope["hookSpecificOutput"].get());
    assert_eq!(specific.len(), 2);
    assert_eq!(string(&specific["hookEventName"]), "PostToolUse");
    object(specific["updatedToolOutput"].get())
}

const STATUS: &str = "On branch topic\nYour branch and 'origin/topic' have diverged,\nand have 2 and 3 different commits each, respectively.\n\nChanges to be committed:\n  (use \"git restore --staged <file>...\" to unstage)\n\n\trenamed:    \"old\\tname\" -> \"new\\nname\"\n\tmodified:   shared.rs\n\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n  (use \"git restore <file>...\" to discard changes in working directory)\n\n\tmodified:   shared.rs\n\tdeleted:    leading space \n\nUntracked files:\n  (use \"git add <file>...\" to include in what will be committed)\n\n\tunicodé.rs\n\t(use \"git add\" is a filename)\n\nUnmerged paths:\n  (use \"git add <file>...\" to mark resolution)\n\tboth modified:   conflict.rs\nUnknown advisory: index scan incomplete\n";

const STATUS_VIEW: &str = "## topic\nYour branch and 'origin/topic' have diverged,\nand have 2 and 3 different commits each, respectively.\n\nR  \"old\\tname\" -> \"new\\nname\"\nM  shared.rs\n M shared.rs\n D leading space \n?? unicodé.rs\n?? (use \"git add\" is a filename)\nUnmerged paths:\n  (use \"git add <file>...\" to mark resolution)\n\tboth modified:   conflict.rs\nUnknown advisory: index scan incomplete\n";

fn cargo_output(failed: bool) -> (String, String) {
    let header = if failed {
        "warning: synthetic build note\nrunning 10 tests\n"
    } else {
        "warning: synthetic build note\nrunning 9 tests\n"
    };
    let mut raw = header.to_owned();
    for name in [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
    ] {
        raw.push_str(&format!("test module::{name} ... ok\n"));
    }
    let tail = if failed {
        "test module::broken ... FAILED\ntest module::slow ... ignored, needs a server\n\nfailures:\n\n---- module::broken stdout ----\nthread 'module::broken' panicked at src/lib.rs:42:9:\nassertion `left == right` failed\n  left: [1, 2]\n right: [3, 4]\nstack backtrace:\n   0: crate::call\ntest printed_a_passing_looking_line ... ok\nunknown diagnostic payload\n\nfailures:\n    module::broken\n\ntest result: FAILED. 8 passed; 1 failed; 1 ignored; 0 measured; 7 filtered out; finished in 0.02s\n\n"
    } else {
        "test module::slow ... ignored, needs a server\n\ntest result: ok. 8 passed; 0 failed; 1 ignored; 0 measured; 7 filtered out; finished in 0.02s\n\n"
    };
    raw.push_str(tail);
    (
        raw,
        format!("{header}[8 passing test lines omitted]\n{tail}"),
    )
}

#[test]
fn claude_complete_bash_uses_existing_git_and_cargo_views() {
    let (cargo, cargo_view) = cargo_output(false);
    let compactor = sift::Compactor::new().unwrap();
    for (command, stdout, view, exit) in [
        ("git status", STATUS, STATUS_VIEW, 0),
        (
            "'/usr/bin/git' -C 'a folder' 'status' --long -- '--porcelain'",
            STATUS,
            STATUS_VIEW,
            0,
        ),
        (r#""git" -C "a folder" "status""#, STATUS, STATUS_VIEW, 0),
        (
            "cargo test --workspace -- --include-ignored",
            cargo.as_str(),
            cargo_view.as_str(),
            0,
        ),
    ] {
        // Expected facts are independently spelled out, including conflicts and
        // test counts; the existing core may encode this presentation.
        let expected = compactor.compact(view);
        assert!(expected.output_tokens < compactor.compact(stdout).output_tokens);
        let response = format!(
            r#"{{"stdout":{},"stderr":{},"interrupted":false,"isImage":false,"exitCode":{exit},"noOutputExpected":false,"numbers":[1e9999,-0,1.2300e+004,123456789012345678901234567890],"marker":{{"$serde_json::private::Number":"00123"}},"error":{{"code":-17,"message":"keep status"}}}}"#,
            json_string(stdout),
            json_string(stdout)
        );
        let input = claude_command(&response, command);
        let original_input = input.clone();
        let changed = claude_output(&hooks::transform("claude", &input).unwrap().unwrap());
        assert_eq!(input, original_input);
        assert_eq!(string(&changed["stdout"]), expected.text);
        assert_eq!(string(&changed["stderr"]), compactor.compact(stdout).text);
        assert_eq!(changed.len(), object(&response).len());
        for (key, value) in object(&response) {
            if key != "stdout" && key != "stderr" {
                assert_eq!(changed[&key].get(), value.get(), "opaque field {key}");
            }
        }
    }
}

#[test]
fn claude_semantic_gate_preserves_lossless_fallback_for_unsupported_commands() {
    let (cargo, _) = cargo_output(false);
    let compactor = sift::Compactor::new().unwrap();
    for (stdout, commands) in [
        (
            STATUS,
            vec![
                "git status --porcelain",
                "git status --porcelain=v2",
                "git status --short",
                "git status --format=json",
                "git -c status.short=true status",
                "git status --future-mode",
                "git status && git diff",
                "git status; git diff",
                "git status | cat",
                "git status\ngit diff",
                "git status > output",
                "git status -- $FILES",
                "git status -- \"$FILES\"",
                "git status -- *.rs",
                "git status -- $(echo file)",
                "git status -- `echo file`",
                "git status -- ~",
                "env git status",
                "GIT_DIR=other git status",
                "'git status'",
                r#""gi\t" status"#,
                r#"git "sta\tus""#,
                "git\rstatus",
                r#"git -C 'literal\path' status"#,
                "",
            ],
        ),
        (
            cargo.as_str(),
            vec![
                "cargo test --message-format=json",
                "cargo test -- --format json",
                "cargo test -- --nocapture",
                "cargo test -- --show-output",
                "cargo test -q",
                "cargo nextest run",
                "cargo test -- --custom-harness",
                "cargo test $FILTER",
            ],
        ),
    ] {
        let response = format!(
            r#"{{"stdout":{},"stderr":"","interrupted":false,"isImage":false,"exitCode":0}}"#,
            json_string(stdout)
        );
        for command in commands {
            let actual = hooks::transform("claude", &claude_command(&response, command)).unwrap();
            // Missing tool_input is the independently established lossless path.
            assert_eq!(
                actual,
                hooks::transform("claude", &claude(&response)).unwrap(),
                "{command}"
            );
            if let Some(output) = actual {
                assert_eq!(
                    string(&claude_output(&output)["stdout"]),
                    compactor.compact(stdout).text
                );
            }
        }
    }
}

#[test]
fn claude_equal_token_semantic_proposal_keeps_original_core_winner() {
    let stdout = "warning: synthetic build note; retain this entire diagnostic paragraph and the location src/lib.rs:42:9 for inspection\nrunning 2 tests\ntest alphabetalphabetalphabet ... ok\ntest skipped ... ignored, needs a server\n\ntest result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 7 filtered out; finished in 0.02s\n\n";
    let view = stdout.replacen(
        "test alphabetalphabetalphabet ... ok\n",
        "[1 passing test lines omitted]\n",
        1,
    );
    assert_eq!(
        command_view::candidate(&["cargo".into(), "test".into()], stdout, false),
        Some(view.clone())
    );
    let compactor = sift::Compactor::new().unwrap();
    let original = compactor.compact(stdout);
    let semantic = compactor.compact(&view);
    let reference = tiktoken_rs::o200k_base().unwrap();
    assert_ne!(original.text, semantic.text);
    assert_eq!(original.output_tokens, semantic.output_tokens);
    assert_eq!(
        reference.encode_ordinary(&original.text).len(),
        reference.encode_ordinary(&semantic.text).len()
    );
    // A separate changed stream makes the preserved stdout directly observable.
    let response = format!(
        r#"{{"stdout":{},"stderr":{},"interrupted":false,"isImage":false,"exitCode":0}}"#,
        json_string(stdout),
        json_string(&log())
    );
    let output = hooks::transform("claude", &claude_command(&response, "cargo test"))
        .unwrap()
        .unwrap();
    let changed = claude_output(&output);
    assert_eq!(string(&changed["stdout"]), original.text);
    assert_eq!(string(&changed["stderr"]), compactor.compact(&log()).text);
}

#[test]
fn claude_semantic_gate_requires_explicit_complete_bash_metadata() {
    let base = serde_json::json!({"stdout": STATUS, "stderr":"", "interrupted":false, "isImage":false, "exitCode":0});
    for field in ["interrupted", "isImage"] {
        let mut response = base.clone();
        response.as_object_mut().unwrap().remove(field);
        let response = response.to_string();
        assert_eq!(
            hooks::transform("claude", &claude_command(&response, "git status")).unwrap(),
            hooks::transform("claude", &claude(&response)).unwrap(),
            "missing {field}"
        );
    }
    let (cargo, _) = cargo_output(false);
    let (failed_cargo, _) = cargo_output(true);
    for (command, stdout) in [
        ("git status", STATUS),
        ("cargo test", cargo.as_str()),
        ("cargo test", failed_cargo.as_str()),
    ] {
        // Nonzero status contradicts this success-only event. Preserve it and
        // use only lossless compaction, including for defensive failure inputs.
        for exit in [
            "null",
            "true",
            "\"0\"",
            "1.0",
            "1e0",
            "2147483648",
            "-2147483649",
            "1e9999",
            "1",
            "101",
            "-17",
            "-2147483648",
            "2147483647",
        ] {
            let response = format!(
                r#"{{"stdout":{},"interrupted":false,"isImage":false,"exitCode":{exit},"noOutputExpected":false}}"#,
                json_string(stdout)
            );
            let actual = hooks::transform("claude", &claude_command(&response, command)).unwrap();
            assert_eq!(
                actual,
                hooks::transform("claude", &claude(&response)).unwrap(),
                "{command}, exit {exit}"
            );
            if let Some(output) = actual {
                let changed = claude_output(&output);
                assert_eq!(changed["exitCode"].get(), exit);
                assert_eq!(changed["noOutputExpected"].get(), "false");
                assert_eq!(
                    string(&changed["stdout"]),
                    sift::Compactor::new().unwrap().compact(stdout).text
                );
            }
        }
    }
    for flag in ["interrupted", "isImage"] {
        for value in [
            serde_json::json!(true),
            serde_json::json!("false"),
            serde_json::Value::Null,
        ] {
            let mut response = base.clone();
            response[flag] = value;
            assert!(
                hooks::transform(
                    "claude",
                    &claude_command(&response.to_string(), "git status")
                )
                .unwrap()
                .is_none()
            );
        }
    }
    let response = base.to_string();
    let input = claude_command(&response, "git status").replace("\"Bash\"", "\"PowerShell\"");
    let expected = claude(&response).replace("\"Bash\"", "\"PowerShell\"");
    assert_eq!(
        hooks::transform("claude", &input).unwrap(),
        hooks::transform("claude", &expected).unwrap()
    );
    for tool_input in [
        "null",
        "{}",
        r#"{"command":null}"#,
        r#"{"command":17}"#,
        r#"{"command":"git status","command":"cargo test"}"#,
    ] {
        let input = claude(&response).replacen('{', &format!("{{\"tool_input\":{tool_input},"), 1);
        assert_eq!(
            hooks::transform("claude", &input).unwrap(),
            hooks::transform("claude", &claude(&response)).unwrap()
        );
    }
    for host in ["copilot", "hermes"] {
        let input = if host == "copilot" {
            serde_json::json!({"toolName":"bash", "toolArgs":{"command":"git status"},
                "toolResult":{"resultType":"success", "textResultForLlm":STATUS,
                              "interrupted":false, "isImage":false, "exitCode":0}})
            .to_string()
        } else {
            serde_json::json!({"hook_event_name":"TransformToolResult", "tool_name":"terminal",
                "tool_input":{"command":"git status"}, "tool_response":{"output":STATUS,
                              "interrupted":false, "isImage":false, "exitCode":0}})
            .to_string()
        };
        let mut without: serde_json::Value = serde_json::from_str(&input).unwrap();
        without
            .as_object_mut()
            .unwrap()
            .remove(if host == "copilot" {
                "toolArgs"
            } else {
                "tool_input"
            });
        assert_eq!(
            hooks::transform(host, &input).unwrap(),
            hooks::transform(host, &without.to_string()).unwrap()
        );
    }
}

#[test]
fn claude_preserves_opaque_metadata_and_both_streams() {
    let stdout = log();
    let stderr = "warning: retaining diagnostic context\r\n".repeat(80);
    let response = format!(
        r#"{{"stdout":{},"stderr":{},"isImage":false,"interrupted":false,"exitCode":17,"error":{{"message":"failed 🦀\r\n","code":-9}},"marker":{{"$serde_json::private::Number":"00123"}},"numbers":[1234567890123456789012345678901234567890,1.2300e+004,-0,1e9999],"content":[{{"type":"image","data":"opaque=="}}]}}"#,
        json_string(&stdout),
        json_string(&stderr)
    );
    let output = hooks::transform("claude", &claude(&response))
        .unwrap()
        .unwrap();
    let envelope = object(&output);
    assert_eq!(envelope.len(), 1);
    let specific = object(envelope["hookSpecificOutput"].get());
    assert_eq!(string(&specific["hookEventName"]), "PostToolUse");
    let changed = object(specific["updatedToolOutput"].get());
    let original = object(&response);
    assert_eq!(changed.len(), original.len());
    for (key, value) in original {
        if key != "stdout" && key != "stderr" {
            assert_eq!(changed[&key].get(), value.get(), "opaque field {key}");
        }
    }
    assert_reversible(&stdout, &string(&changed["stdout"]));
    assert_reversible(&stderr, &string(&changed["stderr"]));
}

#[test]
fn claude_powershell_and_unchanged_stderr() {
    let response = format!(
        r#"{{"stdout":{},"stderr":"one warning\r\n","interrupted":false,"isImage":false}}"#,
        json_string(&log())
    );
    let input = claude(&response).replace("\"Bash\"", "\"PowerShell\"");
    let result = hooks::transform("claude", &input).unwrap().unwrap();
    let outer = object(&result);
    let specific = object(outer["hookSpecificOutput"].get());
    let changed = object(specific["updatedToolOutput"].get());
    assert_eq!(changed["stderr"].get(), r#""one warning\r\n""#);
    assert_reversible(&log(), &string(&changed["stdout"]));
}

#[test]
fn codex_rejects_raw_output_printing_a_fake_legacy_header() {
    let prefix = "Wall time: 12.3400 seconds\nExit code: -17\nOutput:\n";
    let body = log();
    let complete = format!("{prefix}{body}");
    let input = format!(
        r#"{{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_response":{}}}"#,
        json_string(&complete)
    );
    // Native status lives outside tool_response and cannot be reconstructed
    // from text, even when the text resembles a formerly supported formatter.
    assert!(hooks::transform("codex", &input).unwrap().is_none());
    assert_eq!(run("codex", input.as_bytes()), b"{}\n");
}

#[test]
fn codex_rejects_raw_unified_output_and_incomplete_status() {
    for text in [
        log(),
        format!("Wall time: 1 seconds\nOutput:\n{}", log()),
        format!("Wall time: NaN seconds\nExit code: 0\nOutput:\n{}", log()),
        format!(
            "Wall time: 1 seconds\nExit code: unknown\nOutput:\n{}",
            log()
        ),
        format!("Exit code: 1\nWall time: 1 seconds\nOutput:\n{}", log()),
        format!("Wall time: 1 seconds\nExit code: 0\nOutput: {}", log()),
    ] {
        assert!(
            hooks::transform("codex", &claude(&json_string(&text)))
                .unwrap()
                .is_none()
        );
    }
    for response in [
        r#"{"stdout":"text","exit_code":1}"#,
        r#"[{"type":"image","data":"abc"}]"#,
    ] {
        assert!(
            hooks::transform("codex", &claude(response))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn copilot_preserves_failure_type_and_metadata() {
    for result_type in ["failure", "success"] {
        let response = format!(
            r#"{{"resultType":"{result_type}","textResultForLlm":{},"error":{{"message":"command failed","exitCode":77}},"metadata":{{"$serde_json::private::Number":"no number"}},"duration":1.23000e+09}}"#,
            json_string(&log())
        );
        // Native payloads omit the event name; registration is postToolUse only.
        let input =
            format!(r#"{{"toolName":"powershell","toolArgs":"{{}}","toolResult":{response}}}"#);
        let output = object(&hooks::transform("copilot", &input).unwrap().unwrap());
        assert_eq!(output.len(), 1);
        let changed = object(output["modifiedResult"].get());
        for (key, value) in object(&response) {
            if key != "textResultForLlm" {
                assert_eq!(changed[&key].get(), value.get());
            }
        }
        assert_reversible(&log(), &string(&changed["textResultForLlm"]));
        let wrong_event = input.replacen('{', "{\"event\":\"preToolUse\",", 1);
        assert!(hooks::transform("copilot", &wrong_event).unwrap().is_none());
    }
}

#[test]
fn hermes_final_result_preserves_status_and_exact_metadata() {
    let text = log();
    let response = format!(
        r#"{{"output":{},"exit_code":17,"error":null,"hint":"retain hint","approval":"already approved","count":123456789012345678901234567890,"number":-0.00100E+99999,"marker":{{"$serde_json::private::Number":"user object"}}}}"#,
        json_string(&text)
    );
    let input = format!(
        r#"{{"hook_event_name":"TransformToolResult","tool_name":"terminal","tool_response":{response}}}"#
    );
    let output = hooks::transform("hermes", &input).unwrap().unwrap();
    let envelope = object(&output);
    assert_eq!(envelope.len(), 1);
    let result = string(&envelope["result"]);
    let changed = object(&result);
    for (key, value) in object(&response) {
        if key != "output" {
            assert_eq!(changed[&key].get(), value.get());
        }
    }
    assert_reversible(&text, &string(&changed["output"]));
    assert_eq!(
        run("hermes", input.as_bytes()),
        format!("{output}\n").as_bytes()
    );
    assert!(
        hooks::transform(
            "hermes",
            &input.replace("TransformToolResult", "PreToolUse")
        )
        .unwrap()
        .is_none()
    );
    assert!(
        hooks::transform("hermes", &input.replace("\"terminal\"", "\"read_file\""))
            .unwrap()
            .is_none()
    );
}

#[test]
fn malformed_unsupported_and_tiny_are_noops() {
    for host in ["claude", "codex", "copilot", "unknown"] {
        for input in ["", "{", "{}", "null", "[]", "{}\n{}", r#"{"x":1,"x":2}"#] {
            assert!(hooks::transform(host, input).unwrap().is_none());
        }
    }
    for response in [
        r#"{"stdout":"tiny","stderr":""}"#.to_owned(),
        format!(
            r#"{{"stdout":{},"stderr":[],"isImage":false}}"#,
            json_string(&log())
        ),
        format!(r#"{{"stdout":{},"interrupted":true}}"#, json_string(&log())),
        format!(r#"{{"stdout":{},"isImage":true}}"#, json_string(&log())),
        format!(r#"{{"stdout":{},"isImage":"false"}}"#, json_string(&log())),
        format!(r#"{{"stdout":{},"stdout":"other"}}"#, json_string(&log())),
    ] {
        assert!(
            hooks::transform("claude", &claude(&response))
                .unwrap()
                .is_none()
        );
    }
    let input = claude(&format!(r#"{{"stdout":{}}}"#, json_string(&log())));
    assert!(
        hooks::transform("claude", &input.replace("PostToolUse", "PreToolUse"))
            .unwrap()
            .is_none()
    );
    assert!(
        hooks::transform("claude", &input.replace("Bash", "Read"))
            .unwrap()
            .is_none()
    );
    assert!(hooks::transform("unknown", &input).unwrap().is_none());
}

#[test]
fn oversized_input_and_combined_streams_are_noops() {
    assert!(
        hooks::transform("claude", &" ".repeat(16 * 1024 * 1024 + 1))
            .unwrap()
            .is_none()
    );
    let text = "x".repeat(4 * 1024 * 1024 + 1);
    let response = format!(
        r#"{{"stdout":{},"stderr":{}}}"#,
        json_string(&text),
        json_string(&text)
    );
    assert!(
        hooks::transform("claude", &claude(&response))
            .unwrap()
            .is_none()
    );
}

#[test]
fn tool_input_and_transcript_are_never_executed_or_read() {
    let path = std::env::temp_dir().join(format!("sift-hook-no-execution-{}", std::process::id()));
    assert!(!path.exists());
    let input = format!(
        r#"{{"hook_event_name":"PostToolUse","tool_name":"Bash","transcript_path":{},"tool_input":{{"command":{}}},"tool_response":{{"stdout":{}}}}}"#,
        json_string(&path.to_string_lossy()),
        json_string(&format!("touch {}", path.display())),
        json_string(&log())
    );
    assert!(hooks::transform("claude", &input).unwrap().is_some());
    assert!(!path.exists());
}

const MARKER: &[u8] = b"HOOK-ENTRY\n";

#[test]
fn hook_entry() {
    let Ok(host) = std::env::var("SIFT_HOOK_TEST_HOST") else {
        return;
    };
    std::io::stdout().write_all(MARKER).unwrap();
    std::io::stdout().flush().unwrap();
    hooks::run(&host).unwrap();
    std::process::exit(0);
}

fn run(host: &str, input: &[u8]) -> Vec<u8> {
    Sandbox::new().run(host, input)
}

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "sift-hook-state-{}-{}-{}",
            std::process::id(),
            state::unix_millis(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn settings(&self, text: &str) {
        std::fs::create_dir_all(self.0.join("config")).unwrap();
        std::fs::write(self.0.join("config/config.json"), text).unwrap();
    }

    fn events(&self) -> Vec<state::Event> {
        std::fs::read_to_string(self.0.join("state/metrics.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn run(&self, host: &str, input: &[u8]) -> Vec<u8> {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "hooks_tests::hook_entry", "--nocapture"])
            .current_dir(&self.0)
            .env("SIFT_HOOK_TEST_HOST", host)
            .env("SIFT_CONFIG_DIR", self.0.join("config"))
            .env("SIFT_STATE_DIR", self.0.join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        // Oversized input can close the pipe at cap+1, before all bytes are written.
        if let Err(error) = stdin.write_all(input) {
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        }
        drop(stdin);
        let output = child
            .wait_with_output()
            .map(crate::test_support::clean)
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let start = output
            .stdout
            .windows(MARKER.len())
            .position(|bytes| bytes == MARKER)
            .unwrap()
            + MARKER.len();
        output.stdout[start..].to_vec()
    }

    fn cli(&self, host: &str, input: &str, backend: Option<&str>) -> Vec<u8> {
        let mut command = crate::test_support::command(["hook", host]);
        command
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env("SIFT_CONFIG_DIR", self.0.join("config"))
            .env("SIFT_STATE_DIR", self.0.join("state"))
            .env_remove("TERMINAL_ENV")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(backend) = backend {
            command.env("TERMINAL_ENV", backend);
        }
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child
            .wait_with_output()
            .map(crate::test_support::clean)
            .unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert_eq!(output.stdout.iter().filter(|&&b| b == b'\n').count(), 1);
        output.stdout
    }
}

#[test]
fn cli_wrapped_output_skips_replacement_usage_and_original_storage() {
    for host in ["claude", "copilot", "hermes"] {
        if host == "hermes" && !cfg!(unix) {
            continue;
        }
        for command in [
            "sift proxy -- cat file",
            "sift run -- cat file",
            "sift run -- cat --raw",
            "ctx run -- tool --raw",
            "ctx --color never run -- tool --raw",
            "ctx --data-root='/tmp/ctx data' --quiet output run -- tool --raw",
            "sift run --capture --raw -- cat file",
            "ctx --color never run --raw -- cat file",
            "ctx --color=never run --raw -- cat file",
            "ctx --data-root '/tmp/ctx data' output run --raw -- cat file",
            "ctx --data-root='/tmp/ctx data' --color=auto --quiet output proxy -- cat file",
            "ctx --quiet run --raw -- cat file",
            "command '/opt/ctx tools/ctx' --color always output run --capture --raw -- cat file",
        ] {
            let sandbox = Sandbox::new();
            sandbox.settings(r#"{"keep_originals":true}"#);
            let input = completion_command(host, Some(command));
            assert_eq!(sandbox.cli(host, &input, Some("local")), b"{}\n");
            assert!(!sandbox.0.join("state/metrics.jsonl").exists());
            assert!(!sandbox.0.join("state/originals").exists());
            assert_eq!(
                std::fs::read_to_string(sandbox.0.join("config/config.json")).unwrap(),
                r#"{"keep_originals":true}"#
            );
        }
        for command in [
            "cat file",
            "ctx graph query Widget",
            "sift proxy cat; echo done",
            "ctx --color never run --raw cat | cat",
        ] {
            let sandbox = Sandbox::new();
            let input = completion_command(host, Some(command));
            let expected = hooks::transform(host, &completion_command(host, None))
                .unwrap()
                .unwrap();
            assert_eq!(
                sandbox.cli(host, &input, Some("local")),
                format!("{expected}\n").as_bytes()
            );
            let events = sandbox.events();
            assert_eq!(events.len(), if host == "claude" { 2 } else { 1 });
            let compactor = sift::Compactor::new().unwrap();
            let compact = compactor.compact(&log());
            for event in events {
                assert_eq!(event.input_tokens, Some(compact.input_tokens as u64));
                assert_eq!(event.output_tokens, Some(compact.output_tokens as u64));
                assert_eq!(event.input_bytes, log().len() as u64);
                assert_eq!(event.output_bytes, compact.text.len() as u64);
            }
        }
    }
}

#[test]
fn hermes_raw_detection_requires_known_local_posix_backend() {
    let input = completion_command("hermes", Some("sift proxy cat file"));
    let expected = hooks::transform("hermes", &completion_command("hermes", None))
        .unwrap()
        .unwrap();
    for backend in [None, Some("docker"), Some("ssh"), Some("unknown"), Some("")] {
        let sandbox = Sandbox::new();
        assert_eq!(
            sandbox.cli("hermes", &input, backend),
            format!("{expected}\n").as_bytes()
        );
        assert_eq!(sandbox.events().len(), 1);
    }
    if !cfg!(unix) {
        assert_eq!(
            Sandbox::new().cli("hermes", &input, Some("local")),
            format!("{expected}\n").as_bytes()
        );
    }
}

#[test]
fn hook_project_uses_host_cwd_and_keeps_invalid_cwd_unscoped() {
    for host in ["claude", "copilot"] {
        let sandbox = Sandbox::new();
        let other = Sandbox::new();
        std::fs::create_dir(sandbox.0.join(".git")).unwrap();
        std::fs::create_dir(other.0.join(".git")).unwrap();
        let base = if host == "claude" {
            claude(&format!(r#"{{"stdout":{}}}"#, json_string(&log())))
        } else {
            serde_json::json!({"toolName":"bash", "toolResult":{
                "resultType":"success", "textResultForLlm":log()
            }})
            .to_string()
        };
        let launcher = state::project_at(&sandbox.0).unwrap();
        let other_project = state::project_at(&other.0).unwrap();
        for (cwd, expected) in [
            (None, Some(launcher.clone())),
            (Some(serde_json::json!(sandbox.0)), Some(launcher)),
            (Some(serde_json::json!(other.0)), Some(other_project)),
            (Some(serde_json::json!(other.0.join("missing"))), None),
            (Some(serde_json::json!("relative")), None),
            (Some(serde_json::Value::Null), None),
        ] {
            let mut input: serde_json::Value = serde_json::from_str(&base).unwrap();
            if let Some(cwd) = cwd {
                input["cwd"] = cwd;
            }
            assert_ne!(sandbox.run(host, input.to_string().as_bytes()), b"{}\n");
            let metrics = std::fs::read_to_string(sandbox.0.join("state/metrics.jsonl")).unwrap();
            let last: serde_json::Value =
                serde_json::from_str(metrics.lines().last().unwrap()).unwrap();
            assert_eq!(last["project"].as_str(), expected.as_deref());
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn run_emits_one_json_line_and_fails_open() {
    for input in [b"{".as_slice(), b"\xff", b"{}", b"{}\n{}"] {
        assert_eq!(run("claude", input), b"{}\n");
    }
    assert_eq!(run("unknown", b"{}"), b"{}\n");
    assert_eq!(run("claude", &vec![b' '; 16 * 1024 * 1024 + 2]), b"{}\n");
    let input = claude(&format!(
        r#"{{"stdout":{},"stderr":""}}"#,
        json_string(&log())
    ));
    let output = run("claude", input.as_bytes());
    assert_eq!(output.iter().filter(|&&b| b == b'\n').count(), 1);
    assert_eq!(output.last(), Some(&b'\n'));
    assert_eq!(object(std::str::from_utf8(&output).unwrap()).len(), 1);
}

#[test]
fn cli_settings_disable_invalid_and_exact_tool_exclusions() {
    let input = claude(&format!(r#"{{"stdout":{}}}"#, json_string(&log())));
    for config in [
        r#"{"enabled":false}"#,
        "{",
        r#"{"exclude_commands":["Bash"]}"#,
    ] {
        let sandbox = Sandbox::new();
        sandbox.settings(config);
        assert_eq!(sandbox.run("claude", input.as_bytes()), b"{}\n");
        assert!(!sandbox.0.join("state").exists());
        assert_eq!(
            std::fs::read_to_string(sandbox.0.join("config/config.json")).unwrap(),
            config
        );
    }
    let sandbox = Sandbox::new();
    // A program exclusion does not pretend to parse a shell expression.
    sandbox.settings(r#"{"exclude_commands":["git"],"record_usage":false}"#);
    let input = input.replacen('{', "{\"tool_input\":{\"command\":\"git status\"},", 1);
    assert_ne!(sandbox.run("claude", input.as_bytes()), b"{}\n");
    assert!(!sandbox.0.join("state").exists());
}

#[test]
fn usage_records_actual_field_counts_without_raw_text() {
    let sandbox = Sandbox::new();
    let stdout = log();
    let stderr = "diagnostic, retain all context\r\n".repeat(100);
    let input = claude(&format!(
        r#"{{"stdout":{},"stderr":{}}}"#,
        json_string(&stdout),
        json_string(&stderr)
    ));
    assert_ne!(sandbox.run("claude", input.as_bytes()), b"{}\n");
    let events = sandbox.events();
    assert_eq!(events.len(), 2);
    let compactor = sift::Compactor::new().unwrap();
    for (event, (label, text)) in events
        .iter()
        .zip([("Bash.stdout", stdout), ("Bash.stderr", stderr)])
    {
        let expected = compactor.compact(&text);
        assert_eq!(event.command, label);
        assert_eq!(event.source.as_deref(), Some("hook-claude"));
        assert_eq!(
            event.input_tokens,
            Some(compactor.count_tokens(&text) as u64)
        );
        assert_eq!(
            event.output_tokens,
            Some(compactor.count_tokens(&expected.text) as u64)
        );
        assert_eq!(event.input_bytes, text.len() as u64);
        assert_eq!(event.output_bytes, expected.text.len() as u64);
        assert_eq!(event.exit_code, None);
        assert_eq!(event.original_id, None);
    }
    let metrics = std::fs::read_to_string(sandbox.0.join("state/metrics.jsonl")).unwrap();
    assert!(!metrics.contains("checkpoint"));
    assert!(!metrics.contains("diagnostic"));
    assert!(!sandbox.0.join("state/originals").exists());

    let sandbox = Sandbox::new();
    assert_eq!(
        sandbox.run("claude", claude(r#"{"stdout":"tiny"}"#).as_bytes()),
        b"{}\n"
    );
    assert!(!sandbox.0.join("state").exists());
}

#[test]
fn cli_claude_native_shaped_git_and_cargo_omit_exit_metadata_and_count_actual_emission() {
    let (cargo, cargo_view) = cargo_output(false);
    for (command, stdout, expected_view) in [
        ("git status", STATUS, STATUS_VIEW),
        (
            "'cargo' 'test' --workspace",
            cargo.as_str(),
            cargo_view.as_str(),
        ),
    ] {
        for (stdout, expected_view, stderr) in [
            (stdout, expected_view, STATUS),
            (
                stdout.trim_end_matches('\n'),
                expected_view.trim_end_matches('\n'),
                "",
            ),
        ] {
            let sandbox = Sandbox::new();
            // Exercise both the LF/dual-stream counterpart and Claude's native
            // no-exitCode/no-final-LF/empty-stderr shape.
            let response = format!(
                r#"{{"stdout":{},"stderr":{},"interrupted":false,"isImage":false,"noOutputExpected":false,"opaque":1.23000e+9999}}"#,
                json_string(stdout),
                json_string(stderr)
            );
            let input = claude_command(&response, command);
            let mut child = crate::test_support::command(["hook", "claude"])
                .current_dir(&sandbox.0)
                .env("HOME", &sandbox.0)
                .env("SIFT_CONFIG_DIR", sandbox.0.join("config"))
                .env("SIFT_STATE_DIR", sandbox.0.join("state"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            let output = child
                .wait_with_output()
                .map(crate::test_support::clean)
                .unwrap();
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            assert_eq!(output.stdout.iter().filter(|&&b| b == b'\n').count(), 1);
            let changed = claude_output(std::str::from_utf8(&output.stdout).unwrap());
            let compactor = sift::Compactor::new().unwrap();
            assert_eq!(
                string(&changed["stdout"]),
                compactor.compact(expected_view).text
            );
            assert!(
                compactor.compact(expected_view).output_tokens
                    < compactor.compact(stdout).output_tokens
            );
            assert_eq!(string(&changed["stderr"]), compactor.compact(stderr).text);
            assert!(!changed.contains_key("exitCode"));
            assert_eq!(changed.len(), object(&response).len());
            assert_eq!(changed["noOutputExpected"].get(), "false");
            assert_eq!(changed["opaque"].get(), "1.23000e+9999");
            let reference = tiktoken_rs::o200k_base().unwrap();
            let events = sandbox.events();
            assert_eq!(events.len(), if stderr.is_empty() { 1 } else { 2 });
            for (event, (field, original)) in
                events.iter().zip([("stdout", stdout), ("stderr", stderr)])
            {
                let emitted = string(&changed[field]);
                assert_eq!(event.command, format!("Bash.{field}"));
                assert_eq!(event.source.as_deref(), Some("hook-claude"));
                assert_eq!(
                    event.input_tokens,
                    Some(reference.encode_ordinary(original).len() as u64)
                );
                assert_eq!(
                    event.output_tokens,
                    Some(reference.encode_ordinary(&emitted).len() as u64)
                );
                assert_eq!(event.input_bytes, original.len() as u64);
                assert_eq!(event.output_bytes, emitted.len() as u64);
                assert_eq!(event.exit_code, None);
            }
            assert_ne!(
                events[0].input_tokens,
                Some(reference.encode_ordinary(expected_view).len() as u64)
            );
            let metrics = std::fs::read_to_string(sandbox.0.join("state/metrics.jsonl")).unwrap();
            assert!(!metrics.contains("encoding"));
            assert!(!metrics.contains("module::alpha"));
            assert!(!sandbox.0.join("state/originals").exists());
        }
    }
}

#[test]
fn claude_unterminated_utf8_views_preserve_last_character_and_opaque_metadata() {
    let (cargo, cargo_view) = cargo_output(false);
    for (command, raw, view) in [
        ("git status", STATUS, STATUS_VIEW),
        ("cargo test", cargo.as_str(), cargo_view.as_str()),
    ] {
        let stdout = format!("{raw}retained diagnostic: café 字 🦀");
        let expected = format!("{view}retained diagnostic: café 字 🦀");
        let compactor = sift::Compactor::new().unwrap();
        assert!(
            compactor.compact(&expected).output_tokens < compactor.compact(&stdout).output_tokens
        );
        for exit in ["", ",\"exitCode\":0"] {
            let response = format!(
                r#"{{"stdout":{},"stderr":"","interrupted":false,"isImage":false,"noOutputExpected":{{"n":-0.0100e+9999}},"number":1e9999{exit}}}"#,
                json_string(&stdout)
            );
            let output = hooks::transform("claude", &claude_command(&response, command))
                .unwrap()
                .unwrap();
            let changed = claude_output(&output);
            assert_eq!(
                string(&changed["stdout"]),
                compactor.compact(&expected).text
            );
            for (key, value) in object(&response) {
                if key != "stdout" {
                    assert_eq!(changed[&key].get(), value.get(), "opaque field {key}");
                }
            }
            assert_eq!(changed.contains_key("exitCode"), !exit.is_empty());
        }
    }
}

#[test]
fn claude_unterminated_incomplete_and_custom_output_keeps_lossless_path() {
    let (cargo, _) = cargo_output(false);
    let incomplete_git = STATUS.split_once("\tboth modified:").unwrap().0;
    let incomplete_cargo = cargo.split_once("test result:").unwrap().0;
    let inconsistent_cargo = cargo.replace("8 passed", "7 passed");
    let controlled = format!("\x1b[31m{STATUS}");
    let compactor = sift::Compactor::new().unwrap();
    for (command, text) in [
        ("git status", incomplete_git),
        ("cargo test", incomplete_cargo),
        ("cargo test", inconsistent_cargo.as_str()),
        ("git status", controlled.as_str()),
        ("git status --porcelain", STATUS),
        ("cargo test -- --nocapture", cargo.as_str()),
        ("cargo test -- --format json", cargo.as_str()),
    ] {
        let text = text.trim_end_matches('\n');
        assert!(text.len() >= 256);
        for flags in [
            r#""interrupted":false,"isImage":false"#,
            r#""interrupted":false"#,
        ] {
            let response = format!(
                r#"{{"stdout":{},"stderr":"",{flags},"noOutputExpected":false}}"#,
                json_string(text)
            );
            let output = hooks::transform("claude", &claude_command(&response, command)).unwrap();
            assert_eq!(
                output,
                hooks::transform("claude", &claude(&response)).unwrap()
            );
            if let Some(output) = output {
                assert_eq!(
                    string(&claude_output(&output)["stdout"]),
                    compactor.compact(text).text
                );
            }
        }
    }
}

#[test]
fn originals_opt_in_and_storage_failure_do_not_change_output() {
    let text = log();
    let input = claude(&format!(r#"{{"stderr":{}}}"#, json_string(&text)));
    let sandbox = Sandbox::new();
    sandbox.settings(r#"{"keep_originals":true}"#);
    let output = sandbox.run("claude", input.as_bytes());
    assert_ne!(output, b"{}\n");
    let events = sandbox.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, "Bash.stderr");
    let mut recalled = Vec::new();
    state::recall_at(
        &sandbox.0.join("state"),
        events[0].original_id.as_deref().unwrap(),
        false,
        &mut recalled,
    )
    .unwrap();
    assert_eq!(recalled, text.as_bytes());

    let sandbox = Sandbox::new();
    std::fs::write(sandbox.0.join("state"), b"not a directory").unwrap();
    assert_eq!(sandbox.run("claude", input.as_bytes()), output);
    assert_eq!(
        std::fs::read(sandbox.0.join("state")).unwrap(),
        b"not a directory"
    );

    let sandbox = Sandbox::new();
    sandbox.settings(r#"{"record_usage":false,"keep_originals":true}"#);
    assert_eq!(sandbox.run("claude", input.as_bytes()), output);
    assert!(!sandbox.0.join("state").exists());
}
