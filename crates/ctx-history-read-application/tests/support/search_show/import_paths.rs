use super::*;

#[derive(Debug, Clone, Copy)]
enum MissingImportSurface {
    Provider,
    CustomJsonl,
    HistorySourceManifest,
}

impl MissingImportSurface {
    const ALL: [Self; 3] = [
        Self::Provider,
        Self::CustomJsonl,
        Self::HistorySourceManifest,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::CustomJsonl => "custom JSONL",
            Self::HistorySourceManifest => "history-source manifest",
        }
    }
}

fn missing_import_command(temp: &TempDir, surface: MissingImportSurface, path: &Path) -> Command {
    let mut command = ctx(temp);
    command.arg("import");
    match surface {
        MissingImportSurface::Provider => {
            command.args(["--provider", "codex", "--path"]);
        }
        MissingImportSurface::CustomJsonl => {
            command.args(["--input-format", "ctx-history-jsonl-v2", "--path"]);
        }
        MissingImportSurface::HistorySourceManifest => {
            command.arg("--history-source-manifest");
        }
    }
    command.arg(path);
    command
}

const LEAKED_IMPORT_PATH_DETAILS: &[&str] = &[
    "approve explicit source path",
    "check explicit source path",
    "check import path",
    "No such file or directory",
    "The system cannot find the file specified",
    "(os error",
    "ImportPathNotFound",
    "Caused by:",
    "Stack backtrace:",
];

fn assert_no_leaked_import_path_details(rendered: &str, contract: &str) {
    for &leaked_detail in LEAKED_IMPORT_PATH_DETAILS {
        assert!(
            !rendered.contains(leaked_detail),
            "{contract} leaked `{leaked_detail}`:\n{rendered}"
        );
    }
}

fn assert_clean_missing_import_path(stderr: &str, path: &str) {
    let summary_line = stderr
        .lines()
        .find(|line| line.contains("Import path does not exist"))
        .unwrap_or_else(|| panic!("missing import-path summary in:\n{stderr}"));
    assert!(!summary_line.contains(path), "{stderr}");
    assert_eq!(
        stderr.matches("Import path does not exist").count(),
        1,
        "duplicate summary in:\n{stderr}"
    );
    assert_eq!(
        stderr.matches(path).count(),
        1,
        "path was changed, split, or duplicated in:\n{stderr}"
    );
    assert!(
        stderr
            .lines()
            .any(|line| line.contains("Path") && line.contains(path))
            || stderr
                .lines()
                .collect::<Vec<_>>()
                .windows(2)
                .any(|lines| lines[0].trim() == "Path" && lines[1].contains(path)),
        "missing separate Path field in:\n{stderr}"
    );
    assert_no_leaked_import_path_details(stderr, "human missing-path diagnostic");
}

fn assert_clean_missing_import_path_plain(stderr: &[u8], path: &str, contract: &str) {
    let stderr = std::str::from_utf8(stderr)
        .unwrap_or_else(|error| panic!("{contract} emitted non-UTF-8 stderr: {error}"));
    let expected = format!("Import path does not exist: {path}\n");
    assert_eq!(stderr, expected, "{contract}");
    assert!(
        !stderr
            .chars()
            .filter(|&character| character != '\n')
            .any(|character| character <= '\u{001f}'
                || ('\u{007f}'..='\u{009f}').contains(&character)),
        "{contract} emitted a raw control: {stderr:?}"
    );
    assert_no_leaked_import_path_details(stderr, contract);
}

fn assert_clean_missing_import_path_progress(
    output: &std::process::Output,
    path: &str,
    contract: &str,
) {
    assert!(output.stdout.is_empty(), "{contract}: {output:#?}");
    let stderr = std::str::from_utf8(&output.stderr)
        .unwrap_or_else(|error| panic!("{contract} emitted non-UTF-8 stderr: {error}"));
    let event: Value = serde_json::from_str(stderr)
        .unwrap_or_else(|error| panic!("{contract} emitted invalid JSON ({error}): {stderr:?}"));

    assert_eq!(event["type"], "ctx_progress", "{contract}");
    assert_eq!(event["operation"], "import", "{contract}");
    assert_eq!(event["phase"], "failed", "{contract}");
    assert_eq!(
        event["message"],
        format!("Import path does not exist: {path}"),
        "{contract}"
    );
    assert_eq!(event["done"], true, "{contract}");
    assert_no_leaked_import_path_details(stderr, contract);
}

#[test]
fn pi_cli_imports_directory_tree_path() {
    let temp = tempdir();
    let path = temp.path().join("pi-sessions-dir");
    let project = path.join("--workspace--");
    fs::create_dir_all(&project).unwrap();
    write_pi_session_jsonl(
        &project.join("2026-06-24T12-00-00-000Z_pi-dir-alpha.jsonl"),
        "pi-dir-alpha",
        "pi directory alpha oracle",
    );
    write_pi_session_jsonl(
        &project.join("2026-06-24T12-01-00-000Z_pi-dir-beta.jsonl"),
        "pi-dir-beta",
        "ctxpibetauniquetoken",
    );

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        path.to_str().unwrap(),
        "--format=json",
    ]));
    assert_explicit_source_publication(&imported, "pi", "pi_session_jsonl");
    assert_eq!(provider_core_counts(&data_root(&temp), "pi"), (2, 2));

    let search = json_output(ctx(&temp).args([
        "search",
        "ctxpibetauniquetoken",
        "--provider",
        "pi",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "pi", "ctxpibetauniquetoken", 1, "message");
    assert!(search["results"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("ctxpibetauniquetoken"));
}

#[test]
fn pi_cli_discovers_env_session_dir_for_sources_and_search_refresh() {
    let temp = tempdir();
    let path = temp.path().join("pi-env-sessions");
    let project = path.join("--workspace--");
    fs::create_dir_all(&project).unwrap();
    let _daemon =
        start_source_refresh_daemon_with_env(&temp, &[("PI_CODING_AGENT_SESSION_DIR", &path)]);
    write_pi_session_jsonl(
        &project.join("2026-06-24T12-00-00-000Z_pi-env-refresh.jsonl"),
        "pi-env-refresh",
        "pi env refresh oracle",
    );

    let sources = json_output(
        ctx(&temp)
            .env("PI_CODING_AGENT_SESSION_DIR", &path)
            .args(["sources", "--format=json"]),
    );
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "pi"
                && source["source_format"] == "pi_session_jsonl"
                && source["path"] == path.to_str().unwrap()
        })
        .unwrap_or_else(|| panic!("missing env Pi source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert_eq!(source["native_import"], true);
    assert_eq!(source["importable"], true);

    let search = json_output(ctx(&temp).env("PI_CODING_AGENT_SESSION_DIR", &path).args([
        "search",
        "pi env refresh oracle",
        "--provider",
        "pi",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "pi", "pi env refresh oracle", 1, "message");
}

#[test]
fn pi_cli_rejects_wrong_file_import_path() {
    let temp = tempdir();
    let path = temp.path().join("pi-session.txt");
    fs::write(&path, "{}\n").unwrap();

    ctx(&temp)
        .args([
            "import",
            "--provider",
            "pi",
            "--path",
            path.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Pi explicit JSONL file has no valid session header")
                .and(predicate::str::contains(path.to_str().unwrap())),
        );
}

#[test]
fn missing_import_paths_have_one_clean_human_contract_for_every_input_surface() {
    let temp = tempdir();
    for surface in MissingImportSurface::ALL {
        let path = temp
            .path()
            .join(format!("missing-{}-路径", surface.name().replace(' ', "-")));
        let output = missing_import_command(&temp, surface, &path)
            .arg("--color=never")
            .assert()
            .failure()
            .get_output()
            .clone();
        let stderr = String::from_utf8(output.stderr.clone()).unwrap();

        assert!(output.stdout.is_empty(), "{}: {output:#?}", surface.name());
        assert_clean_missing_import_path(&stderr, path.to_str().unwrap());
    }
}

#[test]
fn missing_import_paths_have_one_clean_diagnostic_in_result_json_mode() {
    let temp = tempdir();
    for surface in MissingImportSurface::ALL {
        let path = temp.path().join(format!(
            "missing-{}-format-json-路径",
            surface.name().replace(' ', "-")
        ));
        let output = missing_import_command(&temp, surface, &path)
            .args(["--color=always", "--format=json", "--progress=none"])
            .assert()
            .failure()
            .get_output()
            .clone();
        let contract = format!("{} --format=json", surface.name());

        assert!(output.stdout.is_empty(), "{contract}: {output:#?}");
        assert_clean_missing_import_path_plain(&output.stderr, path.to_str().unwrap(), &contract);
    }
}

#[test]
fn missing_import_paths_have_one_terminal_event_in_progress_json_mode() {
    let temp = tempdir();
    for surface in MissingImportSurface::ALL {
        let path = temp.path().join(format!(
            "missing-{}-progress-json-路径",
            surface.name().replace(' ', "-")
        ));
        let output = missing_import_command(&temp, surface, &path)
            .args(["--color=always", "--progress=json"])
            .assert()
            .failure()
            .get_output()
            .clone();
        let contract = format!("{} --progress=json", surface.name());

        assert_clean_missing_import_path_progress(&output, path.to_str().unwrap(), &contract);
    }
}

#[cfg(unix)]
#[test]
fn one_control_bearing_missing_path_is_visible_and_safe_in_result_json_mode() {
    let temp = tempdir();
    let path = temp
        .path()
        .join("  missing  provider  路径\n\r\t\u{0001}\u{001b}\u{007f}\u{0085}\u{009f}  ");
    let output = missing_import_command(&temp, MissingImportSurface::Provider, &path)
        .args(["--color=always", "--format=json", "--progress=none"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let contract = "control-bearing provider --format=json";

    assert!(output.stdout.is_empty(), "{contract}: {output:#?}");
    let safe_path = format!(
        "os:\"{}/  missing  provider  路径\\n\\r\\t\\u{{0001}}\\x1b\\u{{007f}}\\u{{0085}}\\u{{009f}}  \"",
        temp.path().display()
    );
    assert_clean_missing_import_path_plain(&output.stderr, &safe_path, contract);
    let wire = std::str::from_utf8(&output.stderr).unwrap();
    assert!(
        wire.contains("\\n\\r\\t\\u{0001}\\x1b\\u{007f}\\u{0085}\\u{009f}"),
        "{contract}: {wire:?}"
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_missing_import_path_is_preserved_in_final_binary_progress_output() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt as _};

    let temp = tempdir();
    let path = temp
        .path()
        .join(OsString::from_vec(b"missing-\xFF-provider-path".to_vec()));
    let output = missing_import_command(&temp, MissingImportSurface::Provider, &path)
        .args(["--color=always", "--progress=json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let expected_path = format!(
        "os:\"{}/missing-\\xFF-provider-path\"",
        temp.path().display()
    );
    let contract = "non-UTF-8 provider --progress=json";

    assert_clean_missing_import_path_progress(&output, &expected_path, contract);
}

#[test]
fn import_path_requires_provider_before_initializing_source_epoch() {
    let temp = tempdir();
    let path = temp.path().join("missing-codex-history");
    let path = path.to_str().unwrap();

    ctx(&temp)
        .args(["import", "--path", path])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "ctx import --path requires --provider",
        ));
    assert!(
        !data_root(&temp).join("search").exists(),
        "native path import without provider should not initialize lexical state"
    );
    assert!(
        !data_root(&temp).join("relational.sqlite").exists(),
        "native path import without provider should not create removed relational storage"
    );
    assert!(
        !data_root(&temp).join("catalogs").exists(),
        "native path import without provider should not initialize source catalogs"
    );
}

#[cfg(unix)]
#[test]
fn import_rejects_symlinked_provider_root() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let target = temp.path().join("pi-sessions");
    fs::create_dir_all(&target).unwrap();
    let path = temp.path().join("pi-sessions-link");
    symlink(&target, &path).unwrap();

    ctx(&temp)
        .args([
            "import",
            "--provider",
            "pi",
            "--path",
            path.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("symlinked explicit provider source roots are rejected")
                .and(predicate::str::contains(path.to_str().unwrap())),
        );
}

#[cfg(unix)]
#[test]
fn import_rejects_dangling_symlink_as_unsafe_provider_root() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let missing_target = temp.path().join("missing-pi-sessions");
    let path = temp.path().join("dangling-pi-sessions-link");
    symlink(&missing_target, &path).unwrap();

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        path.to_str().unwrap(),
    ]));
    let expected = format!(
        "symlinked explicit provider source roots are rejected: {}",
        path.display()
    );
    assert!(
        stderr.contains(&expected),
        "dangling symlink lost its unsafe-symlink classification:\n{stderr}"
    );
    assert!(
        !stderr.contains("Import path does not exist") && !stderr.contains("import_path_not_found"),
        "dangling symlink was misclassified as a missing path:\n{stderr}"
    );
}

#[cfg(target_os = "windows")]
fn windows_symlink_unavailable(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314)
}

#[cfg(target_os = "windows")]
fn assert_windows_explicit_provider_symlink_is_unsafe(temp: &TempDir, path: &Path) {
    let stderr = failure_stderr(ctx(temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        path.to_str().unwrap(),
    ]));

    assert!(
        stderr.contains("symlinked explicit provider source roots are rejected"),
        "Windows symlink lost its unsafe classification:\n{stderr}"
    );
    assert!(
        !stderr.contains("Import path does not exist") && !stderr.contains("import_path_not_found"),
        "Windows symlink was misclassified as a missing path:\n{stderr}"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn import_rejects_live_windows_directory_symlink_as_unsafe_provider_root() {
    use std::os::windows::fs::symlink_dir;

    let temp = tempdir();
    let target = temp.path().join("pi-sessions");
    let path = temp.path().join("pi-sessions-link");
    fs::create_dir_all(&target).unwrap();
    if let Err(error) = symlink_dir(&target, &path) {
        if windows_symlink_unavailable(&error) {
            return;
        }
        panic!("create live Windows directory symlink: {error}");
    }

    assert_windows_explicit_provider_symlink_is_unsafe(&temp, &path);
}

#[cfg(target_os = "windows")]
#[test]
fn import_rejects_dangling_windows_directory_symlink_as_unsafe_provider_root() {
    use std::os::windows::fs::symlink_dir;

    let temp = tempdir();
    let target = temp.path().join("missing-pi-sessions");
    let path = temp.path().join("dangling-pi-sessions-link");
    if let Err(error) = symlink_dir(&target, &path) {
        if windows_symlink_unavailable(&error) {
            return;
        }
        panic!("create dangling Windows directory symlink: {error}");
    }

    assert_windows_explicit_provider_symlink_is_unsafe(&temp, &path);
}

#[cfg(unix)]
#[test]
fn import_rejects_unreadable_directory_with_path_context() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir();
    let path = temp.path().join("unreadable-pi-sessions");
    let project = path.join("--workspace--");
    fs::create_dir_all(&project).unwrap();
    write_pi_session_jsonl(
        &project.join("2026-06-24T12-00-00-000Z_unreadable.jsonl"),
        "pi-unreadable",
        "pi unreadable oracle",
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        path.to_str().unwrap(),
    ]));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

    assert!(stderr.contains("is not importable"), "{stderr}");
    assert!(
        stderr.contains("provider path or format is not supported"),
        "{stderr}"
    );
    assert!(stderr.contains(path.to_str().unwrap()), "{stderr}");
}

#[test]
fn codex_cli_search_and_show_survive_deleted_raw_source() {
    let temp = tempdir();
    let source = PathBuf::from(provider_history_fixture("codex-sessions"));
    let copied = temp.path().join("copied-codex-sessions");
    copy_dir_all(&source, &copied);
    let copied_text = copied.to_str().unwrap().to_owned();

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &copied_text,
        "--format=json",
    ]));
    assert_explicit_source_publication(&imported, "codex", "codex_session_jsonl_tree");

    fs::remove_dir_all(&copied).unwrap();

    let search = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "codex", "onboarding", 1, "message");

    let result = &search["results"][0];
    let event_id = result["ctx_event_id"].as_str().unwrap();
    let session_id = result["ctx_session_id"].as_str().unwrap();
    let shown_event =
        json_output(ctx(&temp).args(["show", "event", event_id, "--window", "1", "--format=json"]));
    assert_eq!(
        shown_event["payload_type"], "event_window",
        "{shown_event:#}"
    );
    assert_eq!(shown_event["ctx_event_id"], event_id, "{shown_event:#}");
    assert_eq!(shown_event["ctx_session_id"], session_id, "{shown_event:#}");
    assert_eq!(shown_event["event"]["provider"], "codex", "{shown_event:#}");
    assert!(
        shown_event["event"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("onboarding")),
        "{shown_event:#}"
    );

    let shown_session =
        json_output(ctx(&temp).args(["show", "session", session_id, "--format=json"]));
    assert_eq!(
        shown_session["payload_type"], "session_transcript",
        "{shown_session:#}"
    );
    assert_eq!(
        shown_session["ctx_session_id"], session_id,
        "{shown_session:#}"
    );
    assert!(
        shown_session["events"]
            .as_array()
            .is_some_and(|events| events.iter().any(|event| event["text"]
                .as_str()
                .is_some_and(|text| text.contains("onboarding")))),
        "{shown_session:#}"
    );
}
