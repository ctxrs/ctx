//! Native hook lifecycle and manual-wrapper interaction through the executable.

use super::*;

#[test]
fn sift_is_the_public_output_command() {
    let sandbox = Sandbox::new();
    let output = sandbox.output(&["sift", "--help"], b"");

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("ctx sift"));
}

#[test]
fn output_hook_preserves_already_wrapped_output_without_duplicate_accounting() {
    let sandbox = Sandbox::new();
    sandbox.write("output/config/config.json", br#"{"keep_originals":true}"#);
    let before = sandbox.protected_state();
    let stdout = "synthetic hook output remains complete\n".repeat(160);
    let request = |command| {
        json!({
            "hook_event_name":"PostToolUse", "tool_name":"Bash",
            "tool_input":{"command":command},
            "tool_response":{"stdout":stdout, "stderr":"", "interrupted":false, "isImage":false}
        })
        .to_string()
    };
    for command in [
        "ctx sift -- cat fixture",
        "ctx sift --raw -- cat fixture",
        "ctx sift --capture --raw -- cat fixture",
        "ctx sift cat fixture",
        "ctx sift run --raw -- cat fixture",
        "ctx run --raw -- cat fixture",
        "ctx run -- cat fixture",
        "ctx --color never run -- cat --raw",
        "ctx --quiet output run --capture -- cat fixture",
        "ctx --color never run --raw -- cat fixture",
        "ctx --color=never output run --capture --raw -- cat fixture",
        "ctx --quiet output proxy -- cat fixture",
    ] {
        let output = sandbox.output(&["sift", "hook", "claude"], request(command).as_bytes());
        assert_eq!(json_output(output), json!({}), "wrapped command: {command}");
        assert!(
            !sandbox.root.join("output/state").exists(),
            "wrapped hook recorded output twice"
        );
        assert_eq!(sandbox.protected_state(), before);
    }
    let output = sandbox.output(&["sift", "hook", "claude"], request("cat --raw").as_bytes());
    let replacement = json_output(output);
    assert_eq!(
        replacement["hookSpecificOutput"]["hookEventName"],
        "PostToolUse"
    );
    let changed = replacement["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
        .as_str()
        .expect("an ordinary command flag must not suppress hook compaction");
    assert!(changed.len() < stdout.len());
    sandbox.assert_no_connection();
}

#[test]
fn explicit_output_hook_registration_invokes_the_existing_runtime() {
    let sandbox = Sandbox::new();
    let target = sandbox.repo().join(".claude/settings.json");
    assert!(!target.exists(), "normal installation must leave hooks off");
    sandbox.write(
        "repo/.claude/settings.json",
        br#"{"permissions":{"deny":["Bash(rm:*)"]}}"#,
    );
    let install = sandbox.json(&[
        "integrations",
        "install",
        "sift",
        "--agent",
        "claude-code",
        "--project",
        "--format",
        "json",
    ]);
    assert_eq!(install["results"][0]["status"], "installed");
    let config: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(config["permissions"]["deny"][0], "Bash(rm:*)");
    assert_eq!(
        config["hooks"]["PostToolUse"][0]["matcher"],
        "Bash|PowerShell"
    );
    let command = config["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(command.contains(" sift hook claude"));
    assert!(command.contains(sandbox.binary.to_str().unwrap()));

    let stdout = "synthetic command output remains long\n".repeat(160);
    let request = json!({"hook_event_name":"PostToolUse", "tool_name":"Bash",
        "tool_input":{"command":"cargo test"},
        "tool_response":{"stdout":stdout,"stderr":"","interrupted":false,"isImage":false}});
    let response = sandbox.json(&["sift", "hook", "claude"]);
    assert_eq!(response, json!({}), "missing request must pass through");
    let transformed =
        json_output(sandbox.output(&["sift", "hook", "claude"], request.to_string().as_bytes()));
    let replacement = transformed["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
        .as_str()
        .unwrap();
    assert!(replacement.len() < stdout.len());
    let mut raw_request = request;
    raw_request["tool_input"]["command"] = json!("ctx sift run --raw -- cargo test");
    assert_eq!(
        json_output(sandbox.output(
            &["sift", "hook", "claude"],
            raw_request.to_string().as_bytes()
        )),
        json!({})
    );

    let remove = sandbox.json(&[
        "integrations",
        "remove",
        "output-hook",
        "--agent",
        "claude-code",
        "--project",
        "--format",
        "json",
    ]);
    assert_eq!(remove["results"][0]["status"], "absent");
    let after: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(after["permissions"], config["permissions"]);
    assert_eq!(after["hooks"]["PostToolUse"], json!([]));
    sandbox.assert_no_connection();
}

#[test]
fn legacy_output_hook_can_be_reinstalled_or_removed_after_the_top_level_route_is_removed() {
    let sandbox = Sandbox::new();
    let target = sandbox.repo().join(".claude/settings.json");
    let install = sandbox.json(&[
        "integrations",
        "install",
        "sift",
        "--agent",
        "claude-code",
        "--project",
        "--format",
        "json",
    ]);
    assert_eq!(install["results"][0]["status"], "installed");
    let mut config: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    let command = config["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .replace(" sift hook ", " output hook ");
    config["hooks"]["PostToolUse"][0]["hooks"][0]["command"] = json!(command);
    fs::write(&target, serde_json::to_vec(&config).unwrap()).unwrap();

    let status = sandbox.json(&[
        "integrations",
        "status",
        "sift",
        "--agent",
        "claude-code",
        "--project",
        "--format",
        "json",
    ]);
    assert_eq!(status["results"][0]["status"], "outdated");
    let reinstall = sandbox.json(&[
        "integrations",
        "install",
        "sift",
        "--agent",
        "claude-code",
        "--project",
        "--format",
        "json",
    ]);
    assert_eq!(reinstall["results"][0]["status"], "installed");
    let upgraded: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert!(upgraded["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains(" sift hook claude"));

    fs::write(&target, serde_json::to_vec(&config).unwrap()).unwrap();
    let remove = sandbox.json(&[
        "integrations",
        "remove",
        "sift",
        "--agent",
        "claude-code",
        "--project",
        "--format",
        "json",
    ]);
    assert_eq!(remove["results"][0]["status"], "absent");
    let remaining: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(remaining["hooks"]["PostToolUse"], json!([]));
}

#[cfg(unix)]
#[test]
fn generated_sift_commands_execute_the_real_cli_once_and_preserve_exit() {
    use std::os::unix::fs::PermissionsExt;

    let sandbox = Sandbox::new();
    sandbox.write("repo/git", b"#!/bin/sh\nprintf 'ran\\n' >> invocations\nprintf '%s\\n' \"$@\"\nprintf 'child diagnostic\\n' >&2\nexit 17\n");
    fs::set_permissions(
        sandbox.repo().join("git"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let original = "./git status 'two words' '' 'literal $value;*'";
    let rewritten = sandbox.json(&["sift", "rewrite", "--json", "--", original]);
    let mut commands = vec![rewritten["command"].as_str().unwrap().to_owned()];
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
        let input = json!({"hook_event_name":event, "tool_name":tool,
            "tool_input":{"command":original, "shell":"bash", "description":"keep me"}});
        let output =
            json_output(sandbox.output(&["sift", "hook", host], input.to_string().as_bytes()));
        let updated = output.pointer(pointer).unwrap();
        assert_eq!(updated["description"], "keep me");
        commands.push(updated["command"].as_str().unwrap().to_owned());
    }
    let isolated = sandbox.command();
    for command in commands {
        assert!(command.starts_with(&format!("command true || {original}; ")));
        assert!(command.contains(" sift run "));
        for shell in ["/bin/sh", "/bin/bash"] {
            let counter = sandbox.repo().join("invocations");
            fs::write(&counter, b"").unwrap();
            let output = Command::new(shell)
                .env_clear()
                .envs(
                    isolated
                        .get_envs()
                        .filter_map(|(key, value)| value.map(|value| (key, value))),
                )
                .current_dir(sandbox.repo())
                .timeout(Duration::from_secs(30))
                .args(["-c", &command])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(17), "{output:?}");
            assert_eq!(output.stdout, b"status\ntwo words\n\nliteral $value;*\n");
            assert_eq!(output.stderr, b"child diagnostic\n");
            assert_eq!(fs::read(&counter).unwrap(), b"ran\n");
        }
    }
    sandbox.assert_no_connection();
}

#[test]
fn renamed_cli_hook_registration_is_idempotent_and_raw_output_is_exempt() {
    let mut sandbox = Sandbox::new();
    let renamed = sandbox.root.join(if cfg!(windows) {
        "ctx-linux-x64.exe"
    } else {
        "ctx-linux-x64"
    });
    fs::copy(&sandbox.binary, &renamed).unwrap();
    sandbox.binary = renamed;
    for (agent, relative, event) in [
        ("claude-code", ".claude/settings.json", "PostToolUse"),
        ("github-copilot", ".github/hooks/ctx.json", "postToolUse"),
        ("codex", ".codex/hooks.json", "PreToolUse"),
    ] {
        if agent == "codex" && !cfg!(unix) {
            continue;
        }
        for action in ["install", "status", "install"] {
            let result = sandbox.json(&[
                "integrations",
                action,
                "sift",
                "--agent",
                agent,
                "--project",
                "--format",
                "json",
            ]);
            assert_eq!(
                result["results"][0]["status"], "installed",
                "{agent} {action}: {result}"
            );
        }
        let config: Value =
            serde_json::from_slice(&fs::read(sandbox.repo().join(relative)).unwrap()).unwrap();
        assert_eq!(config["hooks"][event].as_array().unwrap().len(), 1);
    }
    let output = "synthetic hook output remains complete\n".repeat(160);
    let program = sandbox.binary.to_str().unwrap();
    let command = if cfg!(windows) {
        format!(
            "& '{}' sift --raw -- cat fixture",
            program.replace('\'', "''")
        )
    } else {
        format!(
            "'{}' sift --raw -- cat fixture",
            program.replace('\'', "'\"'\"'")
        )
    };
    for (command, wrapped) in [
        (command.as_str(), true),
        ("unrelated sift --raw -- cat fixture", false),
        ("/other/ctx-linux-x64 sift --raw -- cat fixture", false),
    ] {
        let input = json!({"hook_event_name":"PostToolUse", "tool_name":if cfg!(windows) { "PowerShell" } else { "Bash" },
            "tool_input":{"command":command}, "tool_response":{"stdout":output,"stderr":"","interrupted":false,"isImage":false}});
        let result =
            json_output(sandbox.output(&["sift", "hook", "claude"], input.to_string().as_bytes()));
        assert_eq!(result == json!({}), wrapped, "{command}: {result}");
    }
    sandbox.assert_no_connection();
}
