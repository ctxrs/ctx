#[test]
fn human_search_reports_no_results() {
    let temp = tempdir();
    let fresh = ctx(&temp)
        .args(["search", "definitely-no-results-here"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let fresh = String::from_utf8(fresh).unwrap();
    assert!(fresh.contains("no results for definitely-no-results-here"));
    assert!(fresh.contains("next: ctx import --all"));

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
    assert!(indexed.contains("no results for definitely-no-results-here"));
    assert!(indexed.contains("next: try broader terms with ctx search --term \"<term>\""));

    let term_only = ctx(&temp)
        .args(["search", "--term", "term-only-no-results"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let term_only = String::from_utf8(term_only).unwrap();
    assert!(term_only.contains("no results for --term term-only-no-results"));
}

#[test]
fn search_requires_query_term_or_file_before_refreshing() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["search", "--provider", "codex"]));
    assert!(
        stderr.contains("search needs a query, --term, or --file"),
        "{stderr}"
    );
    assert!(
        stderr.contains("ctx search \"failed migration\""),
        "{stderr}"
    );
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "invalid search should fail before creating the ctx store"
    );

    let punctuation = failure_stderr(ctx(&temp).args(["search", "!!!"]));
    assert!(
        punctuation.contains("search needs a query, --term, or --file"),
        "{punctuation}"
    );
    let hyphen_only = failure_stderr(ctx(&temp).args(["search", "--", "---"]));
    assert!(
        hyphen_only.contains("search needs a query, --term, or --file"),
        "{hyphen_only}"
    );
    let underscore_term = failure_stderr(ctx(&temp).args(["search", "--term", "___"]));
    assert!(
        underscore_term.contains("search needs a query, --term, or --file"),
        "{underscore_term}"
    );
}

#[test]
fn search_refresh_off_requires_existing_store_without_creating_one() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["search", "anything", "--refresh", "off"]));

    assert!(stderr.contains("ctx store is not initialized"), "{stderr}");
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "refresh-off search should not create the ctx store"
    );
}

#[test]
fn file_only_search_returns_touched_file_matches() {
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

    let search = json_output(ctx(&temp).args(["search", "--file", "src/main.rs", "--format=json"]));
    assert_eq!(search["query"], "");
    let results = search["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0]["why_matched"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason == "file_touched"));
    assert!(results[0]["citations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|citation| citation["target_type"] == "file" && citation["label"] == "file touched"));
}

#[test]
fn search_normalizes_whitespace_only_filters() {
    let temp = tempdir();
    let no_file = json_output(ctx(&temp).args(["search", "test", "--file", " ", "--format=json"]));
    assert!(
        !no_file["filters"].as_object().unwrap().contains_key("file"),
        "expected no \"file\" key in filters, got: {}",
        no_file["filters"],
    );

    let no_workspace =
        json_output(ctx(&temp).args(["search", "test", "--workspace", " ", "--format=json"]));
    assert!(
        !no_workspace["filters"]
            .as_object()
            .unwrap()
            .contains_key("workspace"),
        "expected no \"workspace\" key in filters, got: {}",
        no_workspace["filters"],
    );
}

#[test]
fn search_trims_whitespace_padded_workspace_and_file_filters() {
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
    assert!(
        results[0]["why_matched"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "file_touched"),
        "result should match by file_touched"
    );
}
