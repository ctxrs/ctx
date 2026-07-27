use super::*;

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
        "pi directory beta oracle",
    );

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        path.to_str().unwrap(),
        "--json",
    ]));
    assert_eq!(imported["totals"]["imported_sessions"], 2);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args([
        "search",
        "pi directory beta oracle",
        "--provider",
        "pi",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "pi", "pi directory beta oracle", 1, "message");
    assert!(search["results"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("pi directory beta oracle"));
}

#[test]
fn pi_cli_discovers_env_session_dir_for_sources_and_search_refresh() {
    let temp = tempdir();
    let path = temp.path().join("pi-env-sessions");
    let project = path.join("--workspace--");
    fs::create_dir_all(&project).unwrap();
    write_pi_session_jsonl(
        &project.join("2026-06-24T12-00-00-000Z_pi-env-refresh.jsonl"),
        "pi-env-refresh",
        "pi env refresh oracle",
    );

    let sources = json_output(
        ctx(&temp)
            .env("PI_CODING_AGENT_SESSION_DIR", &path)
            .args(["sources", "--json"]),
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
        "--json",
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
            predicate::str::contains("pi session entry appeared before session header")
                .and(predicate::str::contains(path.to_str().unwrap())),
        );
}

#[test]
fn import_rejects_nonexistent_path() {
    let temp = tempdir();
    let path = temp.path().join("missing-codex-history");
    let path = path.to_str().unwrap();

    ctx(&temp)
        .args(["import", "--provider", "codex", "--path", path])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains(
                "missing explicit path has no matching prior provider route authority",
            )
            .and(predicate::str::contains(path)),
        );
}

#[test]
fn import_rejects_nonexistent_explicit_format_path() {
    let temp = tempdir();
    let path = temp.path().join("missing-file.jsonl");
    let path = path.to_str().unwrap();

    ctx(&temp)
        .args(["import", "--format", "ctx-history-jsonl-v1", "--path", path])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("all import sources failed")
                .and(predicate::str::contains(path)),
        )
        .stdout(predicate::str::contains("outcome: failure"));
}

#[test]
fn import_path_requires_provider_before_opening_store() {
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
        !temp.path().join("work.sqlite").exists(),
        "native path import without provider should fail before opening the store"
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
            predicate::str::contains("symlinked Pi session roots are rejected")
                .and(predicate::str::contains(path.to_str().unwrap())),
        );
}

#[cfg(unix)]
#[test]
fn import_reports_unreadable_directory_with_path_context() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir();
    let path = temp.path().join("unreadable-pi-sessions");
    fs::create_dir_all(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        path.to_str().unwrap(),
    ]));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

    assert!(stderr.contains("inventory import source"), "{stderr}");
    assert!(stderr.contains("Permission denied"), "{stderr}");
    assert!(stderr.contains(path.to_str().unwrap()), "{stderr}");
}

#[test]
fn codex_cli_marks_deleted_raw_source_citations_unavailable() {
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
        "--json",
    ]));
    assert_eq!(imported["totals"]["imported_events"], 7);

    fs::remove_dir_all(&copied).unwrap();

    let search = json_output(ctx(&temp).args(["search", "onboarding", "--json"]));
    assert!(search["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|result| result["source_exists"] == false));
    assert!(search["results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|result| result["citations"].as_array().unwrap().iter())
        .any(|citation| citation["source_exists"] == false));
}
