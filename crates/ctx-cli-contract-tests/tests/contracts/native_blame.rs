mod support;

use ctx_attribution::protocol::{
    core_record_sha256, BlameAttribution, BlameResultFreshness, BlameTarget, EvidenceCitation,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_history_snapshot_reader::{CoreSnapshot, SnapshotContract};
use support::*;

#[test]
fn authored_commit_has_identical_native_cli_mcp_results_citations_and_one_terminal_per_call() {
    let temp = daemon_test_root();
    let root = data_root(&temp);
    fs::create_dir_all(&root).unwrap();
    ctx_history_platform::platform_security::restrict_private_directory(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n[sources]\nautomatic = false\n",
    )
    .unwrap();
    let seed = ctx_attribution::test_fixtures::authored_commit_fixture(
        &temp.path().join("authored-repository"),
    )
    .unwrap();
    let mut writer = GenerationWriter::open(root.join("search/lexical"), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(seed.record.source.clone()).unwrap();
    writer.add_core_record(seed.record.clone()).unwrap();
    writer.certify_source(seed.certificate).unwrap();
    let generation = writer.commit(|_| true).unwrap().generation_id;
    let snapshot =
        CoreSnapshot::open(&root, &generation, &SnapshotContract::current().unwrap()).unwrap();
    ctx_attribution::catch_up(&root, &snapshot, &|| false).unwrap();
    let target = BlameTarget::Commit {
        oid: seed.oid.clone(),
        repository: None,
    };
    let result = ctx_attribution::query(&root, &target, 8, None).unwrap();
    assert_eq!(result.freshness, BlameResultFreshness::Current);
    assert_eq!(
        result.result.outcome.attribution,
        BlameAttribution::Possible
    );
    assert!(!result.result.matches.is_empty());
    let citation = EvidenceCitation {
        core_generation_id: generation,
        source: seed.record.source.clone(),
        session_id: seed.record.session_id,
        event_id: seed.record.event_id,
        event_sequence: 1,
        byte_range: None,
        evidence_sha256: Some(core_record_sha256(&seed.record).unwrap()),
    };
    assert!(result
        .result
        .evidence
        .iter()
        .any(|evidence| evidence.citation == citation));
    let previews = ctx_attribution::hydrate_evidence_previews(&root, &result.result);
    let (expected, expected_mcp_text) =
        ctx_attribution::presentation::mcp_text::render_blame_tool(&result, Some(&previews));
    let sink = temp.path().join("never-uploaded.jsonl");
    let endpoint = file_url(&sink);
    for args in [
        vec![
            "blame",
            seed.oid.as_str(),
            "--type=commit",
            "--limit=8",
            "--format=json",
        ],
        vec![
            "blame",
            "commit",
            seed.oid.as_str(),
            "--limit=8",
            "--format=json",
        ],
    ] {
        let response = json_output(
            ctx(&temp)
                .args(args)
                .env("CTX_ANALYTICS_ENABLED", "true")
                .env("CTX_ANALYTICS_ENDPOINT", &endpoint),
        );
        assert_eq!(response, expected);
    }
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"authored-test","version":"0"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"blame","arguments":{"target":{"kind":"commit","oid":seed.oid},"limit":8}}}),
        ],
        &[
            ("CTX_ANALYTICS_ENABLED", "true"),
            ("CTX_ANALYTICS_ENDPOINT", endpoint.as_str()),
        ],
    );
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1]["result"]["structuredContent"], expected);
    assert_eq!(
        responses[1]["result"]["content"][0]["text"],
        expected_mcp_text
    );
    assert!(!sink.exists(), "Blame foreground must not upload");

    let events = read_queued_analytics_events(temp.path())
        .into_iter()
        .flat_map(|payload| payload["events"].as_array().unwrap().clone())
        .filter(|event| {
            event["event_name"] == "operation_completed" && event["operation"] == "blame"
        })
        .collect::<Vec<_>>();
    assert_eq!(
        events.len(),
        3,
        "one ordinary terminal per actual call: {events:#?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["surface"] == "cli")
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["surface"] == "mcp")
            .count(),
        1
    );
    for event in events {
        assert_eq!(event["outcome"], "success");
        let properties = event["properties"].as_object().unwrap();
        if event["surface"] == "cli" {
            assert_analytics_properties_are_allowlisted(properties);
        }
        for (key, value) in [
            ("blame_target_kind", "commit"),
            ("blame_request_kind", "first_request"),
            ("blame_result_state", "possible"),
            ("blame_freshness", "current"),
        ] {
            assert_eq!(properties[key], value, "{event:#}");
        }
        assert_eq!(properties["blame_output_served"], true);
        assert_eq!(properties["blame_has_more"], expected["next"].is_object());
        let encoded = serde_json::to_string(properties).unwrap();
        assert!(!encoded.contains(seed.repository.to_str().unwrap()));
        assert!(!encoded.contains(&citation.core_generation_id));
    }
}
