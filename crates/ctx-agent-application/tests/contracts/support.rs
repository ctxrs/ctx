#![allow(dead_code, unused_imports)]

pub(crate) use assert_cmd::Command;
pub(crate) use predicates::prelude::*;
#[cfg(ctx_agent_application_contract_fixtures)]
pub(crate) use rusqlite::{params, Connection};
pub(crate) use serde_json::{json, Value};
pub(crate) use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    ops::Deref,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub(crate) use tempfile::{Builder, TempDir};

use std::{io, process::Child, thread};
use tempfile::TempPath;

#[path = "support/mcp.rs"]
mod mcp;
pub(crate) use mcp::*;

const BOUND_CTX_BINARY_TEST_ROOT_MARKER: &str = ".ctx-test-bound-binary";
const READY_CTX_BINARY_TEST_ROOT_MARKER: &str = ".ctx-test-copy-ready";
const PERSISTENT_DAEMON_TEST_ROOT_MARKER: &str = ".ctx-test-owned-daemon";
const DAEMON_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_STOP_POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(crate) fn tempdir() -> TempDir {
    let temp_root = fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    let temp = Builder::new()
        .prefix("ctx-agent-application-contract-")
        .tempdir_in(temp_root)
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    temp
}

/// A bounded MCP test root whose copied binary may autostart one daemon.
pub(crate) struct McpDaemonTestRoot {
    temp: TempDir,
}

impl Deref for McpDaemonTestRoot {
    type Target = TempDir;

    fn deref(&self) -> &Self::Target {
        &self.temp
    }
}

impl Drop for McpDaemonTestRoot {
    fn drop(&mut self) {
        if let Err(error) = stop_mcp_test_daemon(&self.temp) {
            if thread::panicking() {
                eprintln!("MCP daemon teardown also failed: {error}");
            } else {
                panic!("MCP daemon teardown failed: {error}");
            }
        }
    }
}

pub(crate) fn daemon_test_root() -> McpDaemonTestRoot {
    let temp = tempdir();
    bind_test_ctx_binary(&temp);
    fs::write(
        temp.path().join(PERSISTENT_DAEMON_TEST_ROOT_MARKER),
        b"test-owned persistent MCP daemon root\n",
    )
    .unwrap();
    McpDaemonTestRoot { temp }
}

pub(crate) fn bind_test_ctx_binary(temp: &TempDir) -> PathBuf {
    let binary = copied_ctx_binary(temp);
    fs::write(
        temp.path().join(BOUND_CTX_BINARY_TEST_ROOT_MARKER),
        b"test-owned copied ctx binary\n",
    )
    .unwrap();
    binary
}

pub(crate) fn ctx(temp: &TempDir) -> Command {
    let binary = if temp
        .path()
        .join(BOUND_CTX_BINARY_TEST_ROOT_MARKER)
        .is_file()
    {
        copied_ctx_binary(temp)
    } else {
        ctx_binary()
    };
    let mut command = Command::new(binary);
    apply_hermetic_env(&mut command, temp);
    command
}

pub(crate) fn data_root(temp: &TempDir) -> PathBuf {
    temp.path().join("ctx-data")
}

pub(crate) fn expected_device_path(_home: &Path, _state: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        _state.join("ctx").join("device.json")
    }
    #[cfg(target_os = "macos")]
    {
        _home
            .join("Library")
            .join("Application Support")
            .join("ctx")
            .join("device.json")
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        _state.join("ctx").join("device.json")
    }
}

fn ctx_binary() -> PathBuf {
    let program = PathBuf::from(Command::cargo_bin("ctx").unwrap().get_program());
    if program.is_absolute() {
        program
    } else {
        std::env::current_dir().unwrap().join(program)
    }
}

fn apply_hermetic_env(command: &mut Command, temp: &TempDir) {
    let persistent_daemon_test = temp
        .path()
        .join(PERSISTENT_DAEMON_TEST_ROOT_MARKER)
        .is_file();
    command.env("CTX_DATA_ROOT", data_root(temp));
    command.env("HOME", temp.path());
    command.env("CTX_ANALYTICS_ENABLED", "false");
    command.env("CTX_LOCAL_USAGE_ENABLED", "false");
    for name in [
        "CTX_ANALYTICS_OFF",
        "CTX_DISABLE_ANALYTICS",
        "CTX_INSTALL_DIAGNOSTICS_OFF",
        "CTX_DAEMON_OFF",
        "CTX_DISABLE_DAEMON",
        "CTX_UPGRADE_OFF",
        "CTX_DISABLE_AUTO_UPGRADE",
        "CTX_DAEMON_ENABLED",
        "CTX_QUIET",
        "CI",
        "GITHUB_ACTIONS",
        "BUILDKITE",
        "BUILDKITE_BUILD_ID",
        "OPENCLAW_STATE_DIR",
        "HERMES_HOME",
        "ASTRBOT_ROOT",
        "SHELLEY_DB",
        "KILO_DB",
        "MIMOCODE_HOME",
        "MIMOCODE_CONFIG_DIR",
        "MIMOCODE_DB",
        "MIMOCODE_DISABLE_CHANNEL_DB",
        "FORGE_CONFIG",
        "VIBE_HOME",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "COPILOT_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "LOCALAPPDATA",
        "APPDATA",
    ] {
        command.env_remove(name);
    }
    if persistent_daemon_test {
        command.env_remove("CTX_DAEMON_AUTOSTART_OFF");
    } else {
        command.env("CTX_DAEMON_AUTOSTART_OFF", "1");
    }
}

fn stop_mcp_test_daemon(temp: &TempDir) -> Result<(), String> {
    let binary = test_binary_copy_path(temp);
    if !binary.is_file() {
        return Ok(());
    }
    let output = ctx(temp)
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .args(["daemon", "disable", "--format=json"])
        .output()
        .map_err(|error| format!("disable MCP test daemon: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "disable MCP test daemon failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let deadline = Instant::now() + DAEMON_STOP_TIMEOUT;
    loop {
        let status = ctx(temp)
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .args(["daemon", "status", "--format=json"])
            .output()
            .map_err(|error| format!("inspect disabled MCP test daemon: {error}"))?;
        if status.status.success()
            && serde_json::from_slice::<Value>(&status.stdout)
                .is_ok_and(|packet| packet["daemon"]["running"] != true)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("MCP test daemon remained active after disable".to_owned());
        }
        thread::sleep(DAEMON_STOP_POLL_INTERVAL);
    }
}

fn copied_ctx_binary(temp: &TempDir) -> PathBuf {
    let target = test_binary_copy_path(temp);
    if !target.exists() {
        copied_binary(temp, &ctx_binary());
    }
    ensure_copied_ctx_binary_is_executable(temp, &target);
    target
}

fn ensure_copied_ctx_binary_is_executable(temp: &TempDir, target: &Path) {
    let ready = temp.path().join(READY_CTX_BINARY_TEST_ROOT_MARKER);
    if ready.is_file() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match std::process::Command::new(target).arg("--version").output() {
            Ok(output) if output.status.success() => {
                fs::write(&ready, b"test-owned copied ctx binary is executable\n").unwrap();
                return;
            }
            Ok(output) => panic!(
                "copied ctx binary {} failed its readiness probe: {}",
                target.display(),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error)
                if cfg!(unix) && error.raw_os_error() == Some(26) && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!(
                "copied ctx binary {} failed its readiness probe: {error}",
                target.display()
            ),
        }
    }
}

fn test_binary_copy_path(temp: &TempDir) -> PathBuf {
    temp.path().join(if cfg!(windows) {
        "ctx-test-copy.exe"
    } else {
        "ctx-test-copy"
    })
}

fn copied_binary(temp: &TempDir, source: &Path) -> PathBuf {
    let target = test_binary_copy_path(temp);
    if target.exists() {
        return target;
    }
    let stage_dir = Builder::new()
        .prefix(".ctx-test-copy-stage-")
        .tempdir_in(temp.path())
        .unwrap();
    let staged_path = stage_dir.path().join(if cfg!(windows) {
        "ctx-test-copy.exe"
    } else {
        "ctx-test-copy"
    });
    {
        let mut source_file = fs::File::open(source).unwrap();
        let mut staged_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_path)
            .unwrap();
        io::copy(&mut source_file, &mut staged_file).unwrap();
        staged_file.sync_all().unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&staged_path).unwrap().permissions();
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(&staged_path, permissions).unwrap();
    }
    let staged = TempPath::try_from_path(staged_path).unwrap();
    match staged.persist_noclobber(&target) {
        Ok(()) => {}
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => drop(error.path),
        Err(error) => panic!(
            "publish copied test binary {}: {}",
            target.display(),
            error.error
        ),
    }
    target
}

pub(crate) fn terminate_and_reap_test_child(
    child: &mut Option<Child>,
    description: &str,
) -> Result<Option<u32>, String> {
    let Some(mut child) = child.take() else {
        return Ok(None);
    };
    let pid = child.id();
    if child
        .try_wait()
        .map_err(|error| format!("inspect {description} {pid}: {error}"))?
        .is_none()
    {
        if let Err(kill_error) = child.kill() {
            if child
                .try_wait()
                .map_err(|error| {
                    format!(
                        "inspect {description} {pid} after termination failed ({kill_error}): {error}"
                    )
                })?
                .is_none()
            {
                return Err(format!("terminate {description} {pid}: {kill_error}"));
            }
            return Ok(Some(pid));
        }
        child
            .wait()
            .map_err(|error| format!("reap {description} {pid}: {error}"))?;
    }
    Ok(Some(pid))
}

pub(crate) fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

pub(crate) fn json_output(command: &mut Command) -> Value {
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).unwrap()
}

pub(crate) fn failure_stderr(command: &mut Command) -> String {
    let stderr = command.assert().failure().get_output().stderr.clone();
    String::from_utf8(stderr).unwrap()
}

pub(crate) fn ctx_product_version(temp: &TempDir) -> String {
    let output = ctx(temp)
        .arg("--version")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .expect("ctx --version includes a product version")
        .to_owned()
}

pub(crate) fn read_analytics_events(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(crate) fn analytics_cli_event(event: &Value) -> &Value {
    event["events"]
        .as_array()
        .and_then(|events| {
            events.iter().find(|event| {
                event["event_name"] == "operation_completed" && event["surface"] == "cli"
            })
        })
        .unwrap_or_else(|| panic!("analytics batch has no CLI operation event: {event:#}"))
}

pub(crate) fn assert_no_json_string_contains(value: &Value, forbidden: &[&str]) {
    match value {
        Value::String(text) => {
            for needle in forbidden {
                assert!(
                    !text.contains(needle),
                    "analytics leaked forbidden string {needle:?} in {text:?}"
                );
            }
        }
        Value::Array(values) => {
            for value in values {
                assert_no_json_string_contains(value, forbidden);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                assert_no_json_string_contains(value, forbidden);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

pub(crate) fn assert_explicit_source_publication<'a>(
    packet: &'a Value,
    provider: &str,
    source_format: &str,
) -> &'a Value {
    assert_eq!(packet["schema_version"], 2, "{packet:#}");
    assert_eq!(packet["outcome"], "success", "{packet:#}");
    assert_eq!(packet["failure_scope"], "none", "{packet:#}");
    assert_eq!(packet["failure_type"], "none", "{packet:#}");
    let sources = packet["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("missing explicit source receipts in {packet:#}"));
    assert_eq!(sources.len(), 1, "{packet:#}");
    let source = &sources[0];
    assert_eq!(source["provider"], provider, "{packet:#}");
    assert_eq!(source["source_format"], source_format, "{packet:#}");
    assert_eq!(source["status"], "published", "{packet:#}");
    assert_eq!(
        source["failure_scope"], packet["failure_scope"],
        "{packet:#}"
    );
    assert_eq!(source["failure_type"], packet["failure_type"], "{packet:#}");
    assert!(source["published_generation"].is_string(), "{packet:#}");
    for key in [
        "current_source_count",
        "current_indexed_documents",
        "current_complete_records",
        "current_retained_records",
        "current_rejected_records",
        "current_ignored_records",
        "current_certified_source_bytes",
        "current_sources_with_rejections",
        "removed_source_count",
    ] {
        assert!(
            packet["totals"][key].is_number(),
            "missing {key} in {packet:#}"
        );
        assert_eq!(packet["totals"][key], source[key], "{packet:#}");
    }
    assert_eq!(packet["totals"]["failed_sources"], 0, "{packet:#}");
    assert_eq!(packet["totals"]["rejected_records"], 0, "{packet:#}");
    assert_eq!(
        packet["totals"]["sources_completed_with_rejections"], 0,
        "{packet:#}"
    );
    assert_eq!(
        packet["totals"]["rejections"],
        json!({
            "rejected_records": 0,
            "sources_completed_with_rejections": 0,
        }),
        "{packet:#}"
    );
    assert_eq!(source["rejected_record_total"], 0, "{packet:#}");
    assert_omits_keys(
        packet,
        &["imported_sessions", "imported_events", "skipped_events"],
    );
    source
}

fn assert_omits_keys(value: &Value, forbidden_keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for key in forbidden_keys {
                assert!(
                    !map.contains_key(*key),
                    "forbidden JSON key {key} appeared in {value:#}"
                );
            }
            for nested in map.values() {
                assert_omits_keys(nested, forbidden_keys);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_omits_keys(item, forbidden_keys);
            }
        }
        _ => {}
    }
}

#[cfg(ctx_agent_application_contract_fixtures)]
pub(crate) fn provider_history_fixture(name: &str) -> String {
    materialized_fixture("provider-history", name)
}

#[cfg(ctx_agent_application_contract_fixtures)]
fn materialized_fixture(category: &str, name: &str) -> String {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(category)
        .join(name);
    let materialized_root = std::env::var_os("TEST_TMPDIR")
        .map(|path| PathBuf::from(path).join("test-data/materialized-fixtures"))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap()
                .join("target/test-data/materialized-fixtures")
        });
    fs::create_dir_all(&materialized_root).unwrap();
    let unique = format!(
        "{}-{}-{}-{}",
        category,
        name.replace(['/', '\\', '.'], "_"),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let private_root = materialized_root.join(unique);
    fs::create_dir_all(&private_root).unwrap();
    let mut target = private_root.join("fixture");
    if source.is_file() {
        if let Some(extension) = source.extension() {
            target.set_extension(extension);
        }
    }
    if source.is_dir() {
        copy_dir_all(&source, &target);
    } else {
        fs::copy(&source, &target).unwrap();
    }
    target.to_str().unwrap().to_owned()
}

#[cfg(ctx_agent_application_contract_fixtures)]
pub(crate) fn initialize_authoritative_empty_core(data_root: &Path) -> String {
    use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};

    let index_root = data_root.join("search").join("lexical");
    let writer = GenerationWriter::open(
        &index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let core_receipt = writer.commit(|_| true).unwrap();
    let verified = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(verified.generation_id(), core_receipt.generation_id);
    let generation_id = core_receipt.generation_id;
    let route_identity = "ab".repeat(32);
    let receipt = json!({
        "published_generation": generation_id,
        "generation_changed": true,
        "current": {
            "current_source_count": 0,
            "current_indexed_documents": 0,
            "current_complete_records": 0,
            "current_retained_records": 0,
            "current_rejected_records": 0,
            "current_ignored_records": 0,
            "current_certified_source_bytes": 0,
            "current_sources_with_rejections": 0,
            "removed_source_count": 0,
        },
        "outcome": "completed",
        "selected_route_total": 1,
        "successful_route_total": 1,
        "source_failure_total": 0,
        "source_failures_omitted": 0,
        "rejected_record_total": 0,
        "rejection_diagnostics_omitted": 0,
        "route_results": {(route_identity): ["s", true]},
        "zero_source_authority": {
            "generation_id": generation_id,
            "route_kinds": "e",
        },
        "catalog_route_bindings": {},
    });
    GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap()
        .republish_current_publication_metadata(
            &generation_id,
            serde_json::to_vec(&json!({
                "version": 3,
                "request_id": "mcp-authoritative-empty-fixture",
                "operation": "refresh",
                "refresh_scope": {"kind": "all"},
                "receipt": receipt,
                "route_observations": [null],
                "route_controls": {},
            }))
            .unwrap(),
        )
        .unwrap();
    generation_id
}

#[cfg(ctx_agent_application_contract_fixtures)]
pub(crate) fn copy_dir_all(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let entry_path = entry.path();
        let target = to.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_all(&entry_path, &target);
        } else {
            fs::copy(entry_path, target).unwrap();
        }
    }
}
