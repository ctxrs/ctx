use super::{
    cancellation::{
        assert_single_semantic_writer, write_manual_semantic_config, LoopbackSemanticServer,
    },
    *,
};

#[test]
fn typed_json_semantic_failure_keeps_the_published_core_generation_active_and_searchable() {
    let temp = tempdir();
    let server = LoopbackSemanticServer::start_with_wrong_dimensions();
    write_manual_semantic_config(&temp, server.endpoint());
    let fixture = temp.path().join("semantic-completion-failure.jsonl");
    let query = "semantic failure keeps the published lexical generation searchable";
    write_valid_explicit_custom_source(&fixture, query);

    let output = ctx(&temp)
        .args([
            "import",
            "--input-format",
            "ctx-history-jsonl-v2",
            "--path",
            fixture.to_str().unwrap(),
            "--format=json",
            "--progress",
            "json",
        ])
        .timeout(Duration::from_secs(20))
        .assert()
        .failure()
        .get_output()
        .clone();

    assert!(
        output.stdout.is_empty(),
        "semantic failure must not emit an import success envelope: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines = stderr
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|error| panic!("stderr line was not JSON ({error}): {line:?}"))
        })
        .collect::<Vec<_>>();
    let (error, progress) = lines
        .split_last()
        .expect("semantic failure must emit a typed terminal error");

    assert_eq!(
        error["error_code"], "semantic_completion_failed",
        "{error:#}"
    );
    assert_eq!(
        error["reason"], "semantic_completion_reconciliation_failed",
        "{error:#}"
    );
    assert_eq!(error["core_published"], true, "{error:#}");
    assert_eq!(error["retryable"], false, "{error:#}");
    assert_eq!(error["error"], error["detail"], "{error:#}");
    assert!(error.get("active_generation_id").is_none(), "{error:#}");
    assert!(error.get("failure_class").is_none(), "{error:#}");
    assert_eq!(
        error.as_object().expect("semantic error object").len(),
        7,
        "{error:#}"
    );
    let generation = error["generation_id"]
        .as_str()
        .expect("semantic error generation ID");
    assert!(
        error["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(&format!(
                "foreground semantic reconciliation failed for Core generation {generation}"
            )) && detail
                .contains("semantic embedding response returned the wrong dimensions")),
        "{error:#}"
    );

    assert!(
        progress
            .iter()
            .any(|event| event["phase"] == "semantic" && event["done"] == false),
        "missing nonterminal semantic progress frame: {progress:#?}"
    );
    let terminal_progress = progress
        .last()
        .expect("semantic failure must follow progress frames");
    assert_eq!(terminal_progress["type"], "ctx_progress", "{stderr}");
    assert_eq!(terminal_progress["operation"], "import", "{stderr}");
    assert_eq!(terminal_progress["phase"], "semantic", "{stderr}");
    assert_eq!(terminal_progress["done"], true, "{stderr}");
    assert_eq!(terminal_progress["message"], error["detail"], "{stderr}");
    assert_eq!(
        progress
            .iter()
            .filter(|event| event["done"] == true)
            .count(),
        1,
        "{progress:#?}"
    );
    assert!(
        progress.iter().all(|event| event["phase"] != "published"),
        "terminal Core success must remain deferred: {progress:#?}"
    );

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["history_epoch"]["status"], "ready", "{status:#}");
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    assert_eq!(status["lexical"]["generation_id"], generation, "{status:#}");
    assert_eq!(
        provider_core_counts(&data_root(&temp), "custom"),
        (1, 1),
        "Core publication must survive semantic completion failure"
    );

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
    assert_eq!(
        search["retrieval"]["generation_id"], generation,
        "{search:#}"
    );
    assert_eq!(search["results"].as_array().unwrap().len(), 1, "{search:#}");
    assert!(
        search["results"][0]["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains(query)),
        "{search:#}"
    );

    assert_single_semantic_writer(&server.finish());
}
