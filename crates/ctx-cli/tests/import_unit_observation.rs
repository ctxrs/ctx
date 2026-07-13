mod support;

use std::collections::BTreeMap;

use support::*;

struct CompanionCase {
    provider: &'static str,
    expected_first_events: u64,
    expected_changed_events: u64,
    expected_units: u64,
    fixture: fn(&TempDir, &str) -> String,
    companion: fn(&Path) -> PathBuf,
}

#[test]
fn companion_only_changes_select_each_logical_import_unit_once() {
    let cases = [
        CompanionCase {
            provider: "mistral-vibe",
            expected_first_events: 4,
            expected_changed_events: 4,
            expected_units: 1,
            fixture: write_native_mistral_vibe_fixture,
            companion: |root| find_source_file(root, "meta.json"),
        },
        CompanionCase {
            provider: "mux",
            expected_first_events: 6,
            expected_changed_events: 4,
            expected_units: 2,
            fixture: write_native_mux_fixture,
            companion: |root| root.join("mux-cli-native/metadata.json"),
        },
        CompanionCase {
            provider: "junie",
            expected_first_events: 2,
            expected_changed_events: 2,
            expected_units: 1,
            fixture: write_native_junie_fixture,
            companion: |root| root.join("index.jsonl"),
        },
        CompanionCase {
            provider: "kimi-code-cli",
            expected_first_events: 7,
            expected_changed_events: 7,
            expected_units: 2,
            fixture: write_native_kimi_fixture,
            companion: |root| find_source_file(root, "state.json"),
        },
        CompanionCase {
            provider: "rovodev",
            expected_first_events: 3,
            expected_changed_events: 3,
            expected_units: 1,
            fixture: write_native_rovodev_fixture,
            companion: |root| find_source_file(root, "metadata.json"),
        },
        CompanionCase {
            provider: "continue",
            expected_first_events: 2,
            expected_changed_events: 2,
            expected_units: 1,
            fixture: write_native_continue_fixture,
            companion: |root| root.join("sessions.json"),
        },
    ];

    for case in cases {
        let temp = tempdir();
        let root = PathBuf::from((case.fixture)(
            &temp,
            &format!("{} import-unit oracle", case.provider),
        ));

        let first = import_path(&temp, case.provider, &root);
        assert_eq!(
            first["totals"]["imported_events"].as_u64(),
            Some(case.expected_first_events),
            "{} imported a logical unit more than once: {first:#}",
            case.provider
        );
        let status = json_output(ctx(&temp).args(["status", "--json"]));
        assert_eq!(
            status["source_import_files"].as_u64(),
            Some(case.expected_units),
            "{} inventory did not model canonical logical units: {status:#}",
            case.provider
        );

        let companion = (case.companion)(&root);
        append_whitespace(&companion);
        let source_after_provider_change = snapshot_regular_files(&root);

        let changed = import_path(&temp, case.provider, &root);
        assert_eq!(
            processed_events(&changed),
            case.expected_changed_events,
            "{} companion-only change was missed or imported more than once: {changed:#}",
            case.provider
        );
        assert_eq!(
            snapshot_regular_files(&root),
            source_after_provider_change,
            "{} import mutated provider-owned files",
            case.provider
        );

        let unchanged = import_path(&temp, case.provider, &root);
        assert_eq!(
            processed_events(&unchanged),
            0,
            "{} unchanged logical unit was not a no-op: {unchanged:#}",
            case.provider
        );
    }
}

#[test]
fn kimi_root_index_change_selects_each_dependent_wire_unit() {
    let temp = tempdir();
    let root = PathBuf::from(write_native_kimi_fixture(
        &temp,
        "kimi root index import-unit oracle",
    ));
    let first = import_path(&temp, "kimi-code-cli", &root);
    assert_eq!(first["totals"]["imported_events"], 7, "{first:#}");

    append_whitespace(&root.join("session_index.jsonl"));
    let source_after_provider_change = snapshot_regular_files(&root);
    let changed = import_path(&temp, "kimi-code-cli", &root);

    assert_eq!(processed_events(&changed), 7, "{changed:#}");
    assert_eq!(
        snapshot_regular_files(&root),
        source_after_provider_change,
        "Kimi import mutated provider-owned files"
    );
    let unchanged = import_path(&temp, "kimi-code-cli", &root);
    assert_eq!(processed_events(&unchanged), 0, "{unchanged:#}");
}

#[test]
fn trae_workspace_companion_change_selects_one_database_unit() {
    let temp = tempdir();
    install_default_trae_fixture(&temp, "trae workspace companion oracle");
    let root = temp
        .path()
        .join("Library/Application Support/Trae/User/workspaceStorage");
    let first = import_provider(&temp, "trae");
    assert_eq!(first["totals"]["imported_events"], 2, "{first:#}");

    append_whitespace(&root.join("standard-workspace/workspace.json"));
    let source_after_provider_change = snapshot_regular_files(&root);
    let changed = import_provider(&temp, "trae");

    assert_eq!(processed_events(&changed), 2, "{changed:#}");
    assert_eq!(
        snapshot_regular_files(&root),
        source_after_provider_change,
        "Trae import mutated provider-owned files"
    );
    let unchanged = import_provider(&temp, "trae");
    assert_eq!(processed_events(&unchanged), 0, "{unchanged:#}");
}

#[test]
fn manifested_opencode_observes_wal_only_commits_read_only() {
    let temp = tempdir();
    let initial = "opencode manifested WAL initial oracle";
    let appended = "opencode manifested WAL appended oracle";
    let source = PathBuf::from(write_native_opencode_fixture(&temp, initial));
    let writer = wal_writer(&source);

    let first = import_path(&temp, "opencode", &source);
    assert!(first["totals"]["imported_events"].as_u64().unwrap() >= 1);
    let main_before = fs::metadata(&source).unwrap();
    writer
        .execute(
            "INSERT INTO message VALUES (?1, ?2, 1782259203000, 1782259203000, ?3)",
            params![
                "opencode-cli-native-wal-user",
                "opencode-cli-native",
                json!({
                    "role": "user",
                    "time": {"created": 1782259203000_i64},
                    "text": appended
                })
                .to_string()
            ],
        )
        .unwrap();
    assert_main_database_unchanged(&source, &main_before);
    let source_after_provider_change = snapshot_sqlite_files(&source);

    let changed = import_path(&temp, "opencode", &source);
    assert!(
        changed["totals"]["imported_events"].as_u64().unwrap() >= 1,
        "{changed:#}"
    );
    assert_eq!(
        snapshot_sqlite_files(&source),
        source_after_provider_change,
        "OpenCode import mutated provider-owned SQLite files"
    );
    assert_search(&temp, "opencode", appended);

    let unchanged = import_path(&temp, "opencode", &source);
    assert_eq!(processed_events(&unchanged), 0, "{unchanged:#}");
    drop(writer);
}

#[test]
fn manifested_trae_observes_wal_only_commits_read_only() {
    let temp = tempdir();
    let initial = "trae manifested WAL initial oracle";
    let appended = "trae manifested WAL appended oracle";
    install_default_trae_fixture(&temp, initial);
    let root = temp
        .path()
        .join("Library/Application Support/Trae/User/workspaceStorage");
    let source = root.join("standard-workspace/state.vscdb");
    let writer = wal_writer(&source);

    let first = import_provider(&temp, "trae");
    assert_eq!(first["totals"]["imported_events"], 2, "{first:#}");
    let main_before = fs::metadata(&source).unwrap();
    let value = json!({
        "list": [{
            "id": "standard-session",
            "title": "Standard Trae WAL fixture",
            "createdAt": "2026-07-05T14:00:00Z",
            "messages": [
                {
                    "id": "standard-user",
                    "role": "user",
                    "content": initial,
                    "createdAt": "2026-07-05T14:00:00Z"
                },
                {
                    "id": "standard-assistant",
                    "role": "assistant",
                    "content": format!("{initial} assistant reply"),
                    "createdAt": "2026-07-05T14:01:00Z"
                },
                {
                    "id": "standard-wal-user",
                    "role": "user",
                    "content": appended,
                    "createdAt": "2026-07-05T14:02:00Z"
                }
            ]
        }]
    });
    writer
        .execute(
            "UPDATE ItemTable SET value = ?1 WHERE [key] = 'memento/icube-ai-agent-storage'",
            [value.to_string()],
        )
        .unwrap();
    assert_main_database_unchanged(&source, &main_before);
    let source_after_provider_change = snapshot_sqlite_files(&source);
    let workspace_before = fs::read(root.join("standard-workspace/workspace.json")).unwrap();

    let changed = import_provider(&temp, "trae");
    assert_eq!(processed_events(&changed), 3, "{changed:#}");
    assert_eq!(changed["totals"]["imported_events"], 1, "{changed:#}");
    assert_eq!(
        snapshot_sqlite_files(&source),
        source_after_provider_change,
        "Trae import mutated provider-owned SQLite files"
    );
    assert_eq!(
        fs::read(root.join("standard-workspace/workspace.json")).unwrap(),
        workspace_before,
        "Trae import mutated its workspace companion"
    );
    assert_search(&temp, "trae", appended);

    let unchanged = import_provider(&temp, "trae");
    assert_eq!(processed_events(&unchanged), 0, "{unchanged:#}");
    drop(writer);
}

#[cfg(unix)]
#[test]
fn non_utf8_manifest_owners_are_rejected_without_lossy_aliases() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    let temp = tempdir();
    let root = temp.path().join("non-utf8-claude-projects");
    fs::create_dir(&root).unwrap();
    let owner_a = root.join(OsString::from_vec(b"session-\x80.jsonl".to_vec()));
    let owner_b = root.join(OsString::from_vec(b"session-\x81.jsonl".to_vec()));
    assert_ne!(owner_a, owner_b);
    assert_eq!(owner_a.display().to_string(), owner_b.display().to_string());
    fs::write(&owner_a, b"{}\n").unwrap();
    fs::write(&owner_b, b"{}\n").unwrap();

    let mut command = ctx(&temp);
    command
        .args(["import", "--provider", "claude", "--path"])
        .arg(&root)
        .args(["--json", "--progress", "none"]);
    let stderr = failure_stderr(&mut command);
    assert!(
        stderr.contains("import unit owner cannot be persisted because it is not valid UTF-8"),
        "{stderr}"
    );

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["source_import_files"], 0, "{status:#}");
}

fn import_path(temp: &TempDir, provider: &str, path: &Path) -> Value {
    json_output(ctx(temp).args([
        "import",
        "--provider",
        provider,
        "--path",
        path.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]))
}

fn import_provider(temp: &TempDir, provider: &str) -> Value {
    json_output(ctx(temp).args([
        "import",
        "--provider",
        provider,
        "--json",
        "--progress",
        "none",
    ]))
}

fn assert_search(temp: &TempDir, provider: &str, query: &str) {
    let search = json_output(ctx(temp).args([
        "search",
        query,
        "--provider",
        provider,
        "--refresh",
        "off",
        "--json",
    ]));
    assert_eq!(search["results"].as_array().unwrap().len(), 1, "{search:#}");
}

fn processed_events(report: &Value) -> u64 {
    report["totals"]["imported_events"].as_u64().unwrap()
        + report["totals"]["skipped_events"].as_u64().unwrap()
}

fn wal_writer(path: &Path) -> Connection {
    let writer = Connection::open(path).unwrap();
    let journal_mode: String = writer
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    writer
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .unwrap();
    writer
}

fn assert_main_database_unchanged(path: &Path, before: &fs::Metadata) {
    let after = fs::metadata(path).unwrap();
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    assert!(sqlite_sidecar(path, "-wal").is_file());
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn snapshot_sqlite_files(path: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    [
        path.to_path_buf(),
        sqlite_sidecar(path, "-wal"),
        sqlite_sidecar(path, "-shm"),
        sqlite_sidecar(path, "-journal"),
    ]
    .into_iter()
    .map(|file| (file.clone(), fs::read(file).ok()))
    .collect()
}

fn append_whitespace(path: &Path) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

fn find_source_file(root: &Path, name: &str) -> PathBuf {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|file| file.to_str()) == Some(name) {
                return path;
            }
        }
    }
    panic!("missing {name} under {}", root.display());
}

fn snapshot_regular_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut snapshot = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let file_type = entry.file_type().unwrap();
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                snapshot.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    snapshot
}
