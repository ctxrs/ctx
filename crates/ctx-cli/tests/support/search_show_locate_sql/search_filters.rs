#[test]
fn human_search_reports_no_results() {
    let temp = tempdir();
    let fresh = ctx(&temp)
        .args(["search", "definitely-no-results-here"])
        .output()
        .unwrap();
    if fresh.status.success() {
        let stdout = String::from_utf8(fresh.stdout).unwrap();
        assert!(
            stdout.contains("No results for definitely-no-results-here"),
            "{stdout}"
        );
    } else {
        let stderr = String::from_utf8(fresh.stderr).unwrap();
        assert!(
            stderr.contains(
                "daemon source refresh was queued but no published generation exists; retry with --refresh wait"
            ),
            "{stderr}"
        );
    }
    wait_for_test_daemon_source_refresh(&temp);

    let fixture = provider_history_fixture("codex-sessions");
    ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--progress",
            "none",
        ])
        .assert()
        .success();
    let indexed = ctx(&temp)
        .args(["search", "definitely-no-results-here"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let indexed = String::from_utf8(indexed).unwrap();
    assert!(indexed.contains("No results for definitely-no-results-here"));

    let term_only = ctx(&temp)
        .args(["search", "--term", "term-only-no-results"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let term_only = String::from_utf8(term_only).unwrap();
    assert!(term_only.contains("No results for term-only-no-results"));
}

#[test]
fn search_requires_query_term_or_file_before_refreshing() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["search", "--provider", "codex"]));
    assert!(
        stderr.contains("source-backed search needs a non-empty text query"),
        "{stderr}"
    );
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "invalid search should fail before creating the ctx store"
    );
}

#[test]
fn search_refresh_off_requires_existing_core_generation_without_creating_one() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["search", "anything", "--refresh", "off"]));

    assert!(
        stderr.contains("There is no current searchable generation"),
        "{stderr}"
    );
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "refresh-off search should not create retired work.sqlite"
    );
}

#[test]
fn file_only_search_returns_touched_file_matches() {
    let temp = tempdir();
    let fixture = repository_backed_rich_fixture(&temp);
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
    ]));

    let search = json_output(ctx(&temp).args(["search", "--file", "src/main.rs", "--format=json"]));
    assert_eq!(search["query"], "");
    let results = search["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["provider"], "codex");
    assert!(results[0].get("source_exists").is_none());
}

#[test]
fn search_rejects_whitespace_only_filters() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-rich-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
    ]));
    let no_file =
        failure_stderr(ctx(&temp).args(["search", "test", "--file", " ", "--format=json"]));
    assert!(no_file.contains("query filter file is empty"), "{no_file}");

    let no_workspace =
        failure_stderr(ctx(&temp).args(["search", "test", "--workspace", " ", "--format=json"]));
    assert!(
        no_workspace.contains("query filter workspace is empty"),
        "{no_workspace}"
    );
}

#[test]
fn search_trims_whitespace_padded_workspace_and_file_filters() {
    let temp = tempdir();
    let fixture = repository_backed_rich_fixture(&temp);
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
    ]));

    let with_workspace = json_output(ctx(&temp).args([
        "search",
        "diagnostic",
        "--workspace",
        " ctx-rich-fixture ",
        "--format=json",
    ]));
    assert_eq!(
        with_workspace["filters"]["workspace"], "ctx-rich-fixture",
        "workspace filter value should be trimmed; got filters: {}",
        with_workspace["filters"],
    );
    assert!(
        !with_workspace["results"].as_array().unwrap().is_empty(),
        "workspace-filtered search should match using the trimmed value"
    );

    let with_file =
        json_output(ctx(&temp).args(["search", "--file", " src/main.rs ", "--format=json"]));
    assert_eq!(
        with_file["filters"]["file"], "src/main.rs",
        "file filter value should be trimmed; got filters: {}",
        with_file["filters"],
    );

    let results = with_file["results"].as_array().unwrap();
    assert!(
        !results.is_empty(),
        "file-filtered search should return results with trimmed path"
    );
    assert_eq!(results[0]["provider"], "codex");
    assert!(results[0].get("source_path").is_none());
}

fn repository_backed_rich_fixture(temp: &TempDir) -> String {
    let fixture = provider_history_fixture("codex-rich-sessions");
    let repository = temp.path().join("ctx-rich-fixture");
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(repository.join("src/main.rs"), "fn main() {}\n").unwrap();
    let initialized = StdCommand::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repository)
        .status()
        .unwrap();
    assert!(initialized.success());
    let remote = StdCommand::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/ctxrs/ctx.git",
        ])
        .current_dir(&repository)
        .status()
        .unwrap();
    assert!(remote.success());

    let transcript = Path::new(&fixture).join("2026/06/24/rich.jsonl");
    let original = fs::read_to_string(&transcript).unwrap();
    fs::write(
        transcript,
        original.replace(
            "/workspace/ctx-rich-fixture",
            &repository.display().to_string(),
        ),
    )
    .unwrap();
    fixture
}
