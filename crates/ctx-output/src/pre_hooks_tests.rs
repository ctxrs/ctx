// Adapted from Sift tests, MIT; see this crate’s NOTICE.
use crate::hooks;
use crate::pre_hooks;
use std::path::Path;

#[test]
fn native_envelopes_preserve_all_arguments() {
    for (host, tool, event, pointer) in [
        (
            "codex",
            "Bash",
            "PreToolUse",
            "/hookSpecificOutput/updatedInput",
        ),
        (
            "vibe",
            "bash",
            "pre_tool",
            "/hook_specific_output/tool_input",
        ),
    ] {
        let input = format!(
            r#"{{"hook_event_name":"{event}","tool_name":"{tool}","tool_input":{{"command":"git status","shell":"bash","timeout":123456789012345678901234567890,"description":"test","metadata":{{"$serde_json::private::Number":"001"}}}}}}"#
        );
        let output = pre_hooks::transform(host, &input, Path::new("/opt/ctx"), &[])
            .unwrap()
            .unwrap();
        let mut args: hooks::Object = serde_json::from_str(&output).unwrap();
        for field in pointer.split('/').filter(|s| !s.is_empty()) {
            args = args.object(field).unwrap();
        }
        assert_eq!(
            args.string("command").unwrap(),
            "command true || git status; command '/opt/ctx' output run --capture -- git status"
        );
        assert_eq!(args.string("description").unwrap(), "test");
        assert!(output.contains("123456789012345678901234567890"));
        assert!(output.contains(r#""metadata":{"$serde_json::private::Number":"001"}"#));
    }
}

#[test]
fn shared_copilot_file_does_not_double_wrap_cli() {
    for tool in ["Bash", "bash", "powershell"] {
        let input = format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"{tool}","tool_input":{{"command":"git status"}}}}"#
        );
        assert!(
            pre_hooks::transform("vscode", &input, Path::new("sift"), &[])
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn bad_or_wrong_events_never_create_a_replacement() {
    for input in [
        r#"{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"git status"}}"#,
        r#"{"tool_name":"Read","tool_input":{"command":"git status"}}"#,
        r#"{"tool_name":"Bash","tool_input":{"command":"git status","command":"git diff"}}"#,
        r#"{"tool_name":"Bash","tool_input":{"command":"git status","shell":"fish"}}"#,
    ] {
        assert!(
            pre_hooks::transform("codex", input, Path::new("sift"), &[])
                .ok()
                .flatten()
                .is_none()
        );
    }
    let input = "\u{feff}{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"git status\"}}";
    assert!(
        pre_hooks::transform("codex", input, Path::new("sift"), &[])
            .unwrap()
            .is_some()
    );
}

#[test]
fn unqualified_host_rewrites_leave_native_requests_untouched() {
    for (host, tool, event) in [
        ("gemini", "run_shell_command", "BeforeTool"),
        ("vscode", "run_in_terminal", "PreToolUse"),
        ("cursor", "Shell", "preToolUse"),
        ("droid", "Execute", "PreToolUse"),
    ] {
        let input = serde_json::json!({"hook_event_name":event,"tool_name":tool,
            "tool_input":{"command":"git status && git diff", "description":"metadata rule"}});
        assert!(
            pre_hooks::transform(host, &input.to_string(), Path::new("/opt/ctx"), &[])
                .unwrap()
                .is_none()
        );
    }
}
