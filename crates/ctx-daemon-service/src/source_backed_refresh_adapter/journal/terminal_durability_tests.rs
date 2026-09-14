use super::*;
use ctx_history_refresh::{
    RefreshEngine, RefreshOperation, RefreshRuntime, RefreshRuntimeMetadata,
    SourceBackedRefreshExecution,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

fn isolated_data_root() -> Result<tempfile::TempDir> {
    // Explicit Linux experiment root avoids build-wrapper TMPDIR conventions.
    #[cfg(target_os = "linux")]
    let parent = std::path::PathBuf::from("/tmp");
    #[cfg(not(target_os = "linux"))]
    let parent = std::env::temp_dir();
    Ok(tempfile::Builder::new()
        .prefix("ctx-terminal-durability-")
        .tempdir_in(parent)?)
}

#[derive(Default)]
struct FaultJournal {
    inner: DaemonRefreshJournal,
    fail_parent: AtomicBool,
    events: Mutex<Vec<String>>,
    writes: AtomicUsize,
    bytes: AtomicUsize,
}
impl RefreshJournal for FaultJournal {
    fn load(&self, root: &Path) -> Result<Option<Value>> {
        self.inner.load(root)
    }
    fn store(&self, root: &Path, value: &Value) -> Result<()> {
        self.events
            .lock()
            .unwrap()
            .push(format!("ordinary:{}", value["request_state"]));
        self.inner.store(root, value)?;
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.bytes.fetch_add(
            std::fs::metadata(daemon_source_backed_refresh_job_path(root))?.len() as usize,
            Ordering::SeqCst,
        );
        Ok(())
    }
    fn store_before_ack(&self, root: &Path, value: &Value) -> DurableAdmissionPersistence {
        self.inner.store_with_parent_sync(root, value, |path| {
            // The real adapter has written/synced/replaced the real file before this boundary.
            assert_eq!(read_daemon_job_status_strict(path)?.as_ref(), Some(value));
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.bytes
                .fetch_add(std::fs::metadata(path)?.len() as usize, Ordering::SeqCst);
            self.events
                .lock()
                .unwrap()
                .push(format!("visible:{}", value["request_state"]));
            if self.fail_parent.swap(false, Ordering::SeqCst) {
                anyhow::bail!("injected parent sync failure after visible replacement");
            }
            sync_private_file_parent(path)?;
            self.events
                .lock()
                .unwrap()
                .push(format!("parent_synced:{}", value["request_state"]));
            Ok(())
        })
    }
}
struct IsolatedRuntime;
impl RefreshRuntime for IsolatedRuntime {
    fn metadata(&self, _: &Path, operation: RefreshOperation) -> RefreshRuntimeMetadata {
        RefreshRuntimeMetadata {
            operation,
            ..Default::default()
        }
    }
    fn discovery_context(&self, root: &Path) -> Result<ctx_history_capture::DiscoveryContext> {
        Ok(ctx_history_capture::DiscoveryContext::new(
            root.join("empty-provider-home"),
            root.join("empty-cwd"),
            ctx_history_capture::DiscoveryPlatform::Linux,
            ctx_history_capture::DiscoveryPlatformDirs::default(),
        ))
    }
}
fn fixture(
    journal: Arc<FaultJournal>,
    executions: Arc<AtomicUsize>,
    fail_capture: bool,
) -> RefreshEngine {
    RefreshEngine::with_executor(
        journal,
        Arc::new(IsolatedRuntime),
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            executions.fetch_add(1, Ordering::SeqCst);
            if fail_capture {
                anyhow::bail!("bounded provider failure");
            }
            crate::source_backed_refresh_coordinator::publish_authoritative_empty_generation_for_test(
            execution.index_root, execution.request_id, execution.operation,
            execution.admitted_refresh().publication_scope().clone(), execution.explicit_source_catalog.cloned())
        }),
    )
}

#[test]
fn terminal_durability_visible_failure_stays_pending_and_retry_does_not_recapture() -> Result<()> {
    for fail_capture in [false, true] {
        let root = isolated_data_root()?;
        ctx_history_platform::platform_security::establish_private_data_root(root.path())?;
        let journal = Arc::new(FaultJournal::default());
        let executions = Arc::new(AtomicUsize::new(0));
        let engine = fixture(journal.clone(), executions.clone(), fail_capture);
        let request = engine.enqueue_for_test(None);
        let id = request["request_id"].as_str().unwrap();
        let successor = engine.enqueue_manual_all_demand_for_test(
            root.path(),
            None,
            uuid::Uuid::now_v7().to_string(),
        )?;
        let successor_id = successor["request_id"].as_str().unwrap();
        journal.fail_parent.store(true, Ordering::SeqCst);
        let run = engine
            .run_next_with_coverage_fence_for_test(root.path(), |_, _| Ok(Default::default()))
            .expect("real owner runs");
        assert!(
            run.terminal_persistence_pending,
            "unconfirmed terminal must remain pending"
        );
        assert!(!run.did_work);
        assert_eq!(
            run.failed, fail_capture,
            "unexpected execution outcome: {}",
            run.job
        );
        assert_eq!(
            engine.status(successor_id).unwrap()["request_state"],
            "admission_pending"
        );
        let expected = if fail_capture { "failed" } else { "published" };
        assert_eq!(
            journal.load(root.path())?.unwrap()["request_state"],
            expected
        );
        let status = engine.status(id).unwrap();
        assert_eq!(status["request_state"], "running");
        assert_eq!(status["progress"]["phase"], "persisting_terminal");
        assert!(status.schema_v1_fields().get("receipt").is_none());
        let wire_status = super::super::wire::handle_ipc_request_for_test(
            &engine,
            root.path(),
            &json!({"op":"source_refresh_status", "request_id":id}),
        )?
        .unwrap();
        assert_eq!(wire_status["request_state"], "running");
        for field in ["receipt", "result", "finished_at", "structured_outcome"] {
            assert!(wire_status.get(field).is_none(), "{wire_status}");
        }
        assert!(engine.has_pending_request());
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        // Every later ordinary owner write must preserve the pending image and its flush boundary.
        journal.fail_parent.store(true, Ordering::SeqCst);
        assert!(engine
            .persist_scheduler_status(root.path(), json!({"status":"retrying"}))
            .is_err());
        assert_eq!(engine.status(id).unwrap()["request_state"], "running");
        let retry = engine
            .run_next_with_coverage_fence_for_test(root.path(), |_, _| Ok(Default::default()))
            .expect("terminal retry");
        assert!(!retry.terminal_persistence_pending);
        assert_eq!(engine.status(id).unwrap()["request_state"], expected);
        let wire_status = super::super::wire::handle_ipc_request_for_test(
            &engine,
            root.path(),
            &json!({"op":"source_refresh_status", "request_id":id}),
        )?
        .unwrap();
        assert_eq!(wire_status["request_state"], expected);
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(
            engine.status(successor_id).unwrap()["request_state"],
            "admission_pending"
        );
        let events = journal.events.lock().unwrap().clone();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.starts_with("parent_synced:"))
                .count(),
            1
        );
        assert_eq!(
            events.iter().filter(|e| e.starts_with("visible:")).count(),
            3
        );
        eprintln!("terminal_durability fail_capture={fail_capture} captures=1 writes={} serialized_file_bytes={} events={events:?}", journal.writes.load(Ordering::SeqCst), journal.bytes.load(Ordering::SeqCst));
    }
    Ok(())
}

#[test]
fn terminal_durability_every_overlay_flushes_but_running_status_does_not() -> Result<()> {
    let root = isolated_data_root()?;
    ctx_history_platform::platform_security::establish_private_data_root(root.path())?;
    let journal = Arc::new(FaultJournal::default());
    let engine = fixture(journal.clone(), Arc::new(AtomicUsize::new(0)), false);
    let request = engine.enqueue_for_test(None);
    let id = request["request_id"].as_str().unwrap();
    engine.persist_job_status_for_test(root.path(), id)?;
    assert!(journal
        .events
        .lock()
        .unwrap()
        .iter()
        .all(|e| e.starts_with("ordinary:")));
    let terminal = engine
        .run_next_with_coverage_fence_for_test(root.path(), |_, _| Ok(Default::default()))
        .unwrap();
    assert!(!terminal.terminal_persistence_pending);
    journal.events.lock().unwrap().clear();
    engine.persist_job_status_for_test(root.path(), id)?;
    engine.persist_retry_status(root.path(), terminal.job.clone())?;
    engine.persist_scheduler_status(root.path(), terminal.job.clone())?;
    let events = journal.events.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            "visible:\"published\"",
            "parent_synced:\"published\"",
            "visible:\"published\"",
            "parent_synced:\"published\"",
            "visible:\"published\"",
            "parent_synced:\"published\""
        ]
    );
    eprintln!("terminal_durability overlay_writes=3 added_parent_flushes=3 lifecycle_writes={} serialized_file_bytes={} events={events:?}", journal.writes.load(Ordering::SeqCst), journal.bytes.load(Ordering::SeqCst));
    Ok(())
}

#[test]
fn cold_daemon_root_established_before_journal_keeps_durable_terminal() -> Result<()> {
    let temp = isolated_data_root()?;
    let root = temp.path().join("new-parent/data");
    let owner = crate::paths_status::DaemonLock::acquire(&root)?.expect("isolated daemon lock");
    assert!(root.is_dir());
    let journal = DaemonRefreshJournal::default();
    let terminal = json!({"request_state":"published", "request_id":"cold-root"});
    assert!(matches!(
        journal.store_before_ack(&root, &terminal),
        DurableAdmissionPersistence::Confirmed
    ));
    drop(owner);
    assert_eq!(DaemonRefreshJournal::default().load(&root)?, Some(terminal));
    for directory in [&root, &root.join("daemon"), &root.join("daemon/jobs")] {
        ctx_history_platform::platform_security::verify_private_directory(directory)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_durability_syscall_child() -> Result<()> {
    let Ok(case) = std::env::var("CTX_STORAGE_SYSCALL_CASE") else {
        return Ok(());
    };
    let root = std::path::PathBuf::from(std::env::var_os("CTX_STORAGE_SYSCALL_ROOT").unwrap());
    let value = json!({"request_state":"published", "request_id":"syscall-new"});
    let journal = DaemonRefreshJournal::default();
    // Exercise one live owner: initialization is repeated only after failure.
    if case == "init_sync" {
        assert!(matches!(
            journal.store_before_ack(&root, &value),
            DurableAdmissionPersistence::Failed(_)
        ));
        assert!(journal.initialized_root.lock().unwrap().is_none());
        assert!(matches!(
            journal.store_before_ack(&root, &value),
            DurableAdmissionPersistence::Confirmed
        ));
        return Ok(());
    }
    journal.initialize(&root)?;
    if case == "running" {
        eprintln!("CTX_WARM_WRITE_BEGIN");
        journal.store(
            &root,
            &json!({"request_state":"running", "request_id":"syscall-new"}),
        )?;
        eprintln!("CTX_WARM_WRITE_END");
        return Ok(());
    }
    let result = journal.store_before_ack(&root, &value);
    match case.as_str() {
        "normal" => assert!(matches!(result, DurableAdmissionPersistence::Confirmed)),
        "file_sync" | "rename" => assert!(matches!(result, DurableAdmissionPersistence::Failed(_))),
        "parent_sync" => assert!(matches!(result, DurableAdmissionPersistence::Retained(_))),
        _ => panic!("unknown bounded syscall case"),
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_durability_real_adapter_syscall_order_and_failures() -> Result<()> {
    let child = "source_backed_refresh_adapter::journal::terminal_durability_tests::terminal_durability_syscall_child";
    for (case, fault) in [
        ("normal", None),
        ("running", None),
        ("init_sync", Some("fsync:error=EIO:when=2")),
        ("file_sync", Some("fsync:error=EIO:when=7")),
        ("rename", Some("rename:error=EIO:when=1")),
        ("parent_sync", Some("fsync:error=EIO:when=8")),
    ] {
        let root = isolated_data_root()?;
        let original = json!({"request_state":"published", "request_id":"syscall-old"});
        assert!(matches!(
            DaemonRefreshJournal::default().store_before_ack(root.path(), &original),
            DurableAdmissionPersistence::Confirmed
        ));
        let trace_path = root.path().join("trace.log");
        let mut command = std::process::Command::new("/usr/bin/strace");
        command
            .args([
                "-f",
                "-yy",
                "-e",
                "trace=fsync,rename,openat,newfstatat,statx,fchmod,chmod,close,write",
                "-o",
            ])
            .arg(&trace_path);
        if let Some(fault) = fault {
            command.arg(format!("--inject={fault}"));
        }
        let output = command
            .arg(std::env::current_exe()?)
            .args(["--exact", child, "--nocapture", "--test-threads=1"])
            .env("CTX_STORAGE_SYSCALL_CASE", case)
            .env("CTX_STORAGE_SYSCALL_ROOT", root.path())
            .output()?;
        assert!(
            output.status.success(),
            "{case}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let trace = std::fs::read_to_string(trace_path)?;
        if case == "running" {
            let warm = trace
                .split("CTX_WARM_WRITE_BEGIN")
                .nth(1)
                .unwrap()
                .split("CTX_WARM_WRITE_END")
                .next()
                .unwrap();
            let counts = [
                "openat",
                "newfstatat",
                "statx",
                "fchmod",
                "chmod",
                "close",
                "fsync",
                "rename",
            ]
            .map(|call| {
                (
                    call,
                    warm.lines()
                        .filter(|line| line.contains(&format!(" {call}(")))
                        .count(),
                )
            });
            assert_eq!(
                counts.iter().find(|(call, _)| *call == "fsync").unwrap().1,
                1,
                "{warm}"
            );
            assert_eq!(
                counts.iter().find(|(call, _)| *call == "rename").unwrap().1,
                1,
                "{warm}"
            );
            eprintln!(
                "terminal_durability warm_path={} syscall_counts={counts:?} trace={warm:?}",
                root.path().display()
            );
        }
        let calls = trace
            .lines()
            .filter(|line| line.contains("fsync(") || line.contains("rename("))
            .collect::<Vec<_>>();
        let initialization_calls = if case == "init_sync" { 8 } else { 6 };
        assert_eq!(
            calls.len(),
            initialization_calls
                + match case {
                    "file_sync" => 1,
                    "rename" | "running" => 2,
                    _ => 3,
                },
            "{trace}"
        );
        let replacements = &calls[initialization_calls..];
        assert!(
            replacements[0].contains("fsync(") && replacements[0].contains(".tmp>"),
            "{trace}"
        );
        if case != "file_sync" {
            assert!(replacements[1].contains("rename("), "{trace}");
        }
        if replacements.len() == 3 {
            assert!(
                replacements[2].contains("fsync(") && replacements[2].contains("/daemon/jobs>"),
                "{trace}"
            );
        }
        // No root walk: initializer visits only the selected root's containing
        // link and its daemon/jobs chain. Running replacement adds no dir sync.
        assert!(
            calls[..initialization_calls]
                .iter()
                .all(|line| !line.contains("</>")),
            "{trace}"
        );
        let expected_id = if matches!(case, "file_sync" | "rename") {
            "syscall-old"
        } else {
            "syscall-new"
        };
        assert_eq!(
            DaemonRefreshJournal::default().load(root.path())?.unwrap()["request_id"],
            expected_id
        );
        let path = daemon_source_backed_refresh_job_path(root.path());
        assert!(
            std::fs::read_dir(path.parent().unwrap())?.all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
        eprintln!("terminal_durability syscall_case={case} calls={} final_id={expected_id} trace={calls:?}", calls.len());
    }
    Ok(())
}
