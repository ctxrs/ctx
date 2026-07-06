#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn qwen_kimi_mistral_mux_and_qoder_default_sources_import_search_and_reimport() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("qwen-code/.qwen")),
        &temp.path().join(".qwen"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("kimi-code-cli/.kimi-code")),
        &temp.path().join(".kimi-code"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("mistral-vibe/v1/logs/session")),
        &temp.path().join(".vibe").join("logs").join("session"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("mux/v0.27.0/sessions")),
        &temp.path().join(".mux").join("sessions"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("qoder/projects")),
        &temp.path().join(".qoder").join("projects"),
    );

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    for (provider, source_format) in [
        ("qwen_code", "qwen_code_chat_jsonl_tree"),
        ("kimi_code_cli", "kimi_code_cli_wire_jsonl_tree"),
        ("mistral_vibe", "mistral_vibe_session_jsonl_tree"),
        ("mux", "mux_session_jsonl_tree"),
        ("qoder", "qoder_transcript_jsonl_tree"),
    ] {
        let source = sources["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| {
                source["provider"] == provider && source["source_format"] == source_format
            })
            .unwrap_or_else(|| panic!("missing {provider} source in {sources:#}"));
        assert_eq!(source["status"], "available");
        assert_eq!(source["import_support"], "native");
        assert_eq!(source["native_import"], true);
        assert_eq!(source["importable"], true);
    }

    for (cli_provider, stored_provider, query, minimum_events) in [
        ("qwen-code", "qwen_code", "qwen jsonl oracle prompt", 3),
        (
            "kimi-code-cli",
            "kimi_code_cli",
            "kimi jsonl oracle prompt",
            7,
        ),
        (
            "mistral-vibe",
            "mistral_vibe",
            "mistral vibe oracle prompt",
            4,
        ),
        ("mux", "mux", "mux jsonl oracle prompt", 6),
        ("qoder", "qoder", "qoder jsonl oracle prompt", 7),
    ] {
        let first = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--json",
            "--progress",
            "none",
        ]));
        assert_eq!(first["totals"]["failed"], 0);
        assert_eq!(first["totals"]["imported_sources"], 1);
        assert!(
            first["totals"]["imported_events"].as_u64().unwrap() >= minimum_events,
            "{first:#}"
        );

        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            cli_provider,
            "--refresh",
            "off",
            "--json",
        ]));
        assert_search_provider_oracle(&search, stored_provider, query, 1, "message");

        let second = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--json",
            "--progress",
            "none",
        ]));
        assert_eq!(second["totals"]["failed"], 0);
        assert_eq!(second["totals"]["imported_events"], 0);
    }
}

pub(crate) fn write_native_kimi_fixture(temp: &TempDir, query: &str) -> String {
    let home = temp.path().join("native-kimi/.kimi-code");
    let session = home.join("sessions/wd_demo_abc123/kimi-cli-native");
    let main = session.join("agents/main");
    let child = session.join("agents/agent-1");
    fs::create_dir_all(&main).unwrap();
    fs::create_dir_all(&child).unwrap();
    fs::write(
        home.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": "kimi-cli-native",
                "sessionDir": session.display().to_string(),
                "workDir": "/workspace/kimi"
            })
        ),
    )
    .unwrap();
    fs::write(
        session.join("state.json"),
        json!({
            "createdAt": "2026-07-04T13:00:00Z",
            "updatedAt": "2026-07-04T13:00:05Z",
            "title": "Kimi native CLI",
            "lastPrompt": query,
            "agents": {
                "main": {"homedir": "/fixture/agents/main", "type": "main", "parentAgentId": null},
                "agent-1": {"homedir": "/fixture/agents/agent-1", "type": "coder", "parentAgentId": "main"}
            }
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        main.join("wire.jsonl"),
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n",
            json!({"type": "metadata", "protocol_version": "1.4", "created_at": 1783170000000i64}),
            json!({"type": "turn.prompt", "time": 1783170001000i64, "input": [{"type": "text", "text": query}], "origin": {"kind": "user"}}),
            json!({"type": "context.append_message", "time": 1783170002000i64, "message": {"role": "assistant", "content": [{"type": "text", "text": "native Kimi import ok"}]}}),
            json!({"type": "context.append_loop_event", "time": 1783170003000i64, "event": {"type": "tool.call", "toolName": "Write", "input": {"path": "src/kimi_cli_native.txt", "content": "proof"}}}),
            json!({"type": "context.append_loop_event", "time": 1783170004000i64, "event": {"type": "tool.result", "toolName": "Write", "output": "wrote src/kimi_cli_native.txt"}}),
            json!({"type": "usage.record", "time": 1783170005000i64, "model": "kimi-k2", "usage": {"input_tokens": 11, "output_tokens": 13}})
        ),
    )
    .unwrap();
    fs::write(
        child.join("wire.jsonl"),
        concat!(
            "{\"type\":\"metadata\",\"protocol_version\":\"1.4\",\"created_at\":1783170006000}\n",
            "{\"type\":\"turn.prompt\",\"time\":1783170007000,\"input\":[{\"type\":\"text\",\"text\":\"child inspect\"}]}\n",
            "{\"type\":\"context.append_message\",\"time\":1783170008000,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"child done\"}]}}\n",
        ),
    )
    .unwrap();
    home.to_str().unwrap().to_owned()
}
