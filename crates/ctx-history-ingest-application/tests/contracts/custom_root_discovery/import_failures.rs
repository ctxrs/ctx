use super::*;

#[cfg(unix)]
#[test]
fn native_directory_roots_preserve_history_through_relocation() {
    for (provider, stored_provider, fixture) in [
        (
            "claude",
            "claude",
            write_native_claude_fixture as fn(&TempDir, &str) -> String,
        ),
        (
            "cursor",
            "cursor",
            write_native_cursor_fixture as fn(&TempDir, &str) -> String,
        ),
        ("gemini", "gemini", write_native_gemini_fixture),
        ("mux", "mux", write_native_mux_fixture),
        (
            "mistral-vibe",
            "mistral_vibe",
            write_native_mistral_vibe_fixture,
        ),
    ] {
        let temp = daemon_test_root();
        let query = format!("{provider} relocated root oracle");
        let root = PathBuf::from(fixture(&temp, &query));
        assert!(root.is_dir(), "{provider}: directory fixture");
        let import = || {
            json_output(ctx(&temp).args([
                "import",
                "--provider",
                provider,
                "--path",
                root.to_str().unwrap(),
                "--no-blame",
                "--format=json",
                "--progress",
                "none",
            ]))
        };
        let first = import();
        wait_for_projection(&temp, &first);
        let counts = provider_core_counts(&data_root(&temp), stored_provider);
        assert!(counts.0 > 0 && counts.1 > 0, "{provider}: {counts:?}");
        let moved = temp.path().join("relocated");
        fs::rename(&root, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &root).unwrap();
        let replay = import();
        wait_for_projection(&temp, &replay);
        assert_eq!(
            replay["totals"]["failed_sources"], 0,
            "{provider}: {}",
            replay["totals"]
        );
        assert_eq!(
            provider_core_counts(&data_root(&temp), stored_provider),
            counts,
            "{provider}"
        );
        let search = json_output(ctx(&temp).args([
            "search",
            &query,
            "--provider",
            provider,
            "--refresh",
            "off",
            "--limit",
            "1",
            "--format=json",
        ]));
        assert_search_provider_oracle(&search, stored_provider, &query, 1, "message");
        if matches!(provider, "mux" | "mistral-vibe") {
            let leaf = moved.join(if provider == "mux" {
                "mux-cli-native/chat.jsonl"
            } else {
                "session_20260704_160000_vibecli/messages.jsonl"
            });
            let original = temp.path().join("outside-transcript.jsonl");
            fs::rename(&leaf, &original).unwrap();
            std::os::unix::fs::symlink(&original, &leaf).unwrap();
            let output = ctx(&temp)
                .args([
                    "import",
                    "--provider",
                    provider,
                    "--path",
                    root.to_str().unwrap(),
                    "--no-blame",
                    "--format=json",
                    "--progress",
                    "none",
                ])
                .output()
                .unwrap();
            assert!(
                !output.status.success(),
                "failed source refresh must exit nonzero"
            );
            if !output.stdout.is_empty() {
                let rejected: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(rejected["outcome"], "completed_with_source_failures");
                assert!(rejected["totals"]["failed_sources"].as_u64().unwrap() > 0);
            } else {
                let failure = String::from_utf8(output.stderr).unwrap();
                assert!(
                    failure.contains("symlink") || failure.contains("is not importable"),
                    "{provider}: {failure}"
                );
            }
            assert_eq!(
                provider_core_counts(&data_root(&temp), stored_provider),
                counts,
                "{provider}"
            );
        }
    }
}
