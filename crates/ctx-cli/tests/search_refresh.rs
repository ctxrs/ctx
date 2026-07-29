mod support;

use ctx_history_index::GenerationManifest;
use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
    time::SystemTime,
};
use support::*;

struct SourceRefreshDaemon {
    child: Option<Child>,
}

impl SourceRefreshDaemon {
    fn pid(&self) -> u32 {
        self.child.as_ref().expect("running daemon child").id()
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for SourceRefreshDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn start_source_refresh_daemon(temp: &TempDir) -> SourceRefreshDaemon {
    fs::write(
        temp.path().join("config.toml"),
        "[daemon]\nenabled = true\nmode = \"source-refresh-only\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    let binary = copied_ctx_binary(temp);
    launch_source_refresh_daemon(temp, &binary)
}

fn restart_source_refresh_daemon(temp: &TempDir) -> SourceRefreshDaemon {
    let binary = temp.path().join(if cfg!(windows) {
        "ctx-test-copy.exe"
    } else {
        "ctx-test-copy"
    });
    assert!(
        binary.is_file(),
        "source-refresh restart binary is missing: {}",
        binary.display()
    );
    launch_source_refresh_daemon(temp, &binary)
}

fn launch_source_refresh_daemon(temp: &TempDir, binary: &Path) -> SourceRefreshDaemon {
    let prepared = ctx_from_binary(temp, binary);
    let mut command = StdCommand::new(prepared.get_program());
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    command
        .args([
            "daemon",
            "run",
            "--force",
            "--idle-exit-seconds",
            "600",
            "--loop-interval-seconds",
            "600",
        ])
        .env("CTX_DAEMON_MODE", "source-refresh-only")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let spawn_deadline = Instant::now() + Duration::from_secs(1);
    let child = loop {
        match command.spawn() {
            Ok(child) => break child,
            // Linux can briefly report ETXTBSY after the copied executable's
            // final write handle closes, especially when these tests launch
            // their isolated daemons in parallel.
            Err(error) if error.raw_os_error() == Some(26) && Instant::now() < spawn_deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("start isolated source-refresh daemon: {error}"),
        }
    };
    let mut daemon = SourceRefreshDaemon { child: Some(child) };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(exit) = daemon.child.as_mut().unwrap().try_wait().unwrap() {
            let mut stderr = String::new();
            daemon
                .child
                .as_mut()
                .unwrap()
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("source-refresh daemon exited before becoming ready ({exit}): {stderr}");
        }
        let status = ctx(temp)
            .args(["daemon", "status", "--format=json"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok());
        if status.as_ref().is_some_and(|status| {
            status["daemon"]["running"] == true
                && status["daemon"]["source_refresh_endpoint"]["available"] == true
        }) {
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for source-refresh daemon readiness: {status:#?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn assert_published_generation(search: &Value, expected_mode: &str) -> String {
    assert_eq!(search["freshness"]["mode"], expected_mode, "{search:#}");
    assert_eq!(search["freshness"]["status"], "completed", "{search:#}");
    assert_eq!(search["retrieval"]["index"], "source_backed", "{search:#}");
    search["retrieval"]["generation_id"]
        .as_str()
        .expect("search response should identify its source-backed generation")
        .to_owned()
}

fn wait_for_status(temp: &TempDir, description: &str, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = json_output(ctx(temp).args(["status", "--format=json"]));
        if predicate(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}: {status:#}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn assert_source_generation_ready(temp: &TempDir, expected_generation: &str) -> Value {
    let status = wait_for_status(temp, "published source generation status", |status| {
        status["history_epoch"]["status"] == "ready"
            && status["lexical"]["generation_id"] == expected_generation
            && status["refresh"]["published_generation"] == expected_generation
    });
    assert_eq!(status["history_epoch"]["status"], "ready", "{status:#}");
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    assert_eq!(
        status["lexical"]["generation_id"], expected_generation,
        "{status:#}"
    );
    assert_eq!(
        status["lexical"]["request_state"], "published",
        "{status:#}"
    );
    assert_eq!(
        status["lexical"]["published_generation"], expected_generation,
        "{status:#}"
    );
    assert_eq!(status["lexical"]["generation_matches"], true, "{status:#}");
    assert_eq!(status["refresh"]["status"], "ready", "{status:#}");
    assert_eq!(
        status["refresh"]["published_generation"], expected_generation,
        "{status:#}"
    );
    assert_eq!(status["refresh"]["generation_matches"], true, "{status:#}");
    assert_eq!(status["resolver"]["status"], "ready", "{status:#}");
    assert_eq!(
        status["resolver"]["generation_id"], expected_generation,
        "{status:#}"
    );
    assert_eq!(status["resolver"]["generation_matches"], true, "{status:#}");
    assert_eq!(
        status["indexed_events"], status["lexical"]["indexed_documents"],
        "{status:#}"
    );
    assert_eq!(
        status["indexed_sources"], status["lexical"]["certified_sources"],
        "{status:#}"
    );
    assert_eq!(status["prior_epoch"]["present"], false, "{status:#}");
    assert_eq!(status["prior_epoch"]["opened"], false, "{status:#}");
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "source-backed search/status must not create the previous-epoch Store"
    );
    status
}

fn assert_source_backed_search_show_oracle(
    temp: &TempDir,
    packet: &Value,
    provider: &str,
    query: &str,
    expected_results: usize,
    expected_event_type: &str,
) {
    assert_eq!(packet["schema_version"], 1, "{packet:#}");
    assert_eq!(packet["payload_type"], "search_results", "{packet:#}");
    assert_eq!(packet["query"], query, "{packet:#}");
    assert_eq!(packet["filters"]["provider"], provider, "{packet:#}");
    assert_eq!(packet["retrieval"]["index"], "source_backed", "{packet:#}");
    let generation = packet["retrieval"]["generation_id"]
        .as_str()
        .expect("source-backed search generation");
    assert_eq!(generation.len(), 64, "{packet:#}");
    assert!(
        generation
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "generation ID must be lowercase hexadecimal: {packet:#}"
    );

    let results = packet["results"]
        .as_array()
        .expect("source-backed search results");
    assert_eq!(
        results.len(),
        expected_results,
        "unexpected source-backed result count: {packet:#}"
    );
    for result in results {
        assert_eq!(result["provider"], provider, "{result:#}");
        assert_eq!(result["result_type"], "session_result", "{result:#}");
        assert_eq!(result["result_scope"], "session", "{result:#}");
        assert_eq!(result["item_id"], result["ctx_session_id"], "{result:#}");
        assert_eq!(result["session_id"], result["ctx_session_id"], "{result:#}");
        assert_eq!(result["event_id"], result["ctx_event_id"], "{result:#}");
        assert!(result["ctx_event_id"].is_string(), "{result:#}");
        assert!(result["ctx_session_id"].is_string(), "{result:#}");
        assert!(result["provider_session_id"].is_string(), "{result:#}");
        assert!(result["source_format"].is_string(), "{result:#}");
        assert!(result["source_path"].is_string(), "{result:#}");
        assert!(result["session_importance"].is_number(), "{result:#}");
        assert!(result["more_matches_in_session"].is_number(), "{result:#}");
        assert!(
            result["title"]
                .as_str()
                .is_some_and(|title| title.contains(expected_event_type)),
            "{result:#}"
        );
        assert!(
            result["snippet"]
                .as_str()
                .is_some_and(|snippet| snippet.contains(query)),
            "{result:#}"
        );
        assert_eq!(
            result.get("source_exists"),
            None,
            "source-backed results must not expose a live-path Store oracle: {result:#}"
        );
        assert_eq!(
            result.get("why_matched"),
            None,
            "source-backed results use the indexed event type and hydrated snippet: {result:#}"
        );

        let commands = result["suggested_next_commands"]
            .as_array()
            .expect("source-backed next commands");
        let event_id = result["ctx_event_id"].as_str().unwrap();
        let session_id = result["ctx_session_id"].as_str().unwrap();
        assert!(
            commands
                .iter()
                .any(|command| command == &format!("ctx show event {event_id} --window 10")),
            "{result:#}"
        );
        assert!(
            commands
                .iter()
                .any(|command| command == &format!("ctx show session {session_id}")),
            "{result:#}"
        );
        assert!(
            commands.iter().any(|command| {
                command
                    .as_str()
                    .is_some_and(|command| command.starts_with("ctx search "))
                    && command
                        .as_str()
                        .unwrap()
                        .contains(&format!(" --session {session_id}"))
            }),
            "{result:#}"
        );

        let citations = result["citations"]
            .as_array()
            .expect("source-backed citations");
        assert_eq!(citations.len(), 1, "{result:#}");
        let citation = &citations[0];
        assert_eq!(citation["item_id"], result["ctx_event_id"], "{result:#}");
        assert_eq!(citation["target_type"], "event", "{result:#}");
        assert_eq!(
            citation["ctx_event_id"], result["ctx_event_id"],
            "{result:#}"
        );
        assert_eq!(
            citation["ctx_session_id"], result["ctx_session_id"],
            "{result:#}"
        );
        assert_eq!(citation["provider"], provider, "{result:#}");
        assert_eq!(citation["source_path"], result["source_path"], "{result:#}");
        assert_eq!(
            citation.get("source_exists"),
            None,
            "generation-bound citations must not probe the mutable source path: {result:#}"
        );

        let shown_event = json_output(ctx(temp).args([
            "show", "event", event_id, "--window", "1", "--format", "json",
        ]));
        assert_eq!(
            shown_event["payload_type"], "event_window",
            "{shown_event:#}"
        );
        assert_eq!(shown_event["ctx_event_id"], event_id, "{shown_event:#}");
        assert_eq!(shown_event["ctx_session_id"], session_id, "{shown_event:#}");
        assert_eq!(
            shown_event["event"]["provider"], provider,
            "{shown_event:#}"
        );
        assert_eq!(
            shown_event["event"]["source_path"], result["source_path"],
            "{shown_event:#}"
        );
        assert_eq!(
            shown_event["event"]["content"]["origin"], "provider_source",
            "{shown_event:#}"
        );
        assert_eq!(
            shown_event["event"]["content"]["source_verified"], true,
            "{shown_event:#}"
        );
        assert!(
            shown_event["event"]["text"]
                .as_str()
                .is_some_and(|text| text.contains(query)),
            "{shown_event:#}"
        );

        let shown_session =
            json_output(ctx(temp).args(["show", "session", session_id, "--format", "json"]));
        assert_eq!(
            shown_session["payload_type"], "session_transcript",
            "{shown_session:#}"
        );
        assert_eq!(
            shown_session["ctx_session_id"], session_id,
            "{shown_session:#}"
        );
        assert_eq!(shown_session["provider"], provider, "{shown_session:#}");
        assert!(
            shown_session["events"]
                .as_array()
                .is_some_and(|events| events.iter().any(|event| {
                    event["ctx_event_id"] == result["ctx_event_id"]
                        && event["content"]["origin"] == "provider_source"
                        && event["content"]["source_verified"] == true
                        && event["text"]
                            .as_str()
                            .is_some_and(|text| text.contains(query))
                })),
            "{shown_session:#}"
        );
    }
}

fn generation_id(search: &Value) -> &str {
    search["retrieval"]["generation_id"]
        .as_str()
        .expect("search response should identify its source-backed generation")
}

fn generation_manifest(temp: &TempDir, generation: &str) -> (GenerationManifest, Value) {
    let path = temp
        .path()
        .join("search/lexical/ctx-generations")
        .join(format!("{generation}.json"));
    let bytes = fs::read(&path)
        .unwrap_or_else(|error| panic!("read generation manifest {}: {error}", path.display()));
    let manifest: GenerationManifest = serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse generation manifest {}: {error}", path.display()));
    assert_eq!(
        manifest.generation_id().unwrap(),
        generation,
        "manifest digest must be the published generation ID"
    );
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    (manifest, value)
}

fn generation_manifest_paths(temp: &TempDir) -> Vec<PathBuf> {
    let directory = temp.path().join("search/lexical/ctx-generations");
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths = entries
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

#[derive(Debug, Eq, PartialEq)]
struct PublishedFileState {
    bytes: Vec<u8>,
    modified: SystemTime,
    #[cfg(unix)]
    inode: u64,
}

fn published_file_state(path: &Path) -> PublishedFileState {
    let metadata = fs::metadata(path)
        .unwrap_or_else(|error| panic!("read publication metadata {}: {error}", path.display()));
    PublishedFileState {
        bytes: fs::read(path)
            .unwrap_or_else(|error| panic!("read publication file {}: {error}", path.display())),
        modified: metadata.modified().unwrap(),
        #[cfg(unix)]
        inode: {
            use std::os::unix::fs::MetadataExt as _;

            metadata.ino()
        },
    }
}

fn assert_published_file_unchanged(path: &Path, expected: &PublishedFileState) {
    let actual = published_file_state(path);
    assert_eq!(
        actual.bytes,
        expected.bytes,
        "publication bytes changed at {}",
        path.display()
    );
    assert_eq!(
        actual.modified,
        expected.modified,
        "publication mtime changed at {}",
        path.display()
    );
    #[cfg(unix)]
    assert_eq!(
        actual.inode,
        expected.inode,
        "publication inode changed at {}",
        path.display()
    );
}

fn tantivy_meta_facts(state: &PublishedFileState) -> (u64, Vec<String>) {
    let value: Value = serde_json::from_slice(&state.bytes).unwrap();
    let opstamp = value["opstamp"].as_u64().expect("Tantivy meta opstamp");
    let mut segments = value["segments"]
        .as_array()
        .expect("Tantivy meta segments")
        .iter()
        .map(|segment| {
            segment["segment_id"]
                .as_str()
                .expect("Tantivy segment ID")
                .to_owned()
        })
        .collect::<Vec<_>>();
    segments.sort();
    (opstamp, segments)
}

fn assert_daemon_publication(
    temp: &TempDir,
    expected_generation: &str,
    expected_route_count: u64,
    expected_providers: &[&str],
) -> Value {
    let expected_source_count = expected_providers.len() as u64;
    let status = assert_source_generation_ready(temp, expected_generation);
    let job = &status["daemon"]["jobs"]["source_backed_refresh"];
    assert_eq!(job["owner"], "daemon", "{status:#}");
    assert_eq!(job["daemon_mode"], "source-refresh-only", "{status:#}");
    assert_eq!(job["trigger"], "search", "{status:#}");
    assert_eq!(job["trigger_provenance"], "manual", "{status:#}");
    assert_eq!(job["request_state"], "published", "{status:#}");
    assert_eq!(job["status"], "completed", "{status:#}");
    assert_eq!(
        job["published_generation"], expected_generation,
        "{status:#}"
    );
    assert_eq!(job["source_count"], expected_route_count, "{status:#}");
    assert_eq!(job["progress"]["phase"], "published", "{status:#}");
    assert_eq!(
        job["progress"]["completed_sources"], expected_route_count,
        "{status:#}"
    );
    assert_eq!(
        job["progress"]["total_sources"], expected_route_count,
        "{status:#}"
    );
    assert_eq!(
        job["certified_source_count"], expected_source_count,
        "{status:#}"
    );
    assert_eq!(
        status["lexical"]["certified_sources"], expected_source_count,
        "{status:#}"
    );
    assert_eq!(
        status["indexed_sources"], expected_source_count,
        "{status:#}"
    );
    assert!(
        job["request_id"].as_str().is_some_and(|id| !id.is_empty()),
        "{status:#}"
    );
    for stage in ["discovery", "scan_stage", "commit"] {
        assert!(
            job["timings_us"][stage]
                .as_u64()
                .is_some_and(|duration| duration > 0),
            "missing {stage} timing: {status:#}"
        );
    }

    let (manifest, manifest_value) = generation_manifest(temp, expected_generation);
    assert_eq!(
        manifest.indexed_documents, status["lexical"]["indexed_documents"],
        "{status:#}"
    );
    assert_eq!(
        manifest.certified_source_bytes, status["lexical"]["certified_source_bytes"],
        "{status:#}"
    );
    assert_eq!(
        manifest.certified_source_bytes, job["certified_source_bytes"],
        "{status:#}"
    );
    assert_eq!(manifest.sources.len(), expected_providers.len());
    let mut actual_providers = manifest_value["sources"]
        .as_array()
        .expect("generation manifest sources")
        .iter()
        .map(|source| {
            source["observation"]["source"]["provider"]
                .as_str()
                .expect("certified source provider")
        })
        .collect::<Vec<_>>();
    actual_providers.sort_unstable();
    let mut expected_providers = expected_providers.to_vec();
    expected_providers.sort_unstable();
    assert_eq!(
        actual_providers, expected_providers,
        "published generation must retain the expected provider provenance"
    );
    status
}

fn assert_daemon_refresh_failure(
    temp: &TempDir,
    expected_route_count: u64,
    retained_generation: Option<&str>,
) -> Value {
    let status = wait_for_status(temp, "terminal source refresh failure", |status| {
        status["refresh"]["status"] == "unavailable"
            && status["daemon"]["jobs"]["source_backed_refresh"]["status"] == "failed"
            && status["daemon"]["jobs"]["source_backed_refresh"]["request_state"] == "failed"
    });
    assert_eq!(status["refresh"]["status"], "unavailable", "{status:#}");
    assert_eq!(
        status["refresh"]["reason"], "source_refresh_failed",
        "{status:#}"
    );
    assert_eq!(status["refresh"]["request_state"], "failed", "{status:#}");
    let job = &status["daemon"]["jobs"]["source_backed_refresh"];
    assert_eq!(job["owner"], "daemon", "{status:#}");
    assert_eq!(job["daemon_mode"], "source-refresh-only", "{status:#}");
    assert_eq!(job["trigger"], "search", "{status:#}");
    assert_eq!(job["trigger_provenance"], "manual", "{status:#}");
    assert_eq!(job["status"], "failed", "{status:#}");
    assert_eq!(job["request_state"], "failed", "{status:#}");
    assert_eq!(job["source_count"], expected_route_count, "{status:#}");
    assert_eq!(
        job["progress"]["total_sources"], expected_route_count,
        "{status:#}"
    );
    assert_eq!(job["progress"]["phase"], "failed", "{status:#}");
    assert_eq!(
        job["published_generation"],
        json!(retained_generation),
        "{status:#}"
    );
    assert_eq!(job["generation_changed"], false, "{status:#}");
    assert!(
        job["request_id"].as_str().is_some_and(|id| !id.is_empty()),
        "{status:#}"
    );
    assert!(
        job["last_error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "{status:#}"
    );
    assert_eq!(status["prior_epoch"]["opened"], false, "{status:#}");
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "failed source-backed refresh must not create the previous-epoch Store"
    );
    status
}

fn append_codex_message(path: &Path, timestamp: &str, role: &str, text: &str) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    let content_type = if role == "user" {
        "input_text"
    } else {
        "output_text"
    };
    writeln!(
        file,
        "{}",
        json!({
            "timestamp": timestamp,
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": role,
                "content": [{"type": content_type, "text": text}]
            }
        })
    )
    .unwrap();
}

fn write_codex_session(path: &Path, native_session_id: &str, messages: &[(&str, &str, &str)]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut lines = vec![serde_json::to_string(&json!({
        "timestamp": "2026-07-29T12:00:00.000Z",
        "type": "session_meta",
        "payload": {
            "id": native_session_id,
            "timestamp": "2026-07-29T12:00:00.000Z",
            "cwd": "/repo/lifecycle",
            "originator": "codex-cli",
            "cli_version": "0.200.0",
            "source": "cli",
            "model_provider": "openai"
        }
    }))
    .unwrap()];
    for (timestamp, role, text) in messages {
        let content_type = if *role == "user" {
            "input_text"
        } else {
            "output_text"
        };
        lines.push(
            serde_json::to_string(&json!({
                "timestamp": timestamp,
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": role,
                    "content": [{"type": content_type, "text": text}]
                }
            }))
            .unwrap(),
        );
    }
    fs::write(path, lines.join("\n") + "\n").unwrap();
}

#[test]
fn search_refresh_exact_noop_skips_publication_and_tiny_append_is_one_document() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let sessions = temp.path().join(".codex/sessions");
    copy_dir_all(&fixture, &sessions);
    let appended_source = sessions.join("2026/06/23/root.jsonl");
    let _daemon = start_source_refresh_daemon(&temp);

    let initial = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &initial, "codex", "onboarding", 1, "message");
    let initial_generation = assert_published_generation(&initial, "wait");
    let initial_documents = initial["retrieval"]["indexed_documents"].as_u64().unwrap();
    let initial_status =
        assert_daemon_publication(&temp, &initial_generation, 1, &["codex", "codex"]);
    let initial_job = &initial_status["daemon"]["jobs"]["source_backed_refresh"];
    assert_eq!(initial_job["generation_changed"], true, "{initial_job:#}");
    let initial_current = initial_job["receipt"]["current"].clone();

    let index_root = temp.path().join("search/lexical");
    let meta_path = index_root.join("meta.json");
    let manifest_path = index_root
        .join("ctx-generations")
        .join(format!("{initial_generation}.json"));
    let initial_meta = published_file_state(&meta_path);
    let initial_manifest = published_file_state(&manifest_path);
    let initial_manifests = generation_manifest_paths(&temp);
    let (initial_opstamp, initial_segments) = tantivy_meta_facts(&initial_meta);
    assert!(!initial_segments.is_empty());

    let unchanged = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(generation_id(&unchanged), initial_generation);
    assert_eq!(
        unchanged["retrieval"]["indexed_documents"], initial_documents,
        "{unchanged:#}"
    );
    let unchanged_status =
        assert_daemon_publication(&temp, &initial_generation, 1, &["codex", "codex"]);
    let unchanged_job = &unchanged_status["daemon"]["jobs"]["source_backed_refresh"];
    assert_eq!(
        unchanged_job["generation_changed"], false,
        "{unchanged_job:#}"
    );
    assert_eq!(
        unchanged_job["receipt"]["current"], initial_current,
        "{unchanged_job:#}"
    );
    assert_published_file_unchanged(&meta_path, &initial_meta);
    assert_published_file_unchanged(&manifest_path, &initial_manifest);
    assert_eq!(generation_manifest_paths(&temp), initial_manifests);
    assert_eq!(
        tantivy_meta_facts(&published_file_state(&meta_path)),
        (initial_opstamp, initial_segments.clone())
    );

    let source_bytes_before = fs::metadata(&appended_source).unwrap().len();
    let append_query = "canonical tiny append refresh oracle";
    append_codex_message(
        &appended_source,
        "2026-06-23T15:00:08.000Z",
        "assistant",
        append_query,
    );
    let appended_bytes = fs::metadata(&appended_source).unwrap().len() - source_bytes_before;
    assert!(appended_bytes > 0);

    let appended = json_output(ctx(&temp).args([
        "search",
        append_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &appended, "codex", append_query, 1, "message");
    let append_generation = assert_published_generation(&appended, "wait");
    assert_ne!(append_generation, initial_generation);
    assert_eq!(
        appended["retrieval"]["indexed_documents"],
        initial_documents + 1,
        "{appended:#}"
    );
    let append_status =
        assert_daemon_publication(&temp, &append_generation, 1, &["codex", "codex"]);
    let append_job = &append_status["daemon"]["jobs"]["source_backed_refresh"];
    assert_eq!(append_job["generation_changed"], true, "{append_job:#}");
    let append_current = &append_job["receipt"]["current"];
    assert_eq!(
        append_current["current_indexed_documents"].as_u64(),
        initial_current["current_indexed_documents"]
            .as_u64()
            .map(|count| count + 1)
    );
    assert_eq!(
        append_current["current_complete_records"].as_u64(),
        initial_current["current_complete_records"]
            .as_u64()
            .map(|count| count + 1)
    );
    assert_eq!(
        append_current["current_retained_records"].as_u64(),
        initial_current["current_retained_records"]
            .as_u64()
            .map(|count| count + 1)
    );
    assert_eq!(
        append_current["current_certified_source_bytes"].as_u64(),
        initial_current["current_certified_source_bytes"]
            .as_u64()
            .map(|bytes| bytes + appended_bytes)
    );

    let append_meta = published_file_state(&meta_path);
    let (append_opstamp, append_segments) = tantivy_meta_facts(&append_meta);
    assert!(append_opstamp > initial_opstamp);
    assert!(
        append_segments.len() < initial_segments.len(),
        "the existing Tantivy merge policy should coalesce the fixture's tiny append: \
         before={initial_segments:?}, after={append_segments:?}"
    );
    assert_eq!(
        generation_manifest_paths(&temp).len(),
        initial_manifests.len() + 1
    );
}

#[test]
fn search_refreshes_discovered_codex_sessions_before_query() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);
    let _daemon = start_source_refresh_daemon(&temp);

    let search = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &search, "codex", "onboarding", 1, "message");
    assert_eq!(search["freshness"]["source_count"], 1);
    let generation = assert_published_generation(&search, "wait");
    let status = assert_daemon_publication(&temp, &generation, 1, &["codex", "codex"]);
    assert!(
        status["lexical"]["indexed_documents"].as_u64().unwrap() >= 2,
        "{status:#}"
    );
}

#[test]
fn search_refreshes_discovered_codex_prompt_history_before_query() {
    let temp = tempdir();
    let history = temp.path().join(".codex/history.jsonl");
    fs::create_dir_all(history.parent().unwrap()).unwrap();
    fs::write(
        &history,
        concat!(
            r#"{"session_id":"prompt-refresh-session","ts":1784371200,"text":"prompt history search refresh oracle"}"#,
            "\n"
        ),
    )
    .unwrap();
    let _daemon = start_source_refresh_daemon(&temp);

    let search = json_output(ctx(&temp).args([
        "search",
        "prompt history search refresh oracle",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(
        &temp,
        &search,
        "codex",
        "prompt history search refresh oracle",
        1,
        "message",
    );
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["retrieval"]["indexed_documents"], 1);
    let generation = assert_published_generation(&search, "wait");
    assert_daemon_publication(&temp, &generation, 1, &["codex"]);
}

#[test]
fn machine_readable_default_search_reports_daemon_unavailable_without_autostart() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    copy_dir_all(&fixture, &temp.path().join(".codex").join("sessions"));
    let missing_exe = temp.path().join("missing-ctx-binary");

    let output = ctx(&temp)
        .args([
            "search",
            "onboarding",
            "--provider",
            "codex",
            "--format=json",
        ])
        .env("CTX_DAEMON_AUTOSTART_EXE", &missing_exe)
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(
            "the ctx daemon is unavailable for source-backed refresh; no foreground writer was started"
        ),
        "{stderr}"
    );
    assert!(!temp.path().join("daemon/status.json").exists());
    assert!(!temp.path().join("search/lexical/meta.json").exists());
    assert!(!temp.path().join("work.sqlite").exists());

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(
        status["history_epoch"]["status"], "unavailable",
        "{status:#}"
    );
    assert_eq!(status["lexical"]["status"], "unavailable", "{status:#}");
    assert_eq!(status["refresh"]["status"], "unavailable", "{status:#}");
    assert_eq!(
        status["refresh"]["reason"], "daemon_unavailable",
        "{status:#}"
    );
    assert_eq!(status["daemon"]["running"], false, "{status:#}");
    assert_eq!(
        status["daemon"]["source_refresh_endpoint"]["available"], false,
        "{status:#}"
    );
    assert_eq!(status["prior_epoch"]["present"], false, "{status:#}");
    assert_eq!(status["prior_epoch"]["opened"], false, "{status:#}");
}

#[test]
fn search_refresh_wait_skips_malformed_jsonl_rows() {
    let temp = tempdir();
    write_malformed_claude_session(&temp);
    let _daemon = start_source_refresh_daemon(&temp);

    let output = ctx(&temp)
        .args([
            "search",
            "rejected refresh search marker",
            "--provider",
            "claude",
            "--refresh",
            "wait",
            "--format=json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let search: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_source_backed_search_show_oracle(
        &temp,
        &search,
        "claude",
        "rejected refresh search marker",
        1,
        "message",
    );
    assert_eq!(search["retrieval"]["indexed_documents"], 2, "{search:#}");
    let generation = assert_published_generation(&search, "wait");
    assert_daemon_publication(&temp, &generation, 1, &["claude"]);

    let later_valid_row = json_output(ctx(&temp).args([
        "search",
        "valid rows remain searchable",
        "--provider",
        "claude",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(
        &temp,
        &later_valid_row,
        "claude",
        "valid rows remain searchable",
        1,
        "message",
    );
    assert_eq!(
        later_valid_row["freshness"]["status"],
        "existing_generation"
    );
    assert_eq!(generation_id(&later_valid_row), generation);
}

#[test]
fn search_refresh_wait_human_output_uses_daemon_job_progress_without_stderr_noise() {
    let temp = tempdir();
    write_malformed_claude_session(&temp);
    let _daemon = start_source_refresh_daemon(&temp);

    let output = ctx(&temp)
        .args([
            "search",
            "rejected refresh search marker",
            "--provider",
            "claude",
            "--refresh",
            "wait",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("rejected refresh search marker"),
        "{stdout}"
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    let generation = status["lexical"]["generation_id"]
        .as_str()
        .expect("human search should publish a source-backed generation")
        .to_owned();
    let status = assert_daemon_publication(&temp, &generation, 1, &["claude"]);
    let job = &status["daemon"]["jobs"]["source_backed_refresh"];
    assert_eq!(job["progress"]["phase"], "published", "{status:#}");
}

fn write_malformed_claude_session(temp: &TempDir) {
    let project = temp.path().join(".claude").join("projects").join("-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("claude-session.jsonl"),
        concat!(
            r#"{"sessionId":"claude-session","timestamp":"2026-06-24T10:00:00Z","cwd":"/repo","version":"test","type":"user","message":{"role":"user","content":[{"type":"text","text":"rejected refresh search marker"}]},"uuid":"claude-user"}"#,
            "\n",
            "{malformed-jsonl-row\n",
            r#"{"sessionId":"claude-session","timestamp":"2026-06-24T10:00:01Z","cwd":"/repo","version":"test","type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"valid rows remain searchable"}]},"uuid":"claude-assistant"}"#,
            "\n"
        ),
    )
    .unwrap();
}

#[test]
fn search_refresh_off_serves_published_generation_without_refreshing_sources() {
    let temp = tempdir();
    let history = temp.path().join(".codex/history.jsonl");
    fs::create_dir_all(history.parent().unwrap()).unwrap();
    fs::write(
        &history,
        concat!(
            r#"{"session_id":"off-refresh-session","ts":1784371200,"text":"published off mode oracle"}"#,
            "\n"
        ),
    )
    .unwrap();
    let mut daemon = start_source_refresh_daemon(&temp);

    let initial = json_output(ctx(&temp).args([
        "search",
        "published off mode oracle",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    let published_generation = assert_published_generation(&initial, "wait");
    assert_daemon_publication(&temp, &published_generation, 1, &["codex"]);

    let mut file = fs::OpenOptions::new().append(true).open(&history).unwrap();
    writeln!(
        file,
        r#"{{"session_id":"off-refresh-session","ts":1784371201,"text":"unpublished off mode oracle"}}"#
    )
    .unwrap();
    drop(file);

    let off = json_output(ctx(&temp).args([
        "search",
        "unpublished off mode oracle",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(off["freshness"]["mode"], "off");
    assert_eq!(off["freshness"]["status"], "existing_generation");
    assert_eq!(off["freshness"]["source_count"], 0);
    assert_eq!(generation_id(&off), published_generation);
    assert!(off["results"].as_array().unwrap().is_empty(), "{off:#}");

    daemon.stop();
    let unavailable = json_output(ctx(&temp).args([
        "search",
        "unpublished off mode oracle",
        "--provider",
        "codex",
        "--refresh",
        "background",
        "--format=json",
    ]));
    assert_eq!(unavailable["freshness"]["mode"], "background");
    assert_eq!(unavailable["freshness"]["status"], "daemon_unavailable");
    assert_eq!(generation_id(&unavailable), published_generation);
    assert!(
        unavailable["results"].as_array().unwrap().is_empty(),
        "{unavailable:#}"
    );
}

#[test]
fn search_refresh_wait_recovers_after_invalid_source_is_removed() {
    let temp = tempdir();
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/07/12");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("rollout-empty-session.jsonl"), "").unwrap();
    let query = "pi-later-good-refresh-oracle";
    install_default_pi_fixture(&temp, query);
    let _daemon = start_source_refresh_daemon(&temp);

    let stderr =
        failure_stderr(ctx(&temp).args(["search", query, "--refresh", "wait", "--format=json"]));
    assert!(
        stderr.contains("daemon-owned source-backed refresh failed"),
        "{stderr}"
    );
    assert!(
        stderr.contains("source-backed scan failed for codex"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Codex source certificate has no NativePath checkpoint frontier"),
        "{stderr}"
    );
    assert!(
        temp.path().join("search/lexical/meta.json").is_file(),
        "a failed cold scan may initialize disposable Tantivy metadata"
    );
    assert!(
        generation_manifest_paths(&temp).is_empty(),
        "a failed cold scan must not publish a generation manifest"
    );
    let uncommitted = failure_stderr(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        uncommitted
            .contains("the source-backed index does not exist; retry with daemon refresh enabled"),
        "{uncommitted}"
    );
    let failed = assert_daemon_refresh_failure(&temp, 2, None);
    assert_eq!(
        failed["history_epoch"]["reason"], "source_rebuild_failed",
        "{failed:#}"
    );
    assert_eq!(failed["lexical"]["status"], "unavailable", "{failed:#}");

    fs::remove_dir_all(temp.path().join(".codex")).unwrap();
    let recovered_output = ctx(&temp)
        .args([
            "search",
            query,
            "--provider",
            "pi",
            "--refresh",
            "wait",
            "--format=json",
        ])
        .output()
        .unwrap();
    assert!(
        recovered_output.status.success(),
        "recovery search failed:\n{}\nstatus:\n{:#}\ngeneration manifests: {:#?}",
        String::from_utf8_lossy(&recovered_output.stderr),
        json_output(ctx(&temp).args(["status", "--format=json"])),
        generation_manifest_paths(&temp),
    );
    let recovered: Value = serde_json::from_slice(&recovered_output.stdout).unwrap();
    assert_source_backed_search_show_oracle(&temp, &recovered, "pi", query, 1, "message");
    let generation = assert_published_generation(&recovered, "wait");
    assert_daemon_publication(&temp, &generation, 1, &["pi"]);
}

#[test]
fn source_refresh_daemon_stop_start_resumes_exact_generation() {
    let temp = tempdir();
    let query = "pi-daemon-restart-resume-oracle";
    install_default_pi_fixture(&temp, query);
    let mut daemon = start_source_refresh_daemon(&temp);
    let first_pid = daemon.pid();

    let initial = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &initial, "pi", query, 1, "message");
    let generation = assert_published_generation(&initial, "wait");
    assert_daemon_publication(&temp, &generation, 1, &["pi"]);

    daemon.stop();
    let stopped = wait_for_status(&temp, "stopped source-refresh daemon", |status| {
        status["daemon"]["running"] == false
    });
    assert_eq!(stopped["daemon"]["running"], false, "{stopped:#}");
    let offline = failure_stderr(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        offline.contains("resolver_service_unavailable/temporarily_unavailable"),
        "{offline}"
    );
    assert!(
        offline.contains("no provider rediscovery or stored preview fallback"),
        "{offline}"
    );

    let restarted = restart_source_refresh_daemon(&temp);
    assert_ne!(restarted.pid(), first_pid);
    let resumed = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &resumed, "pi", query, 1, "message");
    assert_eq!(assert_published_generation(&resumed, "wait"), generation);
    assert_daemon_publication(&temp, &generation, 1, &["pi"]);
}

#[test]
fn search_refresh_invalid_source_failure_retains_last_published_generation() {
    let temp = tempdir();
    let query = "pi-retained-generation-oracle";
    install_default_pi_fixture(&temp, query);
    let _daemon = start_source_refresh_daemon(&temp);

    let initial = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    let initial_generation = assert_published_generation(&initial, "wait");
    assert_daemon_publication(&temp, &initial_generation, 1, &["pi"]);

    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/07/12");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("rollout-empty-session.jsonl"), "").unwrap();
    let stderr = failure_stderr(ctx(&temp).args([
        "search",
        "anything",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert!(
        stderr.contains("source-backed scan failed for codex"),
        "{stderr}"
    );
    assert!(stderr.contains("retained generation"), "{stderr}");
    assert!(stderr.contains(&initial_generation), "{stderr}");

    let failed = assert_daemon_refresh_failure(&temp, 2, Some(&initial_generation));
    assert_eq!(failed["history_epoch"]["status"], "ready", "{failed:#}");
    assert_eq!(failed["lexical"]["status"], "unavailable", "{failed:#}");
    assert_eq!(
        failed["lexical"]["reason"], "source_refresh_failed",
        "{failed:#}"
    );
    assert_eq!(
        failed["lexical"]["generation_id"], initial_generation,
        "{failed:#}"
    );

    let retained = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &retained, "pi", query, 1, "message");
    assert_eq!(retained["freshness"]["status"], "existing_generation");
    assert_eq!(generation_id(&retained), initial_generation);
}

#[test]
fn search_refresh_imports_fresh_work_after_large_source_backed_generation() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);
    let root_session = discovered.join("2026/06/23/root.jsonl");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&root_session)
        .unwrap();
    for index in 0..10_000 {
        writeln!(
            file,
            "{}",
            json!({
                "timestamp": "2026-06-23T15:00:00.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": format!("large-source-backed-baseline-{index}")
                    }]
                }
            })
        )
        .unwrap();
    }
    drop(file);
    let _daemon = start_source_refresh_daemon(&temp);

    let initial = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &initial, "codex", "onboarding", 1, "message");
    let initial_generation = assert_published_generation(&initial, "wait");
    let initial_documents = initial["retrieval"]["indexed_documents"].as_u64().unwrap();
    assert!(initial_documents >= 10_000, "{initial:#}");
    assert_daemon_publication(&temp, &initial_generation, 1, &["codex", "codex"]);

    let fresh_query = "fresh work after large source generation oracle";
    let history = temp.path().join(".codex/history.jsonl");
    fs::write(
        &history,
        format!(
            "{{\"session_id\":\"large-generation-fresh\",\"ts\":1784371200,\"text\":\"{fresh_query}\"}}\n"
        ),
    )
    .unwrap();
    let fresh = json_output(ctx(&temp).args([
        "search",
        fresh_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &fresh, "codex", fresh_query, 1, "message");
    assert_eq!(fresh["freshness"]["source_count"], 2);
    let fresh_generation = assert_published_generation(&fresh, "wait");
    assert_ne!(fresh_generation, initial_generation);
    assert!(
        fresh["retrieval"]["indexed_documents"].as_u64().unwrap() > initial_documents,
        "{fresh:#}"
    );
    assert_daemon_publication(&temp, &fresh_generation, 2, &["codex", "codex", "codex"]);
    assert!(!temp.path().join("work.sqlite").exists());
}

#[test]
fn search_refresh_codex_generation_covers_full_source_lifecycle() {
    let temp = tempdir();
    let sessions = temp.path().join(".codex").join("sessions");
    let session = sessions.join("2026/07/29/lifecycle.jsonl");
    let sibling_session = sessions.join("2026/07/29/sibling.jsonl");
    let native_session_id = "019fac90-0000-7000-8000-000000000001";
    let cold_query = "cold-source-lifecycle-oracle";
    write_codex_session(
        &session,
        native_session_id,
        &[("2026-07-29T12:00:01.000Z", "user", cold_query)],
    );
    write_codex_session(
        &sibling_session,
        "019fac90-0000-7000-8000-000000000002",
        &[(
            "2026-07-29T12:00:01.000Z",
            "assistant",
            "certified-deletion-sibling-oracle",
        )],
    );
    let _daemon = start_source_refresh_daemon(&temp);

    let cold = json_output(ctx(&temp).args([
        "search",
        cold_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &cold, "codex", cold_query, 1, "message");
    let cold_generation = assert_published_generation(&cold, "wait");
    assert_eq!(cold["retrieval"]["indexed_documents"], 2, "{cold:#}");
    let cold_status = assert_daemon_publication(&temp, &cold_generation, 1, &["codex", "codex"]);
    assert_eq!(
        cold_status["history_epoch"]["lexical_generation_id"], cold_generation,
        "{cold_status:#}"
    );
    let (cold_manifest, _) = generation_manifest(&temp, &cold_generation);
    assert_eq!(cold_manifest.sources.len(), 2);
    assert!(cold_manifest.removals.is_empty());

    let unchanged = json_output(ctx(&temp).args([
        "search",
        cold_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &unchanged, "codex", cold_query, 1, "message");
    assert_eq!(generation_id(&unchanged), cold_generation, "{unchanged:#}");
    let unchanged_status =
        assert_daemon_publication(&temp, &cold_generation, 1, &["codex", "codex"]);
    assert_eq!(
        unchanged_status["daemon"]["jobs"]["source_backed_refresh"]["generation_changed"], false,
        "{unchanged_status:#}"
    );

    let append_query = "append-source-lifecycle-oracle";
    append_codex_message(
        &session,
        "2026-07-29T12:00:02.000Z",
        "assistant",
        append_query,
    );
    let appended = json_output(ctx(&temp).args([
        "search",
        append_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &appended, "codex", append_query, 1, "message");
    let append_generation = assert_published_generation(&appended, "wait");
    assert_ne!(append_generation, cold_generation);
    assert_eq!(
        appended["retrieval"]["indexed_documents"], 3,
        "{appended:#}"
    );
    let append_status =
        assert_daemon_publication(&temp, &append_generation, 1, &["codex", "codex"]);
    assert_eq!(
        append_status["history_epoch"]["lexical_generation_id"], cold_generation,
        "{append_status:#}"
    );
    let (append_manifest, _) = generation_manifest(&temp, &append_generation);
    assert_eq!(append_manifest.sources.len(), 2);
    let append_source = append_manifest
        .sources
        .iter()
        .find(|source| source.counts().indexed_documents == 2)
        .expect("appended lifecycle source");
    let cold_source = cold_manifest
        .sources
        .iter()
        .find(|source| source.observation().source() == append_source.observation().source())
        .expect("cold lifecycle source");
    assert_ne!(append_source.content_digest(), cold_source.content_digest());
    assert!(
        append_source.counts().certified_bytes > cold_source.counts().certified_bytes,
        "{append_manifest:#?}"
    );
    assert_eq!(append_source.counts().indexed_documents, 2);

    let rewrite_query = "rewrite-source-lifecycle-oracle";
    let rewrite_padding = format!("rewrite-companion-{}", "x".repeat(2_048));
    write_codex_session(
        &session,
        native_session_id,
        &[
            ("2026-07-29T12:00:03.000Z", "user", rewrite_query),
            ("2026-07-29T12:00:04.000Z", "assistant", &rewrite_padding),
        ],
    );
    let rewrite_length = fs::metadata(&session).unwrap().len();
    let rewritten = json_output(ctx(&temp).args([
        "search",
        rewrite_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(
        &temp,
        &rewritten,
        "codex",
        rewrite_query,
        1,
        "message",
    );
    let rewrite_generation = assert_published_generation(&rewritten, "wait");
    assert_ne!(rewrite_generation, append_generation);
    assert_eq!(
        rewritten["retrieval"]["indexed_documents"], 3,
        "{rewritten:#}"
    );
    let rewrite_status =
        assert_daemon_publication(&temp, &rewrite_generation, 1, &["codex", "codex"]);
    assert_eq!(
        rewrite_status["history_epoch"]["lexical_generation_id"], cold_generation,
        "{rewrite_status:#}"
    );
    let (rewrite_manifest, _) = generation_manifest(&temp, &rewrite_generation);
    let rewrite_source = rewrite_manifest
        .sources
        .iter()
        .find(|source| source.observation().source() == append_source.observation().source())
        .expect("rewritten lifecycle source");
    assert_eq!(
        rewrite_source.observation().source(),
        append_source.observation().source()
    );
    assert_ne!(
        rewrite_source.content_digest(),
        append_source.content_digest()
    );
    let replaced = json_output(ctx(&temp).args([
        "search",
        append_query,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        replaced["results"].as_array().unwrap().is_empty(),
        "{replaced:#}"
    );
    assert_eq!(generation_id(&replaced), rewrite_generation);

    let truncate_query = "truncate-source-lifecycle-oracle";
    write_codex_session(
        &session,
        native_session_id,
        &[("2026-07-29T12:00:05.000Z", "user", truncate_query)],
    );
    assert!(
        fs::metadata(&session).unwrap().len() < rewrite_length,
        "truncate lifecycle mutation must reduce the certified source length"
    );
    let truncated = json_output(ctx(&temp).args([
        "search",
        truncate_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(
        &temp,
        &truncated,
        "codex",
        truncate_query,
        1,
        "message",
    );
    let truncate_generation = assert_published_generation(&truncated, "wait");
    assert_ne!(truncate_generation, rewrite_generation);
    assert_eq!(
        truncated["retrieval"]["indexed_documents"], 2,
        "{truncated:#}"
    );
    let truncate_status =
        assert_daemon_publication(&temp, &truncate_generation, 1, &["codex", "codex"]);
    assert_eq!(
        truncate_status["history_epoch"]["lexical_generation_id"], cold_generation,
        "{truncate_status:#}"
    );
    let (truncate_manifest, _) = generation_manifest(&temp, &truncate_generation);
    let truncate_source = truncate_manifest
        .sources
        .iter()
        .find(|source| source.observation().source() == rewrite_source.observation().source())
        .expect("truncated lifecycle source");
    assert_eq!(
        truncate_source.observation().source(),
        rewrite_source.observation().source()
    );
    assert_eq!(truncate_source.counts().indexed_documents, 1);
    let unavailable_event_id = truncated["results"][0]["ctx_event_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let unavailable_sessions = temp.path().join(".codex/sessions-unavailable");
    fs::rename(&sessions, &unavailable_sessions).unwrap();
    let unavailable = failure_stderr(ctx(&temp).args([
        "search",
        truncate_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert!(
        unavailable.contains("no executable source-backed routes were registered"),
        "{unavailable}"
    );
    let unavailable_status = assert_daemon_refresh_failure(&temp, 0, Some(&truncate_generation));
    assert_eq!(
        unavailable_status["lexical"]["generation_id"], truncate_generation,
        "{unavailable_status:#}"
    );
    assert_eq!(
        unavailable_status["lexical"]["status"], "unavailable",
        "{unavailable_status:#}"
    );
    assert_eq!(
        unavailable_status["lexical"]["reason"], "source_refresh_failed",
        "{unavailable_status:#}"
    );
    let (retained_manifest, _) = generation_manifest(&temp, &truncate_generation);
    assert_eq!(retained_manifest.sources, truncate_manifest.sources);
    assert!(retained_manifest.removals.is_empty());
    let unavailable_search = failure_stderr(ctx(&temp).args([
        "search",
        truncate_query,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        unavailable_search.contains("generation-bound source"),
        "{unavailable_search}"
    );
    ctx(&temp)
        .args(["show", "event", &unavailable_event_id, "--format", "json"])
        .assert()
        .failure();

    fs::rename(&unavailable_sessions, &sessions).unwrap();
    let restored = json_output(ctx(&temp).args([
        "search",
        truncate_query,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(
        &temp,
        &restored,
        "codex",
        truncate_query,
        1,
        "message",
    );
    assert_eq!(generation_id(&restored), truncate_generation);

    fs::remove_file(&session).unwrap();
    let deleted = json_output(ctx(&temp).args([
        "search",
        truncate_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    let deletion_generation = assert_published_generation(&deleted, "wait");
    assert_ne!(deletion_generation, truncate_generation);
    assert_eq!(deleted["retrieval"]["indexed_documents"], 1, "{deleted:#}");
    assert!(
        deleted["results"].as_array().unwrap().is_empty(),
        "{deleted:#}"
    );
    let deletion_status = assert_daemon_publication(&temp, &deletion_generation, 1, &["codex"]);
    assert_eq!(
        deletion_status["history_epoch"]["lexical_generation_id"], cold_generation,
        "{deletion_status:#}"
    );
    let (deletion_manifest, _) = generation_manifest(&temp, &deletion_generation);
    assert_eq!(deletion_manifest.sources.len(), 1);
    assert_eq!(deletion_manifest.removals.len(), 1);
    assert_eq!(
        deletion_manifest.removals[0].source(),
        truncate_source.observation().source()
    );
    let deleted_show = failure_stderr(ctx(&temp).args([
        "show",
        "event",
        &unavailable_event_id,
        "--format",
        "json",
    ]));
    assert!(
        deleted_show.contains("was not found in the source-backed Core generation"),
        "{deleted_show}"
    );
}

#[test]
fn search_refresh_publishes_discovered_top_provider_sources() {
    for (cli_provider, stored_provider, install_fixture) in [
        (
            "claude",
            "claude",
            install_default_claude_fixture as fn(&TempDir, &str),
        ),
        ("pi", "pi", install_default_pi_fixture),
        ("hermes", "hermes", install_default_hermes_fixture),
        ("kilo", "kilo", install_default_kilo_fixture),
        ("astrbot", "astrbot", install_default_astrbot_fixture),
        ("continue", "continue", install_default_continue_fixture),
        ("openhands", "openhands", install_default_openhands_fixture),
        ("rovodev", "rovodev", install_default_rovodev_fixture),
        ("lingma", "lingma", install_default_lingma_fixture),
        ("qoder", "qoder", install_default_qoder_fixture),
        ("junie", "junie", install_default_junie_fixture),
        ("cursor", "cursor", install_default_cursor_fixture),
    ] {
        let temp = tempdir();
        let query = format!("{stored_provider}-default-refresh-oracle");
        install_fixture(&temp, &query);
        let _daemon = start_source_refresh_daemon(&temp);

        let search = json_output(ctx(&temp).args([
            "search",
            &query,
            "--provider",
            cli_provider,
            "--refresh",
            "wait",
            "--format=json",
        ]));
        assert_source_backed_search_show_oracle(
            &temp,
            &search,
            stored_provider,
            &query,
            1,
            "message",
        );
        assert_eq!(search["freshness"]["source_count"], 1);
        let generation = assert_published_generation(&search, "wait");
        let status = assert_daemon_publication(&temp, &generation, 1, &[stored_provider]);
        assert_eq!(
            status["lexical"]["certified_sources"], 1,
            "{cli_provider} did not publish source inventory: {status:#}"
        );

        let unchanged = json_output(ctx(&temp).args([
            "search",
            &query,
            "--provider",
            cli_provider,
            "--refresh",
            "wait",
            "--format=json",
        ]));
        assert_source_backed_search_show_oracle(
            &temp,
            &unchanged,
            stored_provider,
            &query,
            1,
            "message",
        );
        assert_eq!(generation_id(&unchanged), generation, "{unchanged:#}");
        let unchanged_status = assert_daemon_publication(&temp, &generation, 1, &[stored_provider]);
        assert_eq!(
            unchanged_status["daemon"]["jobs"]["source_backed_refresh"]["generation_changed"],
            false,
            "{cli_provider} republished an unchanged source: {unchanged_status:#}"
        );
    }
}

#[test]
fn search_refresh_hermes_generation_detects_wal_only_append() {
    let temp = tempdir();
    let initial = "hermes-root-inventory-initial-oracle";
    let appended = "hermes-root-inventory-appended-oracle";
    install_default_hermes_fixture(&temp, initial);
    let source = temp.path().join(".hermes/state.db");
    let writer = Connection::open(&source).unwrap();
    let journal_mode: String = writer
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    writer
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .unwrap();
    let _daemon = start_source_refresh_daemon(&temp);

    let first = json_output(ctx(&temp).args([
        "search",
        initial,
        "--provider",
        "hermes",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &first, "hermes", initial, 1, "message");
    let first_generation = assert_published_generation(&first, "wait");
    let first_documents = first["retrieval"]["indexed_documents"].as_u64().unwrap();

    let unchanged = json_output(ctx(&temp).args([
        "search",
        initial,
        "--provider",
        "hermes",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(generation_id(&unchanged), first_generation, "{unchanged:#}");
    let unchanged_status = assert_daemon_publication(&temp, &first_generation, 1, &["hermes"]);
    assert_eq!(
        unchanged_status["daemon"]["jobs"]["source_backed_refresh"]["generation_changed"], false,
        "{unchanged_status:#}"
    );

    let main_before = fs::metadata(&source).unwrap();
    writer
        .execute(
            "INSERT INTO messages (session_id, role, content, timestamp)
             VALUES (?1, 'user', ?2, 1782259203.0)",
            ["hermes-cli-native", appended],
        )
        .unwrap();
    assert!(source.with_extension("db-wal").is_file());
    let main_after = fs::metadata(&source).unwrap();
    assert_eq!(main_after.len(), main_before.len());
    assert_eq!(
        main_after.modified().unwrap(),
        main_before.modified().unwrap()
    );

    let refreshed = json_output(ctx(&temp).args([
        "search",
        appended,
        "--provider",
        "hermes",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &refreshed, "hermes", appended, 1, "message");
    let refreshed_generation = assert_published_generation(&refreshed, "wait");
    assert_ne!(refreshed_generation, first_generation);
    assert!(
        refreshed["retrieval"]["indexed_documents"]
            .as_u64()
            .unwrap()
            > first_documents,
        "{refreshed:#}"
    );
    let refreshed_status = assert_daemon_publication(&temp, &refreshed_generation, 1, &["hermes"]);
    assert_eq!(
        refreshed_status["daemon"]["jobs"]["source_backed_refresh"]["generation_changed"], true,
        "{refreshed_status:#}"
    );
    drop(writer);
}

#[test]
fn search_refresh_wait_json_keeps_stderr_clean_and_reports_daemon_progress() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    copy_dir_all(&fixture, &temp.path().join(".codex").join("sessions"));
    let _daemon = start_source_refresh_daemon(&temp);

    let output = ctx(&temp)
        .args([
            "search",
            "onboarding",
            "--provider",
            "codex",
            "--refresh",
            "wait",
            "--format=json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_source_backed_search_show_oracle(&temp, &stdout, "codex", "onboarding", 1, "message");
    let generation = assert_published_generation(&stdout, "wait");
    let status = assert_daemon_publication(&temp, &generation, 1, &["codex", "codex"]);
    let job = &status["daemon"]["jobs"]["source_backed_refresh"];
    assert_eq!(job["progress"]["phase"], "published", "{status:#}");
    assert_eq!(job["progress"]["completed_sources"], 1, "{status:#}");
    assert_eq!(job["progress"]["total_sources"], 1, "{status:#}");
}

#[test]
fn search_refresh_wait_reports_typed_failure_for_empty_source_inventory() {
    let temp = tempdir();
    let _daemon = start_source_refresh_daemon(&temp);
    let output = ctx(&temp)
        .args(["search", "anything", "--refresh", "wait", "--format=json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("daemon-owned source-backed refresh failed"),
        "{stderr}"
    );
    assert!(
        stderr.contains("no executable source-backed routes were registered"),
        "{stderr}"
    );

    let status = assert_daemon_refresh_failure(&temp, 0, None);
    assert_eq!(
        status["history_epoch"]["status"], "unavailable",
        "{status:#}"
    );
    assert_eq!(
        status["history_epoch"]["reason"], "source_rebuild_failed",
        "{status:#}"
    );
    assert_eq!(status["lexical"]["status"], "unavailable", "{status:#}");
    assert_eq!(status["refresh"]["source_count"], 0, "{status:#}");
    assert_eq!(
        status["refresh"]["progress"]["phase"], "failed",
        "{status:#}"
    );
    assert_eq!(status["prior_epoch"]["present"], false, "{status:#}");
    assert_eq!(status["prior_epoch"]["opened"], false, "{status:#}");
    assert!(!temp.path().join("search/lexical/meta.json").exists());
    assert!(generation_manifest_paths(&temp).is_empty());
}
