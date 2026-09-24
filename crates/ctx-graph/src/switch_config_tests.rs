use super::*;

fn project() -> &'static Path {
    Path::new("/work/demo")
}
fn entry(args: &[&str]) -> Value {
    json!({"command": "python3", "args": args})
}
fn config(value: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"mcpServers": value})).unwrap()
}
fn plan(path: &str, bytes: Option<&[u8]>, server: Option<&str>) -> Result<Vec<u8>> {
    rewrite(
        Path::new(path),
        bytes,
        project(),
        server,
        Path::new("/tools/ctx"),
        Path::new("/work/demo/.graf/graph.db"),
    )
}

#[test]
fn json_keeps_metadata_and_other_servers_and_replaces_whole_entry() {
    let source = json!({
        "metadata": {"nested": [null, true, 18446744073709551615u64, {"x":"✓"}]},
        "mcpServers": {
            "old": {"command":"python", "args":["-m","graphify.serve"], "cwd":"${workspaceFolder}", "env":{"OLD":"value"}, "transport":"stdio"},
            "other": {"url":"https://example.invalid/mcp", "custom": [1, 2]}
        }
    });
    let bytes = serde_json::to_vec(&source).unwrap();
    let result = plan(".mcp.json", Some(&bytes), None).unwrap();
    let value: Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(value["metadata"], source["metadata"]);
    assert_eq!(value["mcpServers"]["other"], source["mcpServers"]["other"]);
    assert!(value["mcpServers"].get("old").is_none());
    assert_eq!(
        value["mcpServers"]["ctx-graph"],
        json!({"command":"/tools/ctx", "args":["graph", "--db","/work/demo/.graf/graph.db","serve"]})
    );
}

#[test]
fn absent_config_and_vscode_schema() {
    let created = plan(".mcp.json", None, None).unwrap();
    let value: Value = serde_json::from_slice(&created).unwrap();
    assert!(value["mcpServers"]["ctx-graph"].is_object());
    let original = serde_json::to_vec(
        &json!({"inputs":[{"id":"x"}], "servers":{"old":entry(&["-m","graphify.serve"])}}),
    )
    .unwrap();
    let result = plan(".vscode/mcp.json", Some(&original), None).unwrap();
    let value: Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(value["inputs"], json!([{"id":"x"}]));
    assert_eq!(value["servers"]["ctx-graph"]["type"], "stdio");
    assert!(plan(".mcp.json", None, Some("old")).is_err());
}

#[test]
fn malformed_duplicate_and_oversized_configs_fail() {
    for bytes in [
        br#"{"mcpServers":{},"mcpServers":{}}"#.as_slice(),
        br#"{"mcpServers":{},"metadata":{"x":1,"x":2}}"#,
        br#"{"mcpServers":{},"metadata":[{"x":1,"\u0078":2}]}"#,
        br#"{"mcpServers":{},}"#,
        br#"{"mcpServers":{},"servers":{}}"#,
        br#"{"mcpServers":[]}"#,
        b"{}",
        b"[]",
        b"{",
        b"\xff",
    ] {
        assert!(
            candidates(Path::new("config.json"), bytes, project()).is_err(),
            "{bytes:?}"
        );
    }
    assert!(
        candidates(
            Path::new("config.toml"),
            b"[mcp_servers]\n[mcp_servers]",
            project()
        )
        .is_err()
    );
    assert!(
        candidates(
            Path::new("config.json"),
            &vec![b' '; MAX_BYTES + 1],
            project()
        )
        .is_err()
    );
}

#[test]
fn graph_arguments_cwd_and_workspace_paths() {
    for (command, args, cwd, expected) in [
        (
            "python",
            vec!["-m", "graphify.serve"],
            None,
            "/work/demo/graphify-out/graph.json",
        ),
        (
            "/venv/bin/python3.12",
            vec![
                "-u",
                "-m",
                "graphify.serve",
                "--graph",
                "out/graph.json",
                "--transport",
                "stdio",
            ],
            None,
            "/work/demo/out/graph.json",
        ),
        (
            "uv",
            vec!["run", "python", "-m", "graphify.serve", "graph.json"],
            Some("subdir"),
            "/work/demo/subdir/graph.json",
        ),
        (
            "/tools/uv",
            vec![
                "run",
                "--with",
                "graphify",
                "-m",
                "graphify.serve",
                "--graph=${workspaceFolder}/data/g.json",
            ],
            None,
            "/work/demo/data/g.json",
        ),
        (
            "python3",
            vec![
                "-m",
                "graphify.serve",
                "--transport=stdio",
                "${workspace.path}/graph.json",
            ],
            Some("${workspaceFolder}/subdir"),
            "/work/demo/graph.json",
        ),
        (
            "python3",
            vec!["-m", "graphify.serve", "../graph.json"],
            Some("${workspace.path}/subdir"),
            "/work/demo/subdir/../graph.json",
        ),
    ] {
        let mut server = json!({"command":command,"args":args});
        if let Some(cwd) = cwd {
            server["cwd"] = json!(cwd);
        }
        let found = candidates(
            Path::new("config.json"),
            &config(json!({"custom-name":server})),
            project(),
        )
        .unwrap();
        assert_eq!(
            found,
            vec![Candidate {
                name: "custom-name".into(),
                graph: expected.into()
            }]
        );
    }
}

#[test]
fn rejects_http_disabled_wrappers_and_ambiguous_invocations() {
    let base = entry(&["-m", "graphify.serve"]);
    for (key, value) in [
        ("url", json!("https://example.invalid")),
        ("url", Value::Null),
        ("type", json!("http")),
        ("transport", json!("sse")),
        ("disabled", json!(true)),
        ("enabled", json!(false)),
        ("disabled", json!("false")),
        ("command", json!("bash")),
        ("command", json!("node")),
        ("command", json!("python-wrapper")),
        ("args", json!(["-c", "-m graphify.serve"])),
        ("args", json!(["-m", "other", "-m", "graphify.serve"])),
        (
            "args",
            json!(["-m", "graphify.serve", "--transport", "http"]),
        ),
        (
            "args",
            json!(["-m", "graphify.serve", "--graph", "a.json", "b.json"]),
        ),
        ("args", json!(["-m", "graphify.serve", "--unknown"])),
        ("args", json!(["-m", "graphify.serve", "--graph"])),
        ("args", json!(["-m", "graphify.serve", "--graph="])),
    ] {
        let mut value_entry = base.clone();
        value_entry[key] = value;
        let bytes = config(json!({"graphify":value_entry}));
        assert!(
            candidates(Path::new("config.json"), &bytes, project())
                .unwrap()
                .is_empty(),
            "{key}"
        );
        assert!(plan(".mcp.json", Some(&bytes), Some("graphify")).is_err());
    }
    for args in [
        vec!["run", "bash", "-c", "python -m graphify.serve"],
        vec![
            "run",
            "--project",
            "/other",
            "python",
            "-m",
            "graphify.serve",
        ],
    ] {
        assert!(invocation_graph("uv", &args).is_none());
    }
}

#[test]
fn excludes_other_projects_and_unexpanded_variables() {
    for graph in [
        "/work/other/graph.json",
        "../other/graph.json",
        "/work/demo-other/graph.json",
        "${HOME}/graph.json",
        "${workspaceFolder:other}/graph.json",
        "${workspaceFolder}//other/graph.json",
        "~/graph.json",
        "$HOME/graph.json",
        "https://example.invalid/graph.json",
    ] {
        let bytes = config(json!({"old":entry(&["-m","graphify.serve",graph])}));
        assert!(
            candidates(Path::new("config.json"), &bytes, project())
                .unwrap()
                .is_empty(),
            "{graph}"
        );
        assert!(plan(".mcp.json", Some(&bytes), None).is_err());
    }
    let bytes = config(
        json!({"old":{"command":"python","args":["-m","graphify.serve"],"cwd":"/work/other"}}),
    );
    assert!(
        candidates(Path::new("config.json"), &bytes, project())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn implicit_graph_with_environment_override_is_not_guessed() {
    let mut server = entry(&["-m", "graphify.serve"]);
    server["env"] = json!({"GRAPHIFY_OUT": "/work/other"});
    assert!(graph_path(&server, project()).is_none());
    server["args"] = json!(["-m", "graphify.serve", "graphify-out/graph.json"]);
    assert_eq!(
        graph_path(&server, project()),
        Some(project().join("graphify-out/graph.json"))
    );
    let bytes = br#"[mcp_servers.old]
command = "python"
args = ["-m", "graphify.serve"]
[mcp_servers.old.env]
GRAPHIFY_OUT = "/work/other"
"#;
    assert!(
        candidates(Path::new("config.toml"), bytes, project())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn selection_and_graf_collision() {
    let bytes = config(
        json!({"one":entry(&["-m","graphify.serve"]),"two":entry(&["-m","graphify.serve","second.json"])}),
    );
    assert!(plan(".mcp.json", Some(&bytes), None).is_err());
    assert!(plan(".mcp.json", Some(&bytes), Some("missing")).is_err());
    let result = plan(".mcp.json", Some(&bytes), Some("two")).unwrap();
    let value: Value = serde_json::from_slice(&result).unwrap();
    assert!(value["mcpServers"]["one"].is_object());
    let bytes =
        config(json!({"old":entry(&["-m","graphify.serve"]),"ctx-graph":{"command":"ctx-graph"}}));
    assert!(
        plan(".mcp.json", Some(&bytes), Some("old"))
            .unwrap_err()
            .to_string()
            .contains("already exists")
    );
}

#[test]
fn toml_preserves_unrelated_values_and_comments() {
    let bytes = br#"# preferences
model = "example" # keep this comment

[mcp_servers.old]
command = "python3"
args = ["-m", "graphify.serve", "--graph", "${workspaceFolder}/graph.json"]
cwd = "${workspaceFolder}"

[mcp_servers.old.env]
OLD = "value"

# other server
[mcp_servers.other]
command = "other" # retain inline comment
args = ["a"]

[unrelated]
when = 2026-09-17T12:30:00Z # date stays a date
"#;
    let found = candidates(Path::new("config.toml"), bytes, project()).unwrap();
    assert_eq!(found[0].graph, Path::new("/work/demo/graph.json"));
    let result = plan("config.toml", Some(bytes), None).unwrap();
    let output = String::from_utf8(result).unwrap();
    assert!(output.contains("# preferences\nmodel = \"example\" # keep this comment"));
    assert!(output.contains(
        "# other server\n[mcp_servers.other]\ncommand = \"other\" # retain inline comment"
    ));
    assert!(output.contains("when = 2026-09-17T12:30:00Z # date stays a date"));
    assert!(!output.contains("OLD"));
    let doc: DocumentMut = output.parse().unwrap();
    assert!(doc["mcp_servers"].get("old").is_none());
    assert_eq!(
        doc["mcp_servers"]["ctx-graph"]["command"].as_str(),
        Some("/tools/ctx")
    );
    assert_eq!(
        doc["mcp_servers"]["ctx-graph"]["args"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn toml_inline_servers_and_collision() {
    let bytes = br#"mcp_servers = { old = { command = "python", args = ["-m", "graphify.serve"] }, other = { command = "other" } } # keep
"#;
    let result = plan("config.toml", Some(bytes), None).unwrap();
    let output = String::from_utf8(result).unwrap();
    assert!(output.contains("# keep"));
    let doc: DocumentMut = output.parse().unwrap();
    assert_eq!(
        doc["mcp_servers"]["other"]["command"].as_str(),
        Some("other")
    );
    assert_eq!(
        doc["mcp_servers"]["ctx-graph"]["command"].as_str(),
        Some("/tools/ctx")
    );
    let collision = br#"[mcp_servers.old]
command = "python"
args = ["-m", "graphify.serve"]
[mcp_servers.ctx-graph]
command = "ctx-graph"
"#;
    assert!(plan("config.toml", Some(collision), None).is_err());
}
