// Adapted from Sift tests, MIT; see this crate’s NOTICE.
use crate::state;

use state::{Event, Settings};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sift-state-test-{}-{}-{}",
            std::process::id(),
            state::unix_millis(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn age_directory(path: &Path) {
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES,
        };
        fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
    };
    #[cfg(not(windows))]
    let file = fs::File::open(path).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .unwrap();
}
fn event() -> Event {
    Event {
        unix_millis: 86_400_001,
        command: "Bash".into(),
        input_tokens: Some(100),
        output_tokens: Some(40),
        input_bytes: 1000,
        output_bytes: 300,
        duration_ms: 12,
        exit_code: Some(0),
        source: Some("hook".into()),
        original_id: None,
    }
}
fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn defaults_are_read_only_and_corrupt_config_is_preserved() {
    let temp = Temp::new();
    let path = temp.path().join("nested/config.json");
    assert_eq!(Settings::load_from(&path).unwrap(), Settings::default());
    assert!(!path.parent().unwrap().exists());
    Settings::create_default(&path).unwrap();
    assert_eq!(Settings::load_from(&path).unwrap(), Settings::default());
    fs::write(&path, b"{ broken config").unwrap();
    assert!(Settings::load_from(&path).is_err());
    assert!(Settings::create_default(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"{ broken config");
    fs::write(&path, br#"{"exclude_commands":["git *"]}"#).unwrap();
    assert!(Settings::load_from(&path).is_err());
    fs::write(&path, br#"{"exclude_commands":["git"]}"#).unwrap();
    let settings = Settings::load_from(&path).unwrap();
    assert!(settings.excludes("/usr/bin/git"));
    assert!(!settings.excludes("git-lfs"));
    assert_eq!(settings.semantic_selection.mode, state::SemanticMode::Off);
    assert!(settings.semantic_selection.allowed_projects.is_empty());
}

#[test]
fn semantic_original_and_receipt_are_private_and_do_not_change_usage() {
    let temp = Temp::new();
    let settings = Settings {
        record_usage: false,
        keep_originals: false,
        originals_max_entries: 2,
        originals_max_bytes: 1024,
        ..Default::default()
    };
    let id = state::save_semantic_original_at(temp.path(), &settings, b"complete raw text")
        .unwrap()
        .unwrap();
    let mut recalled = Vec::new();
    state::recall_at(temp.path(), &id, false, &mut recalled).unwrap();
    assert_eq!(recalled, b"complete raw text");
    state::record_semantic_receipt_at(
        temp.path(),
        &state::SemanticReceipt {
            unix_millis: 1,
            status: "ok",
            disposition: "selected",
            model: "jev-1.13.0",
            http_status: Some(200),
            usage: Some(state::SemanticUsage {
                input_tokens: 20,
                output_tokens: 2,
            }),
            latency_ms: 8,
            passage_count: 3,
            selected_count: 1,
            omitted_count: 2,
            ordinary_tokens: 100,
            semantic_tokens: Some(20),
            memoized: false,
        },
    )
    .unwrap();
    assert!(state::usage_at(temp.path()).unwrap().events.is_empty());
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(temp.path().join("semantic.jsonl")).unwrap()).unwrap();
    assert_eq!(receipt["disposition"], "selected");
    assert!(receipt.get("task").is_none());
    assert!(receipt.get("text").is_none());

    fs::write(temp.path().join("semantic.jsonl"), b"interrupted").unwrap();
    state::record_semantic_receipt_at(
        temp.path(),
        &state::SemanticReceipt {
            unix_millis: 2,
            status: "fallback",
            disposition: "ordinary",
            model: "jev-1.13.0",
            http_status: None,
            usage: None,
            latency_ms: 1,
            passage_count: 0,
            selected_count: 0,
            omitted_count: 0,
            ordinary_tokens: 3,
            semantic_tokens: None,
            memoized: false,
        },
    )
    .unwrap();
    let repaired = fs::read_to_string(temp.path().join("semantic.jsonl")).unwrap();
    let mut lines = repaired.lines();
    assert_eq!(lines.next(), Some("interrupted"));
    let receipt: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(receipt["disposition"], "ordinary");
    assert_eq!(lines.next(), None);
}
#[test]
fn concurrent_writers_produce_complete_records() {
    let temp = Temp::new();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let path = temp.path();
            scope.spawn(move || {
                for _ in 0..20 {
                    state::record_at(path, &Settings::default(), event(), None).unwrap();
                }
            });
        }
    });
    let usage = state::usage_at(temp.path()).unwrap();
    // Contending optional writes may be skipped; every accepted line is intact.
    assert!((1..=160).contains(&usage.events.len()));
    assert_eq!(usage.malformed_records, 0);
    assert!(usage.events.iter().all(|e| e.command == "Bash"
        && e.input_tokens == Some(100)
        && e.output_tokens == Some(40)));
    assert_eq!(usage.totals().saved_tokens, usage.events.len() as i128 * 60);
}
#[test]
fn unmeasured_records_and_malformed_lines_do_not_inflate_savings() {
    use std::io::Write;
    let temp = Temp::new();
    state::record_at(temp.path(), &Settings::default(), event(), None).unwrap();
    let mut raw = event();
    raw.input_tokens = None;
    raw.output_tokens = None;
    state::record_at(temp.path(), &Settings::default(), raw, None).unwrap();
    let mut partial = event();
    partial.output_tokens = None;
    state::record_at(temp.path(), &Settings::default(), partial, None).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(temp.path().join("metrics.jsonl"))
        .unwrap()
        .write_all(b"bad JSON\n{\"command\":\"private args\"}\n")
        .unwrap();
    let usage = state::usage_at(temp.path()).unwrap();
    assert_eq!(usage.malformed_records, 2);
    let totals = usage.totals();
    assert_eq!(
        (
            totals.events,
            totals.measured_events,
            totals.input_tokens,
            totals.output_tokens,
            totals.saved_tokens
        ),
        (3, 1, 100, 40, 60)
    );
    let mut output = Vec::new();
    state::gain_at(
        temp.path(),
        &args(&["--json", "--history", "--daily"]),
        &mut output,
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["malformed_records"], 2);
    assert_eq!(json["daily_utc"]["1970-01-02"]["saved_tokens"], 60);
    assert!(json["history"][1]["input_tokens"].is_null());
    assert!(state::gain_at(temp.path(), &args(&["--reset", "--json"]), &mut output).is_err());
    assert_eq!(state::usage_at(temp.path()).unwrap().events.len(), 3);
}
#[test]
fn raw_bytes_are_opt_in_separate_and_recall_rejects_traversal() {
    let temp = Temp::new();
    let stdout = b"\xffprivate stdout\0";
    let stderr = b"\xfeprivate stderr\n";
    state::record_at(
        temp.path(),
        &Settings::default(),
        event(),
        Some((stdout, stderr)),
    )
    .unwrap();
    assert!(!temp.path().join("originals").exists());
    let log = fs::read_to_string(temp.path().join("metrics.jsonl")).unwrap();
    assert!(!log.contains("private"));
    assert!(
        state::usage_at(temp.path()).unwrap().events[0]
            .original_id
            .is_none()
    );
    let settings = Settings {
        keep_originals: true,
        ..Default::default()
    };
    state::record_at(temp.path(), &settings, event(), Some((stdout, stderr))).unwrap();
    let entries = state::list_originals_at(temp.path()).unwrap();
    assert_eq!(entries.len(), 1);
    let id = &entries[0].id;
    assert_eq!(
        state::usage_at(temp.path()).unwrap().events[1]
            .original_id
            .as_ref(),
        Some(id)
    );
    for (is_stderr, expected) in [(false, stdout.as_slice()), (true, stderr.as_slice())] {
        let mut output = Vec::new();
        state::recall_at(temp.path(), id, is_stderr, &mut output).unwrap();
        assert_eq!(output, expected);
    }
    for id in ["../config", "/tmp/a", "..", "a/b", "a\\b", ""] {
        assert!(state::recall_at(temp.path(), id, false, &mut Vec::new()).is_err());
    }
    state::reset_at(temp.path()).unwrap();
    assert!(state::usage_at(temp.path()).unwrap().events.is_empty());
    let mut output = Vec::new();
    state::recall_at(temp.path(), id, false, &mut output).unwrap();
    assert_eq!(output, stdout);
}
#[test]
fn retention_evicts_oldest_originals_and_rotates_one_metrics_backup() {
    let temp = Temp::new();
    let settings = Settings {
        keep_originals: true,
        ..Default::default()
    };
    state::record_at(temp.path(), &settings, event(), Some((b"first", b""))).unwrap();
    let first = state::list_originals_at(temp.path()).unwrap().remove(0).id;
    // Set an old timestamp deterministically; no timing assumptions between writes.
    age_directory(&temp.path().join("originals").join(&first));
    for _ in 0..101 {
        state::record_at(temp.path(), &settings, event(), Some((b"x", b"y"))).unwrap();
    }
    let entries = state::list_originals_at(temp.path()).unwrap();
    assert_eq!(entries.len(), 100);
    assert!(!entries.iter().any(|entry| entry.id == first));
    let log = temp.path().join("metrics.jsonl");
    // A full log forces rotation on the next write. The old backup is replaced.
    fs::write(temp.path().join("metrics.1.jsonl"), b"older backup").unwrap();
    fs::write(&log, vec![b'\n'; 10 * 1024 * 1024]).unwrap();
    state::record_at(temp.path(), &Settings::default(), event(), None).unwrap();
    assert_eq!(
        fs::metadata(temp.path().join("metrics.1.jsonl"))
            .unwrap()
            .len(),
        10 * 1024 * 1024
    );
    let current: Event = serde_json::from_slice(&fs::read(log).unwrap()).unwrap();
    assert_eq!(current.command, "Bash");
}
#[test]
fn command_lines_are_rejected_before_any_write() {
    let temp = Temp::new();
    for command in ["git --password secret", "/usr/bin/git", "git\nsecret"] {
        let mut input = event();
        input.command = command.into();
        assert!(state::record_at(temp.path(), &Settings::default(), input, None).is_err());
    }
    assert!(!temp.path().join("metrics.jsonl").exists());
}
#[cfg(unix)]
#[test]
fn files_are_private_and_recall_rejects_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = Temp::new();
    state::record_at(
        temp.path(),
        &Settings {
            keep_originals: true,
            ..Default::default()
        },
        event(),
        Some((b"data", b"")),
    )
    .unwrap();
    let entry = state::list_originals_at(temp.path()).unwrap().remove(0);
    for path in [
        temp.path().join("metrics.jsonl"),
        temp.path().join("state.lock"),
        temp.path().join("originals").join(&entry.id).join("stdout"),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let stream = temp.path().join("originals").join(&entry.id).join("stdout");
    fs::remove_file(&stream).unwrap();
    symlink(temp.path().join("metrics.jsonl"), &stream).unwrap();
    assert!(state::recall_at(temp.path(), &entry.id, false, &mut Vec::new()).is_err());
}

#[test]
fn interrupted_record_is_preserved_and_does_not_swallow_next_event() {
    let temp = Temp::new();
    fs::write(temp.path().join("metrics.jsonl"), b"{broken").unwrap();
    state::record_at(temp.path(), &Settings::default(), event(), None).unwrap();
    assert!(
        fs::read(temp.path().join("metrics.jsonl"))
            .unwrap()
            .starts_with(b"{broken\n")
    );
    let usage = state::usage_at(temp.path()).unwrap();
    assert_eq!(usage.events.len(), 1);
    assert_eq!(usage.malformed_records, 1);
}

#[test]
fn recording_disabled_creates_nothing_and_enabled_only_controls_output() {
    let temp = Temp::new();
    let dir = temp.path().join("absent");
    let settings = Settings {
        record_usage: false,
        keep_originals: true,
        ..Default::default()
    };
    state::record_at(&dir, &settings, event(), Some((b"private", b"error"))).unwrap();
    assert!(!dir.exists());
    let settings = Settings {
        enabled: false,
        ..Default::default()
    };
    state::record_at(&dir, &settings, event(), None).unwrap();
    let before = fs::read(dir.join("metrics.jsonl")).unwrap();
    state::record_at(
        &dir,
        &Settings {
            record_usage: false,
            ..settings
        },
        event(),
        None,
    )
    .unwrap();
    assert_eq!(fs::read(dir.join("metrics.jsonl")).unwrap(), before);
}
#[test]
fn daily_dates_cover_epoch_leap_centuries_and_current_date() {
    let temp = Temp::new();
    // Fixed Unix seconds, checked independently with Python's datetime UTC.
    let fixtures = [
        (0, "1970-01-01"),
        (86400, "1970-01-02"),
        (951782400, "2000-02-29"),
        (951868800, "2000-03-01"),
        (1709164800, "2024-02-29"),
        (1789603200, "2026-09-17"),
        (4107542400, "2100-03-01"),
        (13574563200, "2400-02-29"),
    ];
    for (seconds, _) in fixtures {
        let mut input = event();
        input.unix_millis = seconds * 1000;
        state::record_at(temp.path(), &Settings::default(), input, None).unwrap();
    }
    let mut output = Vec::new();
    state::gain_at(temp.path(), &args(&["--json", "--daily"]), &mut output).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["daily_utc"].as_object().unwrap().len(), fixtures.len());
    for (_, date) in fixtures {
        assert_eq!(json["daily_utc"][date]["saved_tokens"], 60, "{date}");
    }
    for flag in ["--daily", "--graph"] {
        output.clear();
        state::gain_at(temp.path(), &args(&[flag]), &mut output).unwrap();
        let text = String::from_utf8(output.clone()).unwrap();
        for (_, date) in fixtures {
            assert!(text.contains(&format!("{date}: 60")), "{text}");
        }
    }
}
#[test]
fn text_history_is_readable_and_help_does_not_create_state() {
    let temp = Temp::new();
    let dir = temp.path().join("absent");
    let mut output = Vec::new();
    state::gain_at(&dir, &args(&["--help"]), &mut output).unwrap();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("Usage: ctx sift gain")
    );
    assert!(!dir.exists());
    state::record_at(&dir, &Settings::default(), event(), None).unwrap();
    let mut input = event();
    input.input_tokens = None;
    input.output_tokens = None;
    state::record_at(&dir, &Settings::default(), input, None).unwrap();
    let mut output = Vec::new();
    state::gain_at(&dir, &args(&["--history"]), &mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("1970-01-02 00:00:00 UTC  Bash  100 -> 40 tokens (60 saved)"));
    assert!(text.contains("Bash  tokens unmeasured"));
    assert!(!text.contains("{\""));
}

#[test]
fn contended_recording_skips_without_waiting_or_saving_originals() {
    use std::sync::mpsc;
    use std::time::Duration;
    let temp = Temp::new();
    state::record_at(temp.path(), &Settings::default(), event(), None).unwrap();
    let held = fs::File::options()
        .write(true)
        .open(temp.path().join("state.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&held).unwrap();
    // fs2 reports native contention codes: Windows ERROR_LOCK_VIOLATION need
    // not map to WouldBlock. Exercise the actual platform error, then recording.
    let probe = fs::File::options()
        .write(true)
        .open(temp.path().join("state.lock"))
        .unwrap();
    let error = fs2::FileExt::try_lock_exclusive(&probe).unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        fs2::lock_contended_error().raw_os_error()
    );
    #[cfg(windows)]
    assert_eq!(error.raw_os_error(), Some(33));
    drop(probe);
    let (done, result) = mpsc::channel();
    let dir = temp.path().to_owned();
    let worker = std::thread::spawn(move || {
        done.send(state::record_at(
            &dir,
            &Settings {
                keep_originals: true,
                ..Default::default()
            },
            event(),
            Some((b"raw", b"")),
        ))
        .unwrap();
    });
    let completed = result.recv_timeout(Duration::from_secs(2));
    drop(held);
    worker.join().unwrap();
    completed
        .expect("recording waited for a busy lock")
        .unwrap();
    assert_eq!(state::usage_at(temp.path()).unwrap().events.len(), 1);
    assert!(!temp.path().join("originals").exists());
}

#[cfg(unix)]
#[test]
fn paused_recall_releases_lock_and_command_can_complete() {
    use std::io::{self, Write};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    struct Paused {
        entered: Option<mpsc::Sender<()>>,
        resume: mpsc::Receiver<()>,
        bytes: Vec<u8>,
    }
    impl Write for Paused {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.resume.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let temp = Temp::new();
    state::record_at(
        temp.path(),
        &Settings {
            keep_originals: true,
            ..Default::default()
        },
        event(),
        Some((b"saved bytes", b"")),
    )
    .unwrap();
    let id = state::list_originals_at(temp.path()).unwrap().remove(0).id;
    let (entered, blocked) = mpsc::channel();
    let (resume, receiver) = mpsc::channel();
    let dir = temp.path().to_owned();
    let worker = std::thread::spawn(move || {
        let mut sink = Paused {
            entered: Some(entered),
            resume: receiver,
            bytes: Vec::new(),
        };
        state::recall_at(&dir, &id, false, &mut sink).unwrap();
        sink.bytes
    });
    blocked.recv_timeout(Duration::from_secs(2)).unwrap();
    let file = fs::File::options()
        .write(true)
        .open(temp.path().join("state.lock"))
        .unwrap();
    let lock_error = fs2::FileExt::try_lock_exclusive(&file).err();
    drop(file);
    let mut child = crate::test_support::command(["run", "--", "/bin/sh", "-c", "exit 0"])
        .env("SIFT_STATE_DIR", temp.path())
        .env("SIFT_CONFIG_DIR", temp.path().join("config"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    resume.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), b"saved bytes");
    assert!(
        lock_error.is_none(),
        "recall holds the state lock while blocked on its consumer: {lock_error:?}"
    );
    assert!(status.expect("command blocked behind recall").success());
    assert_eq!(state::usage_at(temp.path()).unwrap().events.len(), 2);
}

#[test]
fn interrupted_originals_are_pruned_and_retained_complete_streams_survive() {
    let temp = Temp::new();
    let settings = Settings {
        keep_originals: true,
        ..Default::default()
    };
    state::record_at(
        temp.path(),
        &settings,
        event(),
        Some((b"complete stdout", b"complete stderr")),
    )
    .unwrap();
    let first = state::list_originals_at(temp.path()).unwrap().remove(0).id;
    let originals = temp.path().join("originals");
    for name in ["dead-beef", "dead-beef.pending"] {
        fs::create_dir(originals.join(name)).unwrap();
        fs::write(originals.join(name).join("stdout"), b"interrupted").unwrap();
    }
    // Save directly without listing first: normal recording itself repairs leftovers.
    state::record_at(temp.path(), &settings, event(), Some((b"next", b""))).unwrap();
    assert!(!originals.join("dead-beef").exists());
    assert!(!originals.join("dead-beef.pending").exists());
    assert_eq!(state::list_originals_at(temp.path()).unwrap().len(), 2);
    for (stderr, expected) in [(false, b"complete stdout"), (true, b"complete stderr")] {
        let mut bytes = Vec::new();
        state::recall_at(temp.path(), &first, stderr, &mut bytes).unwrap();
        assert_eq!(bytes, expected);
    }
    for _ in 0..100 {
        state::record_at(temp.path(), &settings, event(), Some((b"later", b""))).unwrap();
    }
    assert_eq!(state::list_originals_at(temp.path()).unwrap().len(), 100);
    assert_eq!(fs::read_dir(originals).unwrap().count(), 100);
    assert_eq!(state::usage_at(temp.path()).unwrap().events.len(), 102);
}

#[test]
fn legacy_records_and_project_filters_preserve_measured_meaning() {
    let temp = Temp::new();
    let legacy = serde_json::to_string(&event()).unwrap() + "\n";
    fs::write(temp.path().join("metrics.1.jsonl"), &legacy).unwrap();
    let project_a = temp.path().join("project-a");
    let project_b = temp.path().join("project-b");
    fs::create_dir_all(project_a.join(".git")).unwrap();
    fs::create_dir_all(project_a.join("nested")).unwrap();
    fs::create_dir_all(&project_b).unwrap();
    // Git worktrees use a .git file. Identical basenames do not identify a project.
    fs::write(project_b.join(".git"), "gitdir: synthetic\n").unwrap();
    let a = state::project_at(&project_a).unwrap();
    let b = state::project_at(&project_b).unwrap();
    assert_eq!(state::project_at(&project_a.join("nested")).unwrap(), a);
    for (project, measured) in [(&a, true), (&b, true), (&a, false)] {
        let mut input = event();
        if !measured {
            input.output_tokens = None;
        }
        state::record_project_at(
            temp.path(),
            &Settings::default(),
            input,
            None,
            Some(project),
        )
        .unwrap();
    }
    let usage = state::usage_at(temp.path()).unwrap();
    assert_eq!(usage.events.len(), 4);
    assert!(usage.events[0].project.is_none());
    assert_eq!(usage.totals().saved_tokens, 180);
    let filtered = state::query_at(
        temp.path(),
        &state::UsageQuery {
            project: Some(a.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        (
            filtered.totals().events,
            filtered.totals().measured_events,
            filtered.totals().saved_tokens
        ),
        (2, 1, 60)
    );
    let mut output = Vec::new();
    state::gain_at(
        temp.path(),
        &args(&["--json", "--history", "--project", &a]),
        &mut output,
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["totals"]["events"], 2);
    assert_eq!(json["history"][0]["project"], a);
    assert_eq!(
        fs::read_to_string(temp.path().join("metrics.1.jsonl")).unwrap(),
        legacy
    );
}

#[test]
fn utc_periods_and_intersected_time_source_command_filters() {
    let temp = Temp::new();
    // Fixed UTC dates: 2024-12-29 (Sunday), 12-30 (Monday), 2025-01-01, 01-06.
    for (millis, command, source) in [
        (1735430400000, "git", "run"),
        (1735516800000, "git", "run"),
        (1735689600000, "git", "hook"),
        (1736121600000, "cargo", "run"),
    ] {
        let mut input = event();
        input.unix_millis = millis;
        input.command = command.into();
        input.source = Some(source.into());
        state::record_at(temp.path(), &Settings::default(), input, None).unwrap();
    }
    let mut output = Vec::new();
    state::gain_at(
        temp.path(),
        &args(&["--json", "--weekly", "--monthly"]),
        &mut output,
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["weekly_utc"]["2024-12-23"]["events"], 1);
    assert_eq!(json["weekly_utc"]["2024-12-30"]["events"], 2);
    assert_eq!(json["weekly_utc"]["2025-01-06"]["events"], 1);
    assert_eq!(json["monthly_utc"]["2024-12"]["saved_tokens"], 120);
    assert_eq!(json["monthly_utc"]["2025-01"]["saved_tokens"], 120);
    output.clear();
    state::gain_at(
        temp.path(),
        &args(&[
            "--json",
            "--since",
            "2024-12-30",
            "--until",
            "1736121600000",
            "--command",
            "git",
            "--source",
            "run",
        ]),
        &mut output,
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["totals"]["events"], 1);
    for options in [
        vec!["--since", "2023-02-29"],
        vec!["--until", "2024-04-31"],
        vec!["--since", "2024-01-02", "--until", "2024-01-01"],
        vec!["--since", "2024-01-01", "--until", "2024-01-01"],
        vec!["--json", "--csv"],
        vec!["--source"],
        vec!["--command", "git status"],
    ] {
        assert!(
            state::gain_at(temp.path(), &args(&options), &mut Vec::new()).is_err(),
            "{options:?}"
        );
    }
    state::gain_at(
        temp.path(),
        &args(&["--since", "2024-02-29", "--until", "2024-03-01"]),
        &mut Vec::new(),
    )
    .unwrap();
    // The first Unix week starts before the epoch.
    state::record_at(temp.path(), &Settings::default(), event(), None).unwrap();
    output.clear();
    state::gain_at(
        temp.path(),
        &args(&["--json", "--weekly", "--until", "1970-01-05"]),
        &mut output,
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["weekly_utc"]["1969-12-29"]["events"], 1);
}

#[test]
fn csv_escapes_project_fields_and_distinguishes_partial_measurements() {
    let temp = Temp::new();
    let project = "workspace, \"quoted\"\r\nnext";
    let mut input = event();
    input.input_tokens = Some(20);
    input.output_tokens = Some(40);
    state::record_project_at(
        temp.path(),
        &Settings::default(),
        input.clone(),
        None,
        Some(project),
    )
    .unwrap();
    input.output_tokens = None;
    state::record_project_at(temp.path(), &Settings::default(), input, None, None).unwrap();
    let mut output = Vec::new();
    state::gain_at(
        temp.path(),
        &args(&["--format", "csv", "--history"]),
        &mut output,
    )
    .unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("86400001,\"workspace, \"\"quoted\"\"\r\nnext\",Bash,hook,true,20,40,-20,1000,300,12,0,\r\n"), "{text:?}");
    assert!(
        text.contains("86400001,,Bash,hook,false,20,,,1000,300,12,0,\r\n"),
        "{text:?}"
    );
    let mut output = Vec::new();
    state::gain_at(temp.path(), &args(&["--csv"]), &mut output).unwrap();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("all,,2,1,20,40,-20,2000,600,0\r\n")
    );
    assert!(
        state::gain_at(
            temp.path(),
            &args(&["--csv", "--history", "--weekly"]),
            &mut Vec::new()
        )
        .is_err()
    );
}

#[test]
fn recall_navigation_filters_then_limits_and_preserves_bytes() {
    let temp = Temp::new();
    let settings = Settings {
        keep_originals: true,
        ..Default::default()
    };
    let stdout = b"skip\r\nmatch first\r\nignore\nmatch \xff second\nmatch last";
    state::record_at(
        temp.path(),
        &settings,
        event(),
        Some((stdout, b"err\nlast")),
    )
    .unwrap();
    let id = state::list_originals_at(temp.path()).unwrap().remove(0).id;
    let mut output = Vec::new();
    state::recall_command_at(
        temp.path(),
        &args(&[&id, "--from", "3", "--grep", "match", "--lines", "1"]),
        &mut output,
    )
    .unwrap();
    assert_eq!(output, b"match \xff second\n");
    output.clear();
    state::recall_command_at(
        temp.path(),
        &args(&[&id, "--from", "4", "--grep", "match", "--lines", "2"]),
        &mut output,
    )
    .unwrap();
    assert_eq!(output, b"match \xff second\nmatch last");
    output.clear();
    state::recall_command_at(
        temp.path(),
        &args(&[&id, "--stderr", "--from", "2"]),
        &mut output,
    )
    .unwrap();
    assert_eq!(output, b"last");
    output.clear();
    state::recall_command_at(temp.path(), &args(&[&id]), &mut output).unwrap();
    assert_eq!(output, stdout);
    for options in [
        vec![&id, "--from", "0"],
        vec![&id, "--lines", "0"],
        vec![&id, "--lines", "10001"],
        vec![&id, "--grep"],
        vec![&id, "--unknown"],
    ] {
        assert!(state::recall_command_at(temp.path(), &args(&options), &mut Vec::new()).is_err());
    }
    let large = "no\n".repeat(300) + &"match\n".repeat(250);
    state::record_at(
        temp.path(),
        &settings,
        event(),
        Some((large.as_bytes(), b"")),
    )
    .unwrap();
    let last = state::usage_at(temp.path())
        .unwrap()
        .events
        .last()
        .unwrap()
        .original_id
        .clone()
        .unwrap();
    output.clear();
    state::recall_command_at(temp.path(), &args(&[&last, "--grep", "match"]), &mut output).unwrap();
    assert_eq!(output, "match\n".repeat(200).as_bytes());
}

#[test]
fn exact_original_ids_win_and_prefixes_must_be_unique() {
    let temp = Temp::new();
    for (id, content) in [("abcd", "exact"), ("abcd-1", "longer"), ("abce", "other")] {
        let dir = temp.path().join("originals").join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("stdout"), content).unwrap();
        fs::write(dir.join("stderr"), b"").unwrap();
    }
    let mut output = Vec::new();
    state::recall_at(temp.path(), "abcd", false, &mut output).unwrap();
    assert_eq!(output, b"exact");
    output.clear();
    state::recall_at(temp.path(), "abcd-", false, &mut output).unwrap();
    assert_eq!(output, b"longer");
    assert!(
        state::recall_at(temp.path(), "abc", false, &mut Vec::new())
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    assert!(
        state::recall_at(temp.path(), "ffff", false, &mut Vec::new())
            .unwrap_err()
            .to_string()
            .contains("not found")
    );
    assert_eq!(state::list_originals_at(temp.path()).unwrap().len(), 3);
}

#[test]
fn retention_caps_load_from_old_and_new_config_and_keep_metrics_for_large_originals() {
    let temp = Temp::new();
    let path = temp.path().join("config.json");
    fs::write(&path, r#"{"keep_originals":true}"#).unwrap();
    let old = Settings::load_from(&path).unwrap();
    assert_eq!(
        (
            old.originals_max_entries,
            old.originals_max_bytes,
            old.originals_max_days
        ),
        (100, 104857600, 30)
    );
    fs::write(&path, r#"{"keep_originals":true,"originals_max_entries":2,"originals_max_bytes":5,"originals_max_days":1}"#).unwrap();
    let settings = Settings::load_from(&path).unwrap();
    state::record_at(temp.path(), &settings, event(), Some((b"abc", b""))).unwrap();
    let first = state::list_originals_at(temp.path()).unwrap().remove(0).id;
    state::record_at(temp.path(), &settings, event(), Some((b"de", b"f"))).unwrap();
    let entries = state::list_originals_at(temp.path()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_ne!(entries[0].id, first);
    // Too large for retention: preserve the measurement without storing raw bytes.
    state::record_at(temp.path(), &settings, event(), Some((b"123456", b""))).unwrap();
    let usage = state::usage_at(temp.path()).unwrap();
    assert_eq!(usage.events.len(), 3);
    assert!(usage.events[2].original_id.is_none());
    let old_dir = temp.path().join("originals").join(&entries[0].id);
    age_directory(&old_dir);
    state::record_at(temp.path(), &settings, event(), Some((b"z", b""))).unwrap();
    assert!(!old_dir.exists());
    let count_settings = Settings {
        originals_max_entries: 1,
        originals_max_bytes: 100,
        ..settings
    };
    state::record_at(temp.path(), &count_settings, event(), Some((b"next", b""))).unwrap();
    assert_eq!(state::list_originals_at(temp.path()).unwrap().len(), 1);
    for key in [
        "originals_max_entries",
        "originals_max_bytes",
        "originals_max_days",
    ] {
        fs::write(&path, format!("{{\"{key}\":0}}")).unwrap();
        assert!(Settings::load_from(&path).is_err());
    }
}

#[cfg(unix)]
#[test]
fn runner_records_project_and_cli_queries_are_isolated_and_private() {
    use std::os::unix::fs::PermissionsExt;
    let temp = Temp::new();
    let project = temp.path().join("project");
    fs::create_dir_all(project.join("nested")).unwrap();
    fs::write(project.join(".git"), "gitdir: synthetic\n").unwrap();
    let state = temp.path().join("state");
    let config = temp.path().join("config");
    let mut run =
        crate::test_support::command(["run", "--", "/bin/sh", "-c", "printf 'synthetic\\n'"]);
    let result = run
        .current_dir(project.join("nested"))
        .env("SIFT_STATE_DIR", &state)
        .env("SIFT_CONFIG_DIR", &config)
        .output()
        .map(crate::test_support::clean)
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = crate::test_support::command(["gain", "--project", "--json", "--history"])
        .current_dir(&project)
        .env("SIFT_STATE_DIR", &state)
        .env("SIFT_CONFIG_DIR", &config)
        .output()
        .map(crate::test_support::clean)
        .unwrap();
    assert!(result.status.success());
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(json["totals"]["events"], 1);
    assert_eq!(
        json["history"][0]["project"],
        state::project_at(&project).unwrap()
    );
    assert!(!state.join("originals").exists());
    assert!(!config.exists());
    assert_eq!(
        fs::metadata(state.join("metrics.jsonl"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn semantic_cli_canonicalizes_one_project_and_never_stores_the_key() {
    let temp = Temp::new();
    let project = temp.path().join("project");
    fs::create_dir_all(project.join("nested")).unwrap();
    fs::write(project.join(".git"), "gitdir: synthetic\n").unwrap();
    let config = temp.path().join("config");
    let run = |args: &[&str]| {
        crate::test_support::command(args)
            .env("SIFT_CONFIG_DIR", &config)
            .env("SIFT_STATE_DIR", temp.path().join("state"))
            .env("TYPESAFE_API_KEY", "must-not-be-stored")
            .output()
            .map(crate::test_support::clean)
            .unwrap()
    };
    let nested = project.join("nested");
    let result = run(&["semantic", "enable", "--project", nested.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("current task"));
    let bytes = fs::read(config.join("config.json")).unwrap();
    assert!(
        !bytes
            .windows("must-not-be-stored".len())
            .any(|part| part == b"must-not-be-stored")
    );
    let settings: Settings = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        settings.semantic_selection.mode,
        state::SemanticMode::Select
    );
    assert_eq!(
        settings.semantic_selection.allowed_projects,
        [state::project_at(&project).unwrap()]
    );
    assert!(
        run(&["semantic", "shadow", "--project", nested.to_str().unwrap()])
            .status
            .success()
    );
    let settings = Settings::load_from(&config.join("config.json")).unwrap();
    assert_eq!(
        settings.semantic_selection.mode,
        state::SemanticMode::Shadow
    );
    assert_eq!(settings.semantic_selection.allowed_projects.len(), 1);
    assert!(run(&["semantic", "disable"]).status.success());
    assert_eq!(
        Settings::load_from(&config.join("config.json"))
            .unwrap()
            .semantic_selection
            .mode,
        state::SemanticMode::Off
    );
    assert!(fs::read_dir(&config).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[cfg(unix)]
#[test]
fn semantic_cli_rejects_a_config_symlink_without_touching_its_target() {
    use std::os::unix::fs::symlink;
    let temp = Temp::new();
    let project = temp.path().join("project");
    let config = temp.path().join("config");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&config).unwrap();
    let target = temp.path().join("external.json");
    let original = serde_json::to_vec_pretty(&Settings::default()).unwrap();
    fs::write(&target, &original).unwrap();
    symlink(&target, config.join("config.json")).unwrap();
    let result = crate::test_support::command([
        "semantic",
        "enable",
        "--project",
        project.to_str().unwrap(),
    ])
    .env("SIFT_CONFIG_DIR", &config)
    .env("SIFT_STATE_DIR", temp.path().join("state"))
    .output()
    .map(crate::test_support::clean)
    .unwrap();
    assert!(!result.status.success());
    assert!(
        fs::symlink_metadata(config.join("config.json"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&target).unwrap(), original);
}
