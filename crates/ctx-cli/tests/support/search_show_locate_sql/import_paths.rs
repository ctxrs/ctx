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
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'pi'"
        ),
        2
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'pi'"
        ),
        2
    );

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
fn import_rejects_nonexistent_path() {
    let temp = tempdir();
    let path = temp.path().join("missing-codex-history");
    let path = path.to_str().unwrap();

    ctx(&temp)
        .args(["import", "--provider", "codex", "--path", path])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("approve explicit source path")
                .and(predicate::str::contains("No such file or directory"))
                .and(predicate::str::contains(path)),
        );
}

#[test]
fn import_rejects_nonexistent_explicit_format_path() {
    let temp = tempdir();
    let path = temp.path().join("missing-file.jsonl");
    let path = path.to_str().unwrap();

    ctx(&temp)
        .args([
            "import",
            "--input-format",
            "ctx-history-jsonl-v1",
            "--path",
            path,
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("approve explicit source path")
                .and(predicate::str::contains("No such file or directory"))
                .and(predicate::str::contains(path)),
        );
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
        "native path import without provider should not initialize relational state"
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
fn codex_cli_fails_closed_when_raw_source_is_deleted() {
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

    let stderr = ctx(&temp)
        .args(["search", "onboarding", "--refresh", "off", "--format=json"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let error: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(error["error_code"], "source_unreadable");
    assert_eq!(error["failure_kind"], "temporarily_unavailable");
    assert_eq!(error["retryable"], true);
}
