mod support;

use ctx_history_index::{GenerationManifest, LEXICAL_SEGMENT_MERGE_FAN_IN};
use std::{
    io::{self, Read},
    process::{Child, Command as StdCommand, Stdio},
    time::SystemTime,
};
use support::*;

fn search_refresh_data_root(temp: &TempDir) -> PathBuf {
    temp.path().join("ctx-data")
}

fn ctx(temp: &TempDir) -> Command {
    let mut command = support::ctx(temp);
    command.env("CTX_DATA_ROOT", search_refresh_data_root(temp));
    command
}

fn ctx_from_binary(temp: &TempDir, binary: &Path) -> Command {
    let mut command = support::ctx_from_binary(temp, binary);
    command.env("CTX_DATA_ROOT", search_refresh_data_root(temp));
    command
}

struct SourceRefreshDaemon {
    child: Option<Child>,
}

impl SourceRefreshDaemon {
    fn pid(&self) -> u32 {
        self.child.as_ref().expect("running daemon child").id()
    }

    fn stop(&mut self) {
        terminate_and_reap_test_child(&mut self.child, "search-refresh daemon")
            .expect("terminate and reap search-refresh daemon");
    }
}

impl Drop for SourceRefreshDaemon {
    fn drop(&mut self) {
        if let Err(error) = terminate_and_reap_test_child(&mut self.child, "search-refresh daemon")
        {
            if std::thread::panicking() {
                eprintln!("search-refresh daemon teardown also failed: {error}");
            } else {
                panic!("search-refresh daemon teardown failed: {error}");
            }
        }
    }
}

fn start_source_refresh_daemon(temp: &TempDir) -> SourceRefreshDaemon {
    start_source_refresh_daemon_with_codex_home(temp, None)
}

fn start_source_refresh_daemon_with_codex_home(
    temp: &TempDir,
    codex_home: Option<&Path>,
) -> SourceRefreshDaemon {
    let data_root = search_refresh_data_root(temp);
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    fs::write(
        data_root.join("config.toml"),
        "[daemon]\nenabled = true\nmode = \"source-refresh-only\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    let binary = bind_test_ctx_binary(temp);
    launch_source_refresh_daemon(temp, &binary, codex_home)
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
    launch_source_refresh_daemon(temp, &binary, None)
}

fn launch_source_refresh_daemon(
    temp: &TempDir,
    binary: &Path,
    codex_home: Option<&Path>,
) -> SourceRefreshDaemon {
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
        .args(["daemon", "run", "--force"])
        .env("CTX_DAEMON_MODE", "source-refresh-only")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(codex_home) = codex_home {
        command.env("CODEX_HOME", codex_home);
    }
    let child = spawn_copied_test_binary(&mut command)
        .unwrap_or_else(|error| panic!("start isolated source-refresh daemon: {error}"));
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
                && status["daemon"]["core_refresh_endpoint"]["available"] == true
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

fn spawn_copied_test_binary(command: &mut StdCommand) -> io::Result<Child> {
    const MAX_TRANSIENT_ATTEMPTS: usize = 10;
    for attempt in 0..=MAX_TRANSIENT_ATTEMPTS {
        match command.spawn() {
            Ok(child) => return Ok(child),
            // Concurrent Bazel tests can briefly observe Linux ETXTBSY while
            // the freshly published task-owned executable becomes runnable.
            // This retry is deliberately test-only and tightly bounded.
            Err(error)
                if cfg!(unix)
                    && error.raw_os_error() == Some(26)
                    && attempt < MAX_TRANSIENT_ATTEMPTS =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded copied-binary launch loop must return")
}

fn assert_published_generation(search: &Value, expected_mode: &str) -> String {
    assert_eq!(search["freshness"]["mode"], expected_mode, "{search:#}");
    let expected_status = if expected_mode == "off" {
        "existing_generation"
    } else {
        "completed"
    };
    assert_eq!(search["freshness"]["status"], expected_status, "{search:#}");
    assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
    search["retrieval"]["generation_id"]
        .as_str()
        .expect("search response should identify its Core generation")
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
            && status["lexical"]["request_state"] == "published"
            && matches!(
                status["refresh"]["status"].as_str(),
                Some("ready" | "partial")
            )
            && status["refresh"]["published_generation"] == expected_generation
            && status["daemon"]["jobs"]["core_refresh"]["request_state"] == "published"
            && status["daemon"]["jobs"]["core_refresh"]["published_generation"]
                == expected_generation
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
    let expected_refresh_status = if status["refresh"]["source_failure_total"]
        .as_u64()
        .unwrap_or_default()
        != 0
        || status["refresh"]["rejected_record_total"]
            .as_u64()
            .unwrap_or_default()
            != 0
    {
        "partial"
    } else {
        "ready"
    };
    assert_eq!(
        status["refresh"]["status"], expected_refresh_status,
        "{status:#}"
    );
    assert_eq!(
        status["refresh"]["published_generation"], expected_generation,
        "{status:#}"
    );
    assert_eq!(status["refresh"]["generation_matches"], true, "{status:#}");
    assert_eq!(
        status["indexed_events"], status["lexical"]["indexed_documents"],
        "{status:#}"
    );
    assert_eq!(
        status["indexed_sources"], status["lexical"]["certified_sources"],
        "{status:#}"
    );
    assert!(status.get("prior_epoch").is_none(), "{status:#}");
    assert!(
        !search_refresh_data_root(temp).join("work.sqlite").exists(),
        "Core search/status must not create the previous-epoch Store"
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
    assert_eq!(packet["retrieval"]["index"], "core", "{packet:#}");
    let generation = packet["retrieval"]["generation_id"]
        .as_str()
        .expect("Core search generation");
    assert_eq!(generation.len(), 64, "{packet:#}");
    assert!(
        generation
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "generation ID must be lowercase hexadecimal: {packet:#}"
    );

    let results = packet["results"].as_array().expect("Core search results");
    for (offset, result) in results.iter().enumerate() {
        assert_eq!(result["rank"], offset + 1, "{result:#}");
        assert!(result["retrieval_score"].is_number(), "{result:#}");
    }
    let matching_results = results
        .iter()
        .filter(|result| {
            result["snippet"]
                .as_str()
                .is_some_and(|snippet| snippet.contains(query))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matching_results.len(),
        expected_results,
        "unexpected exact-oracle Core result count: {packet:#}"
    );
    for result in matching_results {
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
        assert!(result.get("source_path").is_none(), "{result:#}");
        assert!(result.get("source_exists").is_none(), "{result:#}");
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
            result.get("why_matched"),
            None,
            "Core results use the indexed event type and normalized snippet: {result:#}"
        );

        let commands = result["suggested_next_commands"]
            .as_array()
            .expect("Core search next commands");
        let event_id = result["ctx_event_id"].as_str().unwrap();
        let session_id = result["ctx_session_id"].as_str().unwrap();
        let command_prefix = format!(
            "ctx --data-root {}",
            search_refresh_data_root(temp).display()
        );
        assert!(
            commands
                .iter()
                .any(|command| command
                    == &format!("{command_prefix} show event {event_id} --window 10")),
            "{result:#}"
        );
        assert!(
            commands
                .iter()
                .any(|command| command == &format!("{command_prefix} show session {session_id}")),
            "{result:#}"
        );
        assert!(
            commands.iter().any(|command| {
                command.as_str().is_some_and(|command| {
                    command.starts_with(&format!("{command_prefix} search "))
                        && command.contains(&format!(" --session {session_id}"))
                })
            }),
            "{result:#}"
        );

        let citations = result["citations"]
            .as_array()
            .expect("Core search citations");
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
        assert!(citation.get("source_path").is_none(), "{result:#}");
        assert!(citation.get("source_exists").is_none(), "{result:#}");

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
        assert!(shown_event["event"].get("source_path").is_none());
        assert_eq!(
            shown_event["event"]["content"]["policy_status"], "selected",
            "{shown_event:#}"
        );
        assert!(
            shown_event["event"]["text"]
                .as_str()
                .is_some_and(|text| text.contains(query)),
            "{shown_event:#}"
        );

        let shown_session = json_output(ctx(temp).args([
            "show",
            "session",
            session_id,
            "--max-events",
            "4096",
            "--format",
            "json",
        ]));
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
                        && event["content"]["policy_status"] == "selected"
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
        .expect("search response should identify its Core generation")
}

fn generation_manifest(temp: &TempDir, generation: &str) -> (GenerationManifest, Value) {
    let path = search_refresh_data_root(temp)
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
    let directory = search_refresh_data_root(temp).join("search/lexical/ctx-generations");
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths = entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(std::ffi::OsStr::to_str)
                .and_then(|name| name.strip_suffix(".json"))
                .is_some_and(|stem| {
                    stem.len() == 64
                        && stem
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn directory_bytes(path: &Path) -> u64 {
    fs::read_dir(path)
        .unwrap_or_else(|error| panic!("read directory size {}: {error}", path.display()))
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                directory_bytes(&entry.path())
            } else {
                metadata.len()
            }
        })
        .sum()
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
    let job = &status["daemon"]["jobs"]["core_refresh"];
    assert_eq!(job["owner"], "daemon", "{status:#}");
    assert_eq!(
        status["daemon"]["mode"], "source-refresh-only",
        "{status:#}"
    );
    match job["trigger"].as_str() {
        Some("search") => assert_eq!(job["trigger_provenance"], "manual", "{status:#}"),
        Some("periodic") => {
            assert_eq!(job["trigger_provenance"], "daemon_scheduler", "{status:#}")
        }
        trigger => panic!("unexpected source refresh trigger {trigger:?}: {status:#}"),
    }
    assert_eq!(job["request_state"], "published", "{status:#}");
    assert_eq!(job["status"], "completed", "{status:#}");
    assert_eq!(
        job["published_generation"], expected_generation,
        "{status:#}"
    );
    let latest_route_count = job["source_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("latest daemon refresh omitted source_count: {status:#}"));
    assert_eq!(job["progress"]["phase"], "published", "{status:#}");
    assert_eq!(
        job["progress"]["completed_sources"], latest_route_count,
        "{status:#}"
    );
    assert_eq!(
        job["progress"]["total_sources"], latest_route_count,
        "{status:#}"
    );
    // `core-refresh.json` is the daemon's mutable latest-job status, not the
    // durable receipt for the search request that returned this generation.
    // A watcher-driven periodic no-op may therefore replace that status before
    // this read. Keep exact request-route assertions when the latest job is the
    // manual search, while periodic snapshots are checked for self-consistency;
    // callers retain the exact terminal request count from `freshness`.
    if job["trigger"] == "search" {
        assert_eq!(latest_route_count, expected_route_count, "{status:#}");
    }
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
    let status = wait_for_status(temp, "terminal Core refresh failure", |status| {
        status["refresh"]["status"] == "unavailable"
            && status["daemon"]["jobs"]["core_refresh"]["status"] == "failed"
            && status["daemon"]["jobs"]["core_refresh"]["request_state"] == "failed"
    });
    assert_eq!(status["refresh"]["status"], "unavailable", "{status:#}");
    assert_eq!(
        status["refresh"]["reason"], "core_refresh_failed",
        "{status:#}"
    );
    assert_eq!(status["refresh"]["request_state"], "failed", "{status:#}");
    let job = &status["daemon"]["jobs"]["core_refresh"];
    assert_eq!(job["owner"], "daemon", "{status:#}");
    assert!(
        matches!(
            (
                job["daemon_mode"].as_str(),
                job["trigger"].as_str(),
                job["trigger_provenance"].as_str(),
            ),
            (Some("source-refresh-only"), Some("search"), Some("manual"))
                | (Some("full"), Some("periodic"), Some("daemon_scheduler"))
        ),
        "the latest terminal failure must come from the requested source-only refresh or a newer persistent-daemon reconciliation tick: {status:#}"
    );
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
    assert!(
        job["generation_changed"].is_null(),
        "a failed refresh has no publication receipt or generation delta: {status:#}"
    );
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
    assert!(status.get("prior_epoch").is_none(), "{status:#}");
    assert!(
        !search_refresh_data_root(temp).join("work.sqlite").exists(),
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

include!("support/search_refresh/core_behaviors.rs");
include!("support/search_refresh/generation_lifecycle.rs");
