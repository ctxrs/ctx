use super::*;
use ctx_history_source_io::read_metrics::track_source_reads;
use std::io::Write;

const OWNER: &str = "019fb000-0000-7000-8000-000000000091";
const PEER: &str = "019fb000-0000-7000-8000-000000000092";
const RESTART_ROOT: &str = "CTX_TEST_MAINTENANCE_IO_ROOT";

// Authored I/O fixtures; native provider-shape coverage lives with the adapters.
#[test]
fn ordinary_maintenance_reads_changed_data_including_after_restart() {
    if let Some(root) = std::env::var_os(RESTART_ROOT) {
        measure_refresh(Path::new(&root), "restart", true, true);
        return;
    }
    for (scenario, append, create) in [
        ("append", true, false),
        ("new", false, true),
        ("both", true, true),
        ("restart", true, true),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let sessions = root.join("sessions");
        fs::create_dir(&sessions).unwrap();
        let path = session_path(&sessions, OWNER);
        let mut records = vec![session_meta(OWNER), message("oldmarker")];
        records.extend(std::iter::repeat_n(
            json!({"type":"turn_context", "payload":{"cwd":"/example", "padding":"x".repeat(128 * 1024)}}),
            30,
        ));
        fs::write(&path, jsonl(records)).unwrap();
        let (_, _, discovery) = discovery_fixture(root);
        let data_root = root.join("data");
        ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
        let admitted = admission(root, &discovery);
        refresh(root, &discovery, admitted);
        if append {
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(&jsonl([message("appendmarker")]))
                .unwrap();
        }
        if create {
            fs::write(
                session_path(&sessions, PEER),
                jsonl([session_meta(PEER), message("newmarker")]),
            )
            .unwrap();
        }
        if scenario == "restart" {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::execution_path::maintenance_io::ordinary_maintenance_reads_changed_data_including_after_restart",
                    "--nocapture",
                ])
                .env(RESTART_ROOT, root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        } else {
            measure_refresh(root, scenario, append, create);
        }
    }
}

fn measure_refresh(root: &Path, scenario: &str, append: bool, create: bool) {
    let sessions = root.join("sessions");
    let owner_path = session_path(&sessions, OWNER);
    let peer_path = session_path(&sessions, PEER);
    let owner_reads = track_source_reads(&owner_path);
    let peer_reads = create.then(|| track_source_reads(&peer_path));
    let (_, _, discovery) = discovery_fixture(root);
    let catalog = admission(root, &discovery)
        .discovery()
        .watch_catalog()
        .clone();
    let routes = catalog.route_ids().cloned().collect::<BTreeSet<_>>();
    assert_eq!(routes.len(), 1);
    let members = [
        append.then_some(owner_path.clone()),
        create.then_some(peer_path),
    ]
    .into_iter()
    .flatten()
    .collect::<BTreeSet<_>>();
    let worksets = BTreeMap::from([(
        routes.first().unwrap().clone(),
        SourceBackedRefreshWorkset::members(members),
    )]);
    let admitted =
        AdmittedRefresh::from_exact_catalog_authority(routes, StdDuration::ZERO, catalog)
            .unwrap()
            .with_execution_facts(worksets)
            .unwrap();
    let (publication, processed_sessions, processed_bytes) = refresh(root, &discovery, admitted);
    let old_bytes = owner_reads.bytes();
    let new_bytes = peer_reads.as_ref().map_or(0, |reads| reads.bytes());
    eprintln!("{scenario}: existing_source_bytes={old_bytes} new_source_bytes={new_bytes} processed_sessions={processed_sessions} processed_bytes={processed_bytes}");
    assert_eq!(
        publication.certified_source_count,
        if create { 2 } else { 1 }
    );
    let index = VerifiedIndex::open_pinned(source_backed_index_root(&root.join("data"))).unwrap();
    assert_eq!(
        index.complete_lexical_search("oldmarker", 8).unwrap().len(),
        1
    );
    assert_eq!(
        index
            .complete_lexical_search("appendmarker", 8)
            .unwrap()
            .len(),
        usize::from(append)
    );
    assert_eq!(
        index.complete_lexical_search("newmarker", 8).unwrap().len(),
        usize::from(create)
    );
    // Bounded first-record probes remain necessary on route-wide discovery;
    // they must not turn into reads of the multi-megabyte retained prefix.
    assert!(
        old_bytes < 16 * 1024,
        "{scenario}: existing source read {old_bytes} bytes"
    );
    assert!(
        new_bytes < 16 * 1024,
        "{scenario}: new source read {new_bytes} bytes"
    );
    if create {
        assert!(new_bytes >= fs::metadata(session_path(&sessions, PEER)).unwrap().len());
    }
    if !create {
        // One changed session can report the whole certified prefix while
        // actual source I/O is only a few hundred suffix bytes.
        assert_eq!(processed_sessions, 1);
        assert!(old_bytes >= jsonl([message("appendmarker")]).len() as u64);
        assert!(processed_bytes >= fs::metadata(owner_path).unwrap().len());
    }
}

fn admission(root: &Path, discovery: &DiscoveryContext) -> AdmittedRefresh {
    source_backed_admitted_discovery_from_report(
        discovery,
        DiscoveryReport {
            sources: vec![provider_source_for_path(
                CaptureProvider::Codex,
                root.join("sessions"),
            )],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &root.join("data"),
        AdmittedRefreshCoverage::CompleteCatalog,
        None,
        &TestPublishedState,
    )
    .unwrap()
}

fn refresh(
    root: &Path,
    discovery: &DiscoveryContext,
    admitted: AdmittedRefresh,
) -> (SourceBackedRefreshPublication, u64, u64) {
    let data_root = root.join("data");
    let index_root = source_backed_index_root(&data_root);
    let progress = std::sync::Mutex::new((0, 0));
    let publication = super::super::super::execution::execute_capture_owned_refresh(
        SourceBackedRefreshExecution::new(
            &data_root,
            &index_root,
            "maintenance-io",
            RefreshOperation::Refresh,
            None,
            admitted,
            discovery,
            &TestPublishedState,
            &|update| {
                *progress.lock().unwrap() = (update.processed_sessions, update.processed_bytes);
                Ok(())
            },
        ),
    )
    .unwrap();
    let (sessions, bytes) = progress.into_inner().unwrap();
    (publication, sessions, bytes)
}

fn session_path(root: &Path, owner: &str) -> PathBuf {
    root.join(format!("rollout-{owner}.jsonl"))
}

fn jsonl(records: impl IntoIterator<Item = Value>) -> Vec<u8> {
    records
        .into_iter()
        .flat_map(|record| {
            let mut bytes = serde_json::to_vec(&record).unwrap();
            bytes.push(b'\n');
            bytes
        })
        .collect()
}

fn session_meta(owner: &str) -> Value {
    json!({"type":"session_meta", "payload":{"id":owner, "timestamp":"2026-08-17T00:00:00Z", "cwd":"/example", "originator":"codex_cli_rs", "cli_version":"1.0.0", "source":"cli", "model_provider":"openai"}})
}

fn message(text: &str) -> Value {
    json!({"timestamp":"2026-08-17T00:00:01Z", "type":"response_item", "payload":{"type":"message", "role":"user", "content":[{"type":"input_text", "text":text}]}})
}
