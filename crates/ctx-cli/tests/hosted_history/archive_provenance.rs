use std::{collections::BTreeSet, fs};

use serde_json::{json, Value};

use super::support::{seed_history, success, Sandbox};

#[test]
fn archive_labels_are_creator_supplied_and_survive_without_archive_files() {
    let source = Sandbox::new();
    seed_history(&source);
    let destination = Sandbox::new();
    for origin in ["synthetic-laptop", "synthetic-desktop"] {
        success(
            source
                .command()
                .args([
                    "archive",
                    "export",
                    "--origin",
                    origin,
                    "--view",
                    "work `notes`",
                    "--output",
                ])
                .arg(source.path(origin))
                .arg("--format=json"),
        );
        success(
            destination
                .command()
                .args(["archive", "restore"])
                .arg(source.path(origin))
                .arg("--format=json"),
        );
    }
    fs::remove_dir_all(source.path("history")).unwrap();
    fs::remove_dir_all(destination.path("history/archives")).unwrap();
    let search = success(destination.command().args([
        "search",
        "observatory",
        "--backend",
        "lexical",
        "--refresh",
        "off",
        "--format=json",
    ]));
    let hits = search["results"].as_array().unwrap();
    assert_eq!(
        hits.len(),
        2,
        "identical content from distinct origins must remain distinct"
    );
    let origins: BTreeSet<_> = hits
        .iter()
        .map(|hit| hit["archive_provenance"]["origin"].as_str().unwrap())
        .collect();
    assert_eq!(
        origins,
        BTreeSet::from(["synthetic-laptop", "synthetic-desktop"])
    );
    assert_ne!(hits[0]["ctx_session_id"], hits[1]["ctx_session_id"]);
    let listing = success(destination.command().args([
        "list",
        "events",
        "--format=json",
        "--content",
        "none",
    ]));
    for hit in hits {
        let expected = &hit["archive_provenance"];
        assert_eq!(expected["label_authority"], "archive_creator");
        assert_eq!(expected["view"], "work `notes`");
        let session = hit["ctx_session_id"].as_str().unwrap();
        let event = hit["ctx_event_id"].as_str().unwrap();
        let shown = success(destination.command().args([
            "show",
            "session",
            session,
            "--mode",
            "log",
            "--format=json",
        ]));
        assert_eq!(&shown["archive_provenance"], expected);
        assert_eq!(&shown["events"][0]["archive_provenance"], expected);
        let shown = success(
            destination
                .command()
                .args(["show", "event", event, "--format=json"]),
        );
        assert_eq!(&shown["event"]["archive_provenance"], expected);
        let located =
            success(
                destination
                    .command()
                    .args(["locate", "event", event, "--format=json"]),
            );
        assert_eq!(&located["archive_provenance"], expected);
        assert!(listing
            .to_string()
            .contains(expected["origin"].as_str().unwrap()));
        for args in [
            vec!["show", "session", session, "--mode", "log"],
            vec!["show", "event", event],
            vec!["locate", "event", event],
            vec!["show", "session", session, "--format=markdown"],
        ] {
            let output = destination.command().args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(
                text.contains(expected["origin"].as_str().unwrap()),
                "{text}"
            );
            assert!(text.to_lowercase().contains("creator-supplied"), "{text}");
            assert!(text.to_lowercase().contains("not verified"), "{text}");
        }
    }
    let output = destination
        .command()
        .args([
            "search",
            "observatory",
            "--backend",
            "lexical",
            "--refresh",
            "off",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("synthetic-laptop") && text.contains("synthetic-desktop"),
        "{text}"
    );
    assert!(text.contains("Creator-supplied"));

    let session = hits[0]["ctx_session_id"].as_str().unwrap();
    let event = hits[0]["ctx_event_id"].as_str().unwrap();
    let calls = [
        (
            "search",
            json!({"query":"observatory", "backend":"lexical"}),
        ),
        (
            "show_session",
            json!({"ctx_session_id":session, "mode":"log"}),
        ),
        ("show_event", json!({"ctx_event_id":event})),
        ("query_events", json!({"content":"none"})),
    ];
    let mut messages = vec![
        json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
            "protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"archive-test", "version":"1"}}}),
        json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
    ];
    for (i, (name, arguments)) in calls.iter().enumerate() {
        messages.push(json!({"jsonrpc":"2.0", "id":i+2, "method":"tools/call", "params":{"name":name, "arguments":arguments}}));
    }
    let input = messages
        .iter()
        .map(|m| format!("{m}\n"))
        .collect::<String>();
    let output = destination
        .command()
        .args(["mcp", "serve", "--graph-db", "absent.db"])
        .write_stdin(input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for id in 2..=5 {
        let result = &responses.iter().find(|r| r["id"] == id).unwrap()["result"];
        assert_ne!(result["isError"], true, "{result}");
        assert!(
            result["structuredContent"]
                .to_string()
                .contains("archive_creator"),
            "{result}"
        );
        if id != 5 {
            assert!(
                result["content"].to_string().contains("creator-supplied"),
                "{result}"
            );
        }
    }
}
