use serde_json::Value;
use std::{
    fs,
    ops::Deref,
    path::{Path, PathBuf},
    process::{Child, Command as StdCommand, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
use super::analytics_outbox_paths;
use super::runner::{
    ctx, ctx_from_binary, data_root, json_output, tempdir, test_binary_copy_path,
    PERSISTENT_DAEMON_TEST_ROOT_MARKER,
};
const DAEMON_DISABLE_COMMAND_TIMEOUT: Duration = Duration::from_secs(12);
const DAEMON_DISABLE_REAP_TIMEOUT: Duration = Duration::from_secs(1);
const DAEMON_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_STOP_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
pub(crate) struct TerminalUpgradeObservation {
    pub(crate) state: Value,
    pub(crate) events: Vec<Value>,
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
impl TerminalUpgradeObservation {
    pub(crate) fn assert_applied_auto_upgrade(self) {
        assert_eq!(self.events.len(), 1);
        let event = &self.events[0];
        assert_eq!(event["event_name"], "operation_completed");
        assert_eq!(event["event_version"], 1);
        assert_eq!(event["surface"], "cli");
        assert_eq!(event["operation"], "upgrade");
        assert_eq!(event["outcome"], "success");
        assert!(event["event_id"].as_str().is_some_and(|value| {
            uuid::Uuid::parse_str(value).is_ok_and(|id| id.get_version_num() == 4)
        }));
        let properties = event["properties"].as_object().unwrap();
        assert_eq!(properties["upgrade_mode"], "auto");
        assert_eq!(properties["upgrade_status"], "applied");
        assert_eq!(properties["upgrade_applied"], true);
        assert_eq!(
            properties["upgrade_attempt_id"],
            self.state["attempt_id"].as_str().unwrap()
        );
    }
}

/// A temporary CLI root that owns every daemon started from its copied binary.
///
/// The marker makes commands use the production-persistent daemon contract.
/// Teardown first asks that exact binary to disable its daemon, then uses the
/// lock's root, owner, binary, and live-process identities before any fallback
/// signal. A PID alone is never sufficient authority.
pub(crate) struct DaemonTestRoot {
    temp: TempDir,
    daemon_data_root: PathBuf,
}

impl DaemonTestRoot {
    fn new() -> Self {
        let temp = tempdir();
        let daemon_data_root = data_root(&temp);
        Self::with_temp_and_data_root(temp, daemon_data_root)
    }

    fn with_data_root_name(name: &str) -> Self {
        let temp = tempdir();
        let relative = Path::new(name);
        let mut components = relative.components();
        assert!(
            matches!(components.next(), Some(std::path::Component::Normal(_)))
                && components.next().is_none(),
            "daemon test data root must be one relative path component"
        );
        let daemon_data_root = temp.path().join(relative);
        Self::with_temp_and_data_root(temp, daemon_data_root)
    }

    fn with_temp_and_data_root(temp: TempDir, daemon_data_root: PathBuf) -> Self {
        fs::write(
            temp.path().join(PERSISTENT_DAEMON_TEST_ROOT_MARKER),
            b"test-owned persistent daemon root\n",
        )
        .unwrap();
        Self {
            temp,
            daemon_data_root,
        }
    }

    pub(crate) fn daemon_data_root(&self) -> &Path {
        &self.daemon_data_root
    }

    #[cfg(all(
        unix,
        any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
    ))]
    pub(crate) fn observe_terminal_upgrade_handoff(
        &self,
        child: Child,
        state_path: &Path,
        events_path: &Path,
        timeout: Duration,
    ) -> TerminalUpgradeObservation {
        let mut child = TestOwnedDaemonChild::new(self, child);
        let original_pid = child.id();
        let deadline = Instant::now() + timeout;
        let (mut state, mut replacement_pid, mut event, mut status) = (None, None, None, None);
        loop {
            state = state.or_else(|| read_applied_upgrade_state(state_path));
            replacement_pid = replacement_pid
                .or_else(|| running_replacement_daemon_pid(&self.daemon_data_root, original_pid));
            if event.is_none() {
                event = state.as_ref().and_then(|state| {
                    terminal_upgrade_event(events_path, self.path(), state["attempt_id"].as_str()?)
                        .unwrap_or_else(|error| panic!("observe terminal upgrade event: {error}"))
                });
            }
            if status.is_none() {
                status = child.try_wait().unwrap();
            }
            if state.is_some() && replacement_pid.is_some() && event.is_some() && status.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for durable state, replacement daemon, terminal event, and original daemon exit"
            );
            thread::sleep(Duration::from_millis(25));
        }
        assert!(status.unwrap().success(), "original upgrade daemon failed");

        let replacement_pid = replacement_pid.unwrap();
        stop_test_owned_daemon(&self.temp, &self.daemon_data_root, Some(replacement_pid))
            .unwrap_or_else(|error| panic!("stop test-owned replacement daemon: {error}"));
        reap_exited_process_if_child(replacement_pid)
            .unwrap_or_else(|error| panic!("reap replacement daemon: {error}"));
        let state = state.unwrap();
        let events = terminal_upgrade_event(
            events_path,
            self.path(),
            state["attempt_id"].as_str().unwrap(),
        )
        .unwrap_or_else(|error| panic!("observe stopped terminal upgrade event: {error}"))
        .into_iter()
        .collect();
        TerminalUpgradeObservation { state, events }
    }
}

impl Deref for DaemonTestRoot {
    type Target = TempDir;

    fn deref(&self) -> &Self::Target {
        &self.temp
    }
}

impl AsRef<Path> for DaemonTestRoot {
    fn as_ref(&self) -> &Path {
        self.temp.path()
    }
}

impl Drop for DaemonTestRoot {
    fn drop(&mut self) {
        if let Err(error) = stop_test_owned_daemon(&self.temp, &self.daemon_data_root, None) {
            if thread::panicking() {
                eprintln!("test-owned daemon teardown also failed: {error}");
            } else {
                panic!("test-owned daemon teardown failed: {error}");
            }
        }
    }
}

pub(crate) fn daemon_test_root() -> DaemonTestRoot {
    DaemonTestRoot::new()
}

pub(crate) fn daemon_test_root_with_data_root(name: &str) -> DaemonTestRoot {
    DaemonTestRoot::with_data_root_name(name)
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
struct TestOwnedDaemonChild<'a> {
    root: &'a DaemonTestRoot,
    child: Option<Child>,
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
impl<'a> TestOwnedDaemonChild<'a> {
    fn new(root: &'a DaemonTestRoot, child: Child) -> Self {
        Self {
            root,
            child: Some(child),
        }
    }

    fn id(&self) -> u32 {
        self.child.as_ref().expect("daemon child was reaped").id()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let status = self
            .child
            .as_mut()
            .expect("daemon child was reaped")
            .try_wait()?;
        if status.is_some() {
            self.child.take();
        }
        Ok(status)
    }
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
impl Drop for TestOwnedDaemonChild<'_> {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let stop = stop_test_owned_daemon(&self.root.temp, &self.root.daemon_data_root, None);
        let deadline = Instant::now() + DAEMON_STOP_TIMEOUT;
        let reap = loop {
            match child.try_wait() {
                Ok(Some(_)) => break Ok(()),
                Ok(None) if Instant::now() < deadline => thread::sleep(DAEMON_STOP_POLL_INTERVAL),
                Ok(None) => break Err("timed out reaping daemon child".to_owned()),
                Err(error) => break Err(format!("reap daemon child: {error}")),
            }
        };
        let result = match (stop, reap) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(stop), Err(reap)) => Err(format!("{stop}; {reap}")),
        };
        if let Err(error) = result {
            if thread::panicking() {
                eprintln!("test-owned daemon child teardown also failed: {error}");
            } else {
                panic!("test-owned daemon child teardown failed: {error}");
            }
        }
    }
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
fn read_applied_upgrade_state(path: &Path) -> Option<Value> {
    let state: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (state["status"] == "applied" && state["attempt_id"].as_str().is_some()).then_some(state)
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
fn running_replacement_daemon_pid(root: &Path, original_pid: u32) -> Option<u32> {
    let status: Value =
        serde_json::from_slice(&fs::read(root.join("daemon/status.json")).ok()?).ok()?;
    let pid = status["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())?;
    (status["status"] == "running" && pid != original_pid).then_some(pid)
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
#[derive(Clone)]
struct ObservedAnalyticsPayload {
    raw: Option<Vec<u8>>,
    value: Value,
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
fn delivered_payloads(events_path: &Path) -> Result<Option<Vec<ObservedAnalyticsPayload>>, String> {
    let bytes = match fs::read(events_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Some(Vec::new())),
        Err(error) => return Err(format!("read {}: {error}", events_path.display())),
    };
    // The file transport writes each body and its newline separately. Retry a
    // snapshot ending in an in-flight body instead of parsing its partial JSON.
    if bytes.last().is_some_and(|byte| *byte != b'\n') {
        return Ok(None);
    }
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .map(|line| {
            let value = serde_json::from_slice(line).map_err(|error| {
                format!(
                    "parse complete record in {}: {error}",
                    events_path.display()
                )
            })?;
            Ok(ObservedAnalyticsPayload {
                raw: Some(line.to_vec()),
                value,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
fn queued_payloads(analytics_root: &Path) -> Result<Vec<ObservedAnalyticsPayload>, String> {
    let mut payloads = Vec::new();
    for outbox in analytics_outbox_paths(analytics_root) {
        let bytes = match fs::read(&outbox) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "read analytics outbox {}: {error}",
                    outbox.display()
                ))
            }
        };
        let state: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse analytics outbox {}: {error}", outbox.display()))?;
        let entries = state["entries"]
            .as_array()
            .ok_or_else(|| format!("analytics outbox has no entries array: {state:#}"))?;
        for entry in entries {
            let body = entry
                .get("payload")
                .or_else(|| entry.get("body"))
                .ok_or_else(|| format!("analytics outbox entry has no payload body: {entry:#}"))?;
            let payload = match body {
                Value::String(body) => ObservedAnalyticsPayload {
                    raw: Some(body.as_bytes().to_vec()),
                    value: serde_json::from_str(body).map_err(|error| {
                        format!(
                            "parse queued analytics payload in {}: {error}",
                            outbox.display()
                        )
                    })?,
                },
                Value::Object(_) => ObservedAnalyticsPayload {
                    raw: None,
                    value: body.clone(),
                },
                _ => {
                    return Err(format!(
                        "analytics outbox payload is not JSON text: {entry:#}"
                    ))
                }
            };
            payloads.push(payload);
        }
    }
    Ok(payloads)
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
type TerminalEventRecord = (String, Option<Vec<u8>>, Value);

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
fn matching_terminal_events(
    payloads: Vec<ObservedAnalyticsPayload>,
    attempt_id: &str,
) -> Result<Vec<TerminalEventRecord>, String> {
    let mut matches = Vec::new();
    for payload in payloads {
        let Some(events) = payload.value["events"].as_array() else {
            continue;
        };
        for event in events {
            if event["event_name"] == "operation_completed"
                && event["surface"] == "cli"
                && event["operation"] == "upgrade"
                && event["properties"]["upgrade_attempt_id"] == attempt_id
            {
                let id = event["event_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| "matching terminal upgrade event has no event_id".to_owned())?;
                matches.push((id.to_owned(), payload.raw.clone(), event.clone()));
            }
        }
    }
    Ok(matches)
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
fn terminal_upgrade_event(
    events_path: &Path,
    analytics_root: &Path,
    attempt_id: &str,
) -> Result<Option<Value>, String> {
    // A successful delivery is flushed before its durable entry is removed.
    // Queue-first observation therefore cannot see a false zero during drain.
    let queued = matching_terminal_events(queued_payloads(analytics_root)?, attempt_id)?;
    let Some(delivered) = delivered_payloads(events_path)? else {
        return Ok(None);
    };
    let delivered = matching_terminal_events(delivered, attempt_id)?;
    if delivered.len() > 1 || queued.len() > 1 {
        return Err(format!(
            "terminal upgrade event count by source was delivered={} queued={}",
            delivered.len(),
            queued.len()
        ));
    }
    match (delivered.first(), queued.first()) {
        (None, None) => Ok(None),
        (Some(event), None) | (None, Some(event)) => Ok(Some(event.2.clone())),
        (Some(delivered), Some(queued))
            if delivered.0 == queued.0
                && delivered
                    .1
                    .as_deref()
                    .zip(queued.1.as_deref())
                    .is_some_and(|(delivered, queued)| delivered == queued) =>
        {
            Ok(Some(delivered.2.clone()))
        }
        (Some(delivered), Some(queued)) => Err(format!(
            "conflicting delivered/queued terminal events {} and {}",
            delivered.0, queued.0
        )),
    }
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_upgrade)
))]
pub(crate) fn assert_terminal_upgrade_raw_overlap_contract() {
    const ATTEMPT_ID: &str = "attempt-1";
    const DELIVERED: &[u8] = br#"{"events":[{"event_id":"event-1","event_name":"operation_completed","surface":"cli","operation":"upgrade","properties":{"upgrade_attempt_id":"attempt-1"}}]}"#;
    const REENCODED: &[u8] = br#"{ "events": [ { "operation": "upgrade", "surface": "cli", "event_name": "operation_completed", "event_id": "event-1", "properties": { "upgrade_attempt_id": "attempt-1" } } ] }"#;

    assert_eq!(
        serde_json::from_slice::<Value>(DELIVERED).unwrap(),
        serde_json::from_slice::<Value>(REENCODED).unwrap()
    );
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let outbox_path = temp.path().join("analytics-outbox-v1.json");
    let mut delivered_record = DELIVERED.to_vec();
    delivered_record.push(b'\n');
    fs::write(&events_path, delivered_record).unwrap();
    let queue = |raw: &[u8]| {
        let raw = std::str::from_utf8(raw).unwrap();
        fs::write(
            &outbox_path,
            serde_json::to_vec(&serde_json::json!({"entries": [{"payload": raw}]})).unwrap(),
        )
        .unwrap();
    };

    queue(DELIVERED);
    assert!(
        terminal_upgrade_event(&events_path, temp.path(), ATTEMPT_ID)
            .unwrap()
            .is_some()
    );
    queue(REENCODED);
    let error = terminal_upgrade_event(&events_path, temp.path(), ATTEMPT_ID).unwrap_err();
    assert!(error.contains("conflicting delivered/queued terminal events"));
}

#[cfg(unix)]
fn reap_exited_process_if_child(pid: u32) -> Result<(), String> {
    let raw_pid = libc::pid_t::try_from(pid).map_err(|error| format!("invalid pid: {error}"))?;
    let mut status = 0;
    let result = unsafe { libc::waitpid(raw_pid, &mut status, libc::WNOHANG) };
    if result == raw_pid {
        (!process_is_running(pid))
            .then_some(())
            .ok_or_else(|| format!("reaped replacement daemon pid {pid} was reused"))
    } else if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
        (!process_is_running(pid))
            .then_some(())
            .ok_or_else(|| format!("replacement daemon {pid} was not a child and remained live"))
    } else if result == 0 {
        Err(format!("replacement daemon {pid} remained live"))
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

pub(crate) fn wait_for_test_lexical_projection(temp: &TempDir, generation: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = json_output(ctx(temp).args(["status", "--format=json"]));
        if status["lexical"]["status"] == "ready"
            && status["lexical"]["generation_id"] == generation
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for lexical projection at generation {generation}: {status:#}"
        );
        thread::sleep(DAEMON_STOP_POLL_INTERVAL);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DaemonIdentity {
    pid: u32,
    owner_id: String,
    binary: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundedCommandExit {
    Exited,
    KilledAndReaped,
}

fn std_command_from_assert(prepared: &assert_cmd::Command) -> StdCommand {
    let mut command = StdCommand::new(prepared.get_program());
    command.args(prepared.get_args());
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
    if let Some(directory) = prepared.get_current_dir() {
        command.current_dir(directory);
    }
    command
}

fn poll_child_until(
    child: &mut Child,
    deadline: Instant,
    label: &str,
) -> Result<Option<ExitStatus>, String> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {}
            Err(error) => return Err(format!("poll {label}: {error}")),
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        thread::sleep(DAEMON_STOP_POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
    }
}

fn kill_and_reap_child_bounded(
    child: &mut Child,
    timeout: Duration,
    label: &str,
) -> Result<(), String> {
    let kill_error = child.kill().err();
    match poll_child_until(child, Instant::now() + timeout, label) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(format!(
            "{label} did not exit within {timeout:?} after Child::kill{}",
            kill_error
                .map(|error| format!(" failed: {error}"))
                .unwrap_or_default()
        )),
        Err(reap_error) => Err(match kill_error {
            Some(kill_error) => format!("kill {label}: {kill_error}; {reap_error}"),
            None => reap_error,
        }),
    }
}

fn run_child_bounded(
    command: &mut StdCommand,
    timeout: Duration,
    reap_timeout: Duration,
    label: &str,
) -> Result<BoundedCommandExit, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| format!("start {label}: {error}"))?;
    match poll_child_until(&mut child, Instant::now() + timeout, label) {
        Ok(Some(_)) => Ok(BoundedCommandExit::Exited),
        Ok(None) => {
            kill_and_reap_child_bounded(&mut child, reap_timeout, label)?;
            Ok(BoundedCommandExit::KilledAndReaped)
        }
        Err(poll_error) => match kill_and_reap_child_bounded(&mut child, reap_timeout, label) {
            Ok(()) => Err(poll_error),
            Err(cleanup_error) => Err(format!("{poll_error}; {cleanup_error}")),
        },
    }
}

#[cfg(unix)]
pub(crate) fn assert_bounded_disable_command_timeout(fixture: &str) {
    let mut command = StdCommand::new(std::env::current_exe().unwrap());
    command.args(["--exact", fixture, "--ignored"]);
    let started = Instant::now();
    let exit = run_child_bounded(
        &mut command,
        Duration::from_millis(100),
        DAEMON_DISABLE_REAP_TIMEOUT,
        "stalled disable fixture",
    )
    .unwrap();
    assert_eq!(exit, BoundedCommandExit::KilledAndReaped);
    assert!(started.elapsed() < Duration::from_secs(2));
}

fn stop_test_owned_daemon(
    temp: &TempDir,
    daemon_data_root: &Path,
    expected_pid: Option<u32>,
) -> Result<(), String> {
    let binary = test_binary_copy_path(temp);
    stop_test_owned_daemon_for_binary(temp, daemon_data_root, &binary, expected_pid)
}

pub(crate) fn stop_test_owned_daemon_for_binary(
    temp: &TempDir,
    daemon_data_root: &Path,
    binary: &Path,
    expected_pid: Option<u32>,
) -> Result<(), String> {
    if !binary.is_file() {
        if expected_pid.is_some() {
            return Err("cannot attribute replacement daemon without its test binary".into());
        }
        return Ok(());
    }

    let Some(initial) = read_daemon_identity(daemon_data_root)? else {
        if expected_pid.is_some() {
            return Err("cannot attribute replacement daemon without an active lock".into());
        }
        return remove_released_test_daemon_artifacts(daemon_data_root, binary);
    };
    if let Some(expected_pid) = expected_pid {
        if expected_pid != initial.pid {
            return Err(format!(
                "observed replacement pid {expected_pid} does not match test daemon lock {initial:?}"
            ));
        }
        verify_live_daemon_identity(daemon_data_root, binary, &initial)?;
    } else if process_is_running(initial.pid) {
        verify_live_daemon_identity(daemon_data_root, binary, &initial)?;
    } else if !same_file(&initial.binary, binary)? {
        return Err(format!(
            "daemon lock binary {} is not the test copy {}",
            initial.binary.display(),
            binary.display()
        ));
    }
    let mut prepared = ctx_from_binary(temp, binary);
    prepared
        .env("CTX_DATA_ROOT", daemon_data_root)
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .args(["daemon", "disable", "--format=json"]);
    let disable = run_child_bounded(
        &mut std_command_from_assert(&prepared),
        DAEMON_DISABLE_COMMAND_TIMEOUT,
        DAEMON_DISABLE_REAP_TIMEOUT,
        "test daemon disable command",
    );
    let cooperative_wait = if matches!(&disable, Ok(BoundedCommandExit::Exited)) {
        DAEMON_STOP_TIMEOUT
    } else {
        Duration::ZERO
    };
    let fallback = (|| {
        if wait_for_process_exit(initial.pid, cooperative_wait) {
            return assert_daemon_released(daemon_data_root, &initial);
        }
        verify_live_daemon_identity(daemon_data_root, binary, &initial)?;
        terminate_process(initial.pid, false)
            .map_err(|error| format!("terminate verified test daemon {}: {error}", initial.pid))?;
        if !wait_for_process_exit(initial.pid, Duration::from_secs(1)) {
            verify_live_daemon_identity(daemon_data_root, binary, &initial)?;
            terminate_process(initial.pid, true).map_err(|error| {
                format!(
                    "force-terminate verified test daemon {}: {error}",
                    initial.pid
                )
            })?;
        }
        if !wait_for_process_exit(initial.pid, DAEMON_STOP_TIMEOUT) {
            return Err(format!(
                "verified test daemon {} remained alive after teardown",
                initial.pid
            ));
        }
        assert_daemon_released(daemon_data_root, &initial)
    })();
    match (disable, fallback) {
        (Ok(_), result) => result,
        (Err(disable), Ok(())) => Err(disable),
        (Err(disable), Err(fallback)) => Err(format!("{disable}; {fallback}")),
    }
}

fn remove_released_test_daemon_artifacts(
    daemon_data_root: &Path,
    expected_binary: &Path,
) -> Result<(), String> {
    let path = daemon_data_root.join("daemon/daemon.lock");
    if let Ok(bytes) = fs::read(&path) {
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse released daemon lock {}: {error}", path.display()))?;
        if value["released"] != true {
            return Err(format!(
                "daemon lock is still active without an attributable live owner: {value:#}"
            ));
        }
        let recorded_data_root = value["data_root"]
            .as_str()
            .map(Path::new)
            .ok_or_else(|| format!("released daemon lock has no data root: {value:#}"))?;
        if !same_path(recorded_data_root, daemon_data_root) {
            return Err(format!(
                "released daemon lock data root {} does not match test root {}",
                recorded_data_root.display(),
                daemon_data_root.display()
            ));
        }
        let recorded_binary = value["binary"]
            .as_str()
            .map(Path::new)
            .ok_or_else(|| format!("released daemon lock has no binary: {value:#}"))?;
        if !same_file(recorded_binary, expected_binary)? {
            return Err(format!(
                "released daemon lock binary {} is not the test copy {}",
                recorded_binary.display(),
                expected_binary.display()
            ));
        }
        if value["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .is_some_and(process_is_running)
        {
            return Err(format!(
                "released daemon lock still identifies a live process: {value:#}"
            ));
        }
    }
    remove_test_daemon_artifacts(daemon_data_root)
}

fn read_daemon_identity(daemon_data_root: &Path) -> Result<Option<DaemonIdentity>, String> {
    let path = daemon_data_root.join("daemon/daemon.lock");
    let Some(bytes) = fs::read(&path)
        .map(Some)
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error)
            }
        })
        .map_err(|error| format!("read daemon lock {}: {error}", path.display()))?
    else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse daemon lock {}: {error}", path.display()))?;
    if value["released"] == true {
        return Ok(None);
    }
    let pid = value["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| format!("daemon lock has no valid pid: {value:#}"))?;
    let owner_id = value["owner_id"]
        .as_str()
        .filter(|owner| !owner.is_empty())
        .ok_or_else(|| format!("daemon lock has no owner identity: {value:#}"))?
        .to_owned();
    let recorded_data_root = value["data_root"]
        .as_str()
        .map(Path::new)
        .ok_or_else(|| format!("daemon lock has no data-root identity: {value:#}"))?;
    if !same_path(recorded_data_root, daemon_data_root) {
        return Err(format!(
            "daemon lock data root {} does not match test root {}",
            recorded_data_root.display(),
            daemon_data_root.display()
        ));
    }
    let binary = value["binary"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| format!("daemon lock has no binary identity: {value:#}"))?;
    Ok(Some(DaemonIdentity {
        pid,
        owner_id,
        binary,
    }))
}

fn verify_live_daemon_identity(
    daemon_data_root: &Path,
    expected_binary: &Path,
    expected: &DaemonIdentity,
) -> Result<(), String> {
    let current = read_daemon_identity(daemon_data_root)?
        .ok_or_else(|| "daemon lock was released while its process remained alive".to_owned())?;
    if &current != expected {
        return Err(format!(
            "daemon lock identity changed before teardown: expected {expected:?}, found {current:?}"
        ));
    }
    if !same_file(&expected.binary, expected_binary)? {
        return Err(format!(
            "daemon lock binary {} is not the test copy {}",
            expected.binary.display(),
            expected_binary.display()
        ));
    }
    let process_binary = process_executable(expected.pid).ok_or_else(|| {
        format!(
            "cannot verify executable identity for test daemon {}",
            expected.pid
        )
    })?;
    if !same_file(&process_binary, expected_binary)? {
        return Err(format!(
            "daemon {} executable {} is not the test copy {}",
            expected.pid,
            process_binary.display(),
            expected_binary.display()
        ));
    }
    Ok(())
}

fn assert_daemon_released(
    daemon_data_root: &Path,
    expected: &DaemonIdentity,
) -> Result<(), String> {
    if process_is_running(expected.pid) {
        return Err(format!("test daemon {} is still running", expected.pid));
    }
    let path = daemon_data_root.join("daemon/daemon.lock");
    if let Ok(bytes) = fs::read(&path) {
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse released daemon lock {}: {error}", path.display()))?;
        if value["owner_id"] != expected.owner_id
            || value["pid"].as_u64() != Some(u64::from(expected.pid))
        {
            return Err(format!(
                "daemon lock changed owners during teardown; refusing cleanup: {value:#}"
            ));
        }
    }
    remove_test_daemon_artifacts(daemon_data_root)
}

fn assert_no_endpoint_identity(daemon_data_root: &Path) -> Result<(), String> {
    for name in [
        "daemon.lock",
        "daemon.guard",
        "query-endpoint.json",
        "source-refresh-endpoint.json",
        "query.sock",
        "source-refresh.sock",
    ] {
        let path = daemon_data_root.join("daemon").join(name);
        if path.exists() {
            return Err(format!(
                "test daemon artifact remained after teardown: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn remove_test_daemon_artifacts(daemon_data_root: &Path) -> Result<(), String> {
    for name in [
        "query-endpoint.json",
        "source-refresh-endpoint.json",
        "query.sock",
        "source-refresh.sock",
        "daemon.lock",
        "daemon.guard",
    ] {
        let path = daemon_data_root.join("daemon").join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "remove verified test daemon artifact {}: {error}",
                    path.display()
                ));
            }
        }
    }
    assert_no_endpoint_identity(daemon_data_root)
}

fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_is_running(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(DAEMON_STOP_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn same_file(left: &Path, right: &Path) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt as _;

    let left = fs::metadata(left)
        .map_err(|error| format!("inspect executable identity {}: {error}", left.display()))?;
    let right = fs::metadata(right)
        .map_err(|error| format!("inspect executable identity {}: {error}", right.display()))?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(windows)]
fn same_file(left: &Path, right: &Path) -> Result<bool, String> {
    Ok(same_path(left, right))
}

fn same_path(left: &Path, right: &Path) -> bool {
    let canonical = |path: &Path| {
        fs::canonicalize(path)
            .ok()
            .map(|path| path.to_string_lossy().to_lowercase())
    };
    matches!(
        (canonical(left), canonical(right)),
        (Some(left), Some(right)) if left == right
    )
}

#[cfg(target_os = "linux")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;

    const MAX_PATH_BYTES: usize = 4096;
    unsafe extern "C" {
        fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, size: u32) -> libc::c_int;
    }
    let mut buffer = vec![0_u8; MAX_PATH_BYTES];
    let length = unsafe {
        proc_pidpath(
            libc::pid_t::try_from(pid).ok()?,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    if length <= 0 {
        return None;
    }
    CStr::from_bytes_until_nul(&buffer)
        .ok()
        .map(|path| PathBuf::from(path.to_string_lossy().into_owned()))
}

#[cfg(target_os = "freebsd")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;

    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PATHNAME,
        libc::c_int::try_from(pid).ok()?,
    ];
    let mut buffer = vec![0_u8; libc::PATH_MAX as usize];
    let mut length = buffer.len();
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || length == 0 {
        return None;
    }
    let end = buffer[..length]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(length);
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buffer[..end])))
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))
))]
fn process_executable(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn process_executable(pid: u32) -> Option<PathBuf> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buffer = vec![0_u16; 32_768];
    let mut length = u32::try_from(buffer.len()).ok()?;
    let queried =
        unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) } != 0;
    unsafe {
        CloseHandle(handle);
    }
    queried.then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize])))
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    let running = unsafe { libc::kill(pid, 0) } == 0;
    #[cfg(target_os = "linux")]
    if running {
        return fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, rest)| rest.to_owned()))
            .and_then(|rest| rest.split_whitespace().next().map(str::to_owned))
            .is_some_and(|state| state != "Z");
    }
    running
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, STILL_ACTIVE},
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut exit_code = 0;
    let queried = unsafe { GetExitCodeProcess(handle, &mut exit_code) } != 0;
    unsafe {
        CloseHandle(handle);
    }
    queried && exit_code == STILL_ACTIVE as u32
}

#[cfg(unix)]
fn terminate_process(pid: u32, force: bool) -> std::io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn terminate_process(pid: u32, _force: bool) -> std::io::Result<()> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE},
    };

    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let terminated = unsafe { TerminateProcess(handle, 137) } != 0;
    unsafe {
        CloseHandle(handle);
    }
    if terminated {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
