#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn provider_json_names_are_accepted_as_cli_filter_aliases() {
    let temp = tempdir();
    initialize_empty_store(&temp);

    for (provider, expected) in [
        ("copilot_cli", "copilot_cli"),
        ("github-copilot", "copilot_cli"),
        ("factory_ai_droid", "factory_ai_droid"),
        ("droid", "factory_ai_droid"),
        ("kilo_code", "kilo"),
        ("qwen_code", "qwen_code"),
        ("kimi_code_cli", "kimi_code_cli"),
        ("code_buddy", "codebuddy"),
        ("trae", "trae"),
        ("trae-cn", "trae"),
        ("auggie", "auggie"),
        ("augment", "auggie"),
        ("augment-code", "auggie"),
        ("forge", "forgecode"),
        ("forge_code", "forgecode"),
        ("mistral_vibe", "mistral_vibe"),
        ("mux", "mux"),
        ("qoder-cn", "lingma"),
        ("qoder_cn", "lingma"),
        ("qoder", "qoder"),
        ("open_claw", "openclaw"),
        ("nano_claw", "nanoclaw"),
        ("astr_bot", "astrbot"),
        ("windsurf_cascade", "windsurf"),
        ("open_hands", "openhands"),
    ] {
        let search = json_output(ctx(&temp).args([
            "search",
            "anything",
            "--provider",
            provider,
            "--refresh",
            "off",
            "--json",
        ]));
        assert_eq!(search["filters"]["provider"], expected);
    }
}

pub(crate) fn write_native_copilot_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp
        .path()
        .join("native-copilot/session-state/copilot-cli-native");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "id": "copilot-cli-native-start",
                "timestamp": "2026-06-24T12:00:00Z",
                "type": "session.start",
                "data": {
                    "sessionId": "copilot-cli-native",
                    "startTime": "2026-06-24T12:00:00Z",
                    "selectedModel": "gpt-5-mini",
                    "context": {"cwd": "/workspace"}
                }
            }),
            json!({
                "id": "copilot-cli-native-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "type": "user.message",
                "data": {"content": query}
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-copilot/session-state")
        .to_str()
        .unwrap()
        .to_owned()
}
