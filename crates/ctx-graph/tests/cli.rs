use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Output, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

fn ctx_command(cwd: &Path) -> Command {
    let mut command = ctx_root_command(cwd);
    command.arg("graph");
    command
}

fn ctx_root_command(cwd: &Path) -> Command {
    // The binary-contract test target supplies ctx; never select an installed CLI.
    let binary = std::env::var_os("CARGO_BIN_EXE_ctx")
        .expect("the binary-contract runner must supply CARGO_BIN_EXE_ctx");
    let binary = fs::canonicalize(binary).expect("ctx test artifact must exist");
    let mut command = Command::new(binary);
    command.env_clear();
    if cfg!(windows)
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        command.env("SystemRoot", root);
    }
    command
        .env("HOME", cwd.join(".test-home"))
        .env("USERPROFILE", cwd.join(".test-home"))
        .env("XDG_CONFIG_HOME", cwd.join(".test-home/config"))
        .env("XDG_DATA_HOME", cwd.join(".test-home/data"))
        .env("XDG_STATE_HOME", cwd.join(".test-home/state"))
        .env("XDG_RUNTIME_DIR", cwd.join(".test-home/runtime"))
        .env("PATH", cwd.join(".test-home/bin"))
        .env("CTX_DATA_ROOT", cwd.join(".test-history"));
    command
}

#[test]
fn graphify_switch_uses_ctx_graph_and_undo_keeps_the_imported_store() {
    for selective_ignore in [false, true] {
        graphify_switch_roundtrip(selective_ignore);
    }
}

fn git(cwd: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", cwd.join(".test-git-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", cwd.join(".test-git-home/config"))
        .current_dir(cwd)
        .args(args);
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
        .output()
        .expect("Git is required for ignore contracts")
}

fn graphify_switch_roundtrip(selective_ignore: bool) {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let initialized = git(root, &["init", "--quiet", "--template="]);
    assert!(initialized.status.success(), "{initialized:?}");
    let ignore = root.join(".graf/.gitignore");
    let original_ignore = b"# Keep shared graph notes visible\n/index.db";
    if selective_ignore {
        fs::create_dir(root.join(".graf")).unwrap();
        fs::write(&ignore, original_ignore).unwrap();
        fs::write(
            root.join(".graf/shared-notes.txt"),
            b"ordinary project notes",
        )
        .unwrap();
    }
    fs::create_dir(root.join("graphify-out")).unwrap();
    fs::write(
        root.join("graphify-out/graph.json"),
        json!({"directed":true,"multigraph":false,
            "nodes":[{"id":"a","label":"Policy"},{"id":"b","label":"Apply"}],
            "links":[{"source":"a","target":"b","relation":"calls"}]})
        .to_string(),
    )
    .unwrap();
    let config = root.join(".mcp.json");
    let original = b"{\"mcpServers\":{\"graphify\":{\"command\":\"python\",\"args\":[\"-m\",\"graphify.serve\",\"graphify-out/graph.json\"],\"env\":{\"SYNTHETIC_TOKEN\":\"SYNTHETIC_RECEIPT_CANARY\"}},\"other\":{\"command\":\"leave-me\"}}}\n";
    fs::write(&config, original).unwrap();
    let report = success(cli(root, &["switch", "graphify", "--json"]));
    assert_eq!(report["status"], "switched");
    let installed: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(installed["mcpServers"]["other"]["command"], "leave-me");
    let entry = &installed["mcpServers"]["ctx-graph"];
    let args = entry["args"].as_array().unwrap();
    assert_eq!(args[0], "graph");
    assert_eq!(args[1], "--db");
    assert_eq!(args[3], "serve");
    assert_eq!(
        fs::canonicalize(entry["command"].as_str().unwrap()).unwrap(),
        fs::canonicalize(std::env::var_os("CARGO_BIN_EXE_ctx").unwrap()).unwrap(),
    );
    let db = root.join(".graf/index.db");
    assert!(db.is_file());
    let receipt_path = root.join(".graf/ctx-switch-graphify.json");
    let receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(receipt["before"].as_str().unwrap().as_bytes(), original);
    assert!(
        receipt["before"]
            .as_str()
            .unwrap()
            .contains("SYNTHETIC_RECEIPT_CANARY")
    );
    let protected = git(
        root,
        &[
            "check-ignore",
            "--quiet",
            "--",
            ".graf/ctx-switch-graphify.json",
        ],
    );
    assert!(protected.status.success(), "{protected:?}");
    let saved_ignore = fs::read(&ignore).unwrap();
    if selective_ignore {
        assert!(saved_ignore.starts_with(original_ignore));
        let visible = git(
            root,
            &["check-ignore", "--quiet", "--", ".graf/shared-notes.txt"],
        );
        assert_eq!(visible.status.code(), Some(1), "{visible:?}");
    }
    let saved = fs::read(&config).unwrap();
    assert_eq!(
        success(cli(root, &["switch", "graphify", "--json"]))["status"],
        "already_switched"
    );
    assert_eq!(fs::read(&config).unwrap(), saved);
    for _ in 0..2 {
        assert_eq!(
            success(cli(root, &["switch", "--undo", "--json"]))["status"],
            "undone"
        );
        assert_eq!(fs::read(&config).unwrap(), original);
    }
    assert_eq!(fs::read(&ignore).unwrap(), saved_ignore);
    let protected = git(
        root,
        &[
            "check-ignore",
            "--quiet",
            "--",
            ".graf/ctx-switch-graphify.json",
        ],
    );
    assert!(protected.status.success(), "{protected:?}");
    assert!(db.is_file());
    assert!(!root.join(".graf/switch-graphify.json").exists());
}

#[test]
fn graph_commands_work_with_no_history_or_external_executables() {
    let dir = imported();
    let search = success(cli(dir.path(), &["search", "entry", "--json"]));
    let alias = success(cli(dir.path(), &["query", "entry", "--json"]));
    assert_eq!(search, alias);
    assert!(!dir.path().join(".test-history").exists());
    assert!(!dir.path().join(".test-home").exists());
    let bad_history = dir.path().join(".test-history");
    fs::create_dir(&bad_history).unwrap();
    let bad_config = bad_history.join("config.toml");
    let malformed_toml = b"[invalid";
    fs::write(&bad_config, malformed_toml).unwrap();
    assert_eq!(
        success(cli(dir.path(), &["search", "entry", "--json"])),
        search
    );
    assert_eq!(fs::read(&bad_config).unwrap(), malformed_toml);
    let parsed_route = ctx_root_command(dir.path())
        .current_dir(dir.path())
        .args(["--color", "never", "graph", "search", "entry", "--json"])
        .output()
        .unwrap();
    assert_eq!(success(parsed_route), search);
    assert_eq!(fs::read(&bad_config).unwrap(), malformed_toml);
    assert_eq!(fs::read_dir(&bad_history).unwrap().count(), 1);
    assert!(!dir.path().join(".test-home").exists());
}

fn cli(cwd: &Path, args: &[&str]) -> Output {
    ctx_command(cwd)
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout must contain exactly one JSON value")
}

fn failure(output: Output) -> String {
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "failure must not write prose to stdout"
    );
    String::from_utf8(output.stderr).unwrap()
}

fn imported() -> TempDir {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("graph.json"),
        json!({
            "directed": true, "multigraph": false,
            "nodes": [
                {"id":"a", "label":"entry"},
                {"id":"b", "label":"middle"},
                {"id":"c", "label":"leaf"},
                {"id":"x", "label":"member"},
                {"id":"d1", "label":"duplicate"},
                {"id":"d2", "label":"duplicate"}
            ],
            "links": [
                {"source":"a", "target":"b", "relation":"calls"},
                {"source":"b", "target":"c", "relation":"calls"},
                {"source":"c", "target":"x", "relation":"contains"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let stats = success(cli(
        dir.path(),
        &["import", "graphify", "graph.json", "--json"],
    ));
    assert_eq!(stats["kind"], "imported");
    dir
}

#[test]
fn compact_reclaims_native_space_without_source_access_or_graph_changes() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("keep.py"), "def orchard():\n    return 42\n").unwrap();
    let discarded = (0..160)
        .map(|i| format!("def discarded_{i}():\n    return {i}\n"))
        .collect::<String>();
    fs::write(source.join("discard.py"), discarded).unwrap();
    let db = dir.path().join("index.db");
    graf::index::run(&source, &db).unwrap();
    fs::remove_file(source.join("discard.py")).unwrap();
    graf::index::run(&source, &db).unwrap();
    let before = serde_json::to_value(
        graf::store::Store::open_read_only(&db)
            .unwrap()
            .snapshot()
            .unwrap(),
    )
    .unwrap();
    fs::remove_dir_all(&source).unwrap();

    let report = success(cli(dir.path(), &["--db", "index.db", "compact", "--json"]));
    assert_eq!(report["schema_version"], 1);
    assert!(report["pages_after"].as_u64().unwrap() < report["pages_before"].as_u64().unwrap());
    assert_eq!(report["free_pages_after"], 0);
    assert_eq!(report["checkpoint_busy"], false);
    assert_eq!(
        fs::metadata(&db).unwrap().len(),
        report["pages_after"].as_u64().unwrap() * report["page_size"].as_u64().unwrap()
    );
    let found = success(cli(
        dir.path(),
        &["--db", "index.db", "query", "orchard", "--json"],
    ));
    assert!(
        found["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["label"] == "orchard")
    );
    let human = cli(dir.path(), &["--db", "index.db", "compact"]);
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    assert!(
        String::from_utf8(human.stdout)
            .unwrap()
            .contains("Compacted database:")
    );
    let after = serde_json::to_value(
        graf::store::Store::open_read_only(&db)
            .unwrap()
            .snapshot()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(after, before);
    assert!(!source.exists());
}

#[test]
fn compact_discovers_imported_database_and_does_not_create_a_missing_one() {
    let dir = imported();
    fs::remove_file(dir.path().join("graph.json")).unwrap();
    fs::create_dir(dir.path().join("child")).unwrap();
    let before = success(cli(dir.path(), &["stats", "--json"]));
    let report = success(cli(&dir.path().join("child"), &["compact", "--json"]));
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["checkpoint_busy"], false);
    assert_eq!(success(cli(dir.path(), &["stats", "--json"])), before);
    let path = success(cli(dir.path(), &["path", "a", "b", "--json"]));
    assert_eq!(path["found"], true);
    failure(cli(
        dir.path(),
        &["--db", "missing.db", "compact", "--json"],
    ));
    assert!(!dir.path().join("missing.db").exists());
}

#[test]
fn compact_rejects_legacy_storage_without_migration_or_writes() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("old.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(include_str!("fixtures/native-format1.sql"))
            .unwrap();
    }
    let before = fs::read(&db).unwrap();
    let error = failure(cli(dir.path(), &["--db", "old.db", "compact", "--json"]));
    assert!(error.contains("storage"), "{error}");
    assert_eq!(fs::read(&db).unwrap(), before);
}

fn ids(graph: &Value) -> Vec<&str> {
    let mut ids: Vec<_> = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

fn learning_cli_fixture() -> (TempDir, String) {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("source")).unwrap();
    fs::write(
        dir.path().join("source/policy.py"),
        "def policy():\n    return 1\n",
    )
    .unwrap();
    let db = dir.path().join("learning.db");
    graf::index::run(&dir.path().join("source"), &db).unwrap();
    let graph = graf::store::Store::open_read_only(&db)
        .unwrap()
        .snapshot()
        .unwrap();
    let id = graph
        .nodes
        .iter()
        .find(|node| node.label == "policy" && node.kind == "function")
        .unwrap()
        .id
        .clone();
    (dir, id)
}

fn save_cli_learning(dir: &Path, id: &str) {
    success(cli(
        dir,
        &[
            "--db",
            "learning.db",
            "--json",
            "save-result",
            "--question",
            "Where is the policy?",
            "--answer",
            "Do not include this saved answer in node annotations.",
            "--nodes",
            id,
            "--outcome",
            "useful",
            "--memory-dir",
            "memory",
        ],
    ));
}

#[test]
fn explain_learning_is_explicit_read_only_and_fresh_in_json_and_text() {
    let (dir, id) = learning_cli_fixture();
    let args = ["--db", "learning.db", "explain", &id, "--json"];
    let baseline = success(cli(dir.path(), &args));
    let baseline_text = cli(dir.path(), &["--db", "learning.db", "show", &id]);
    assert!(baseline_text.status.success());
    save_cli_learning(dir.path(), &id);
    save_cli_learning(dir.path(), &id);
    let db_before = fs::read(dir.path().join("learning.db")).unwrap();
    let memory_before: std::collections::BTreeMap<_, _> = fs::read_dir(dir.path().join("memory"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (path.clone(), fs::read(path).unwrap())
        })
        .collect();
    assert_eq!(success(cli(dir.path(), &args)), baseline);
    assert_eq!(
        cli(dir.path(), &["--db", "learning.db", "show", &id]).stdout,
        baseline_text.stdout
    );
    let annotated_args = [
        "--db",
        "learning.db",
        "explain",
        &id,
        "--json",
        "--memory-dir",
        "memory",
    ];
    let mut annotated = success(cli(dir.path(), &annotated_args));
    assert_eq!(annotated["learning"]["nodes"][&id]["status"], "preferred");
    assert_eq!(annotated["learning"]["nodes"][&id]["verified_useful"], 2);
    assert!(annotated["learning"].get("lessons").is_none());
    assert!(
        !annotated
            .to_string()
            .contains("Do not include this saved answer")
    );
    annotated.as_object_mut().unwrap().remove("learning");
    assert_eq!(annotated, baseline);
    let text = cli(
        dir.path(),
        &[
            "--db",
            "learning.db",
            "explain",
            &id,
            "--memory-dir",
            "memory",
        ],
    );
    assert!(text.status.success());
    assert!(text.stdout.starts_with(&baseline_text.stdout));
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Lesson ") && text.contains("preferred"));
    assert_terminal_safe(&text);
    fs::write(
        dir.path().join("source/policy.py"),
        "def policy():\n    return 2\n",
    )
    .unwrap();
    let stale = success(cli(dir.path(), &annotated_args));
    assert_eq!(stale["learning"]["nodes"][&id]["status"], "stale");
    assert_eq!(stale["generation"], baseline["generation"]);
    let text = cli(
        dir.path(),
        &[
            "--db",
            "learning.db",
            "explain",
            &id,
            "--memory-dir",
            "memory",
        ],
    );
    assert!(text.status.success());
    assert!(String::from_utf8(text.stdout).unwrap().contains("stale"));
    assert_eq!(fs::read(dir.path().join("learning.db")).unwrap(), db_before);
    for (path, bytes) in memory_before {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(fs::read_dir(dir.path().join("memory")).unwrap().count(), 2);
    assert!(!dir.path().join("graf-out").exists());
    failure(cli(
        dir.path(),
        &[
            "--db",
            "learning.db",
            "callers",
            &id,
            "--memory-dir",
            "memory",
        ],
    ));
}

#[test]
fn explain_learning_budget_and_invalid_memory_preserve_the_selected_graph() {
    let (dir, id) = learning_cli_fixture();
    save_cli_learning(dir.path(), &id);
    let full = success(cli(
        dir.path(),
        &[
            "--db",
            "learning.db",
            "show",
            &id,
            "--json",
            "--budget",
            "2000",
        ],
    ));
    let bytes = serde_json::to_vec(&full["graph"]).unwrap().len();
    let budget = (bytes.div_ceil(4) + 4).to_string();
    let base_args = [
        "--db",
        "learning.db",
        "show",
        &id,
        "--json",
        "--budget",
        &budget,
    ];
    let baseline = success(cli(dir.path(), &base_args));
    let mut annotated_args = base_args.to_vec();
    annotated_args.extend(["--memory-dir", "memory"]);
    let mut annotated = success(cli(dir.path(), &annotated_args));
    assert!(annotated.get("learning").is_none());
    assert!(
        annotated["learning_notice"]
            .as_str()
            .unwrap()
            .contains("budget")
    );
    annotated.as_object_mut().unwrap().remove("learning_notice");
    assert_eq!(annotated, baseline);
    fs::write(
        dir.path().join("memory/too-large.md"),
        vec![b'x'; 2 * 1024 * 1024 + 1],
    )
    .unwrap();
    let mut invalid = success(cli(
        dir.path(),
        &[
            "--db",
            "learning.db",
            "show",
            &id,
            "--json",
            "--budget",
            "2000",
            "--memory-dir",
            "memory",
        ],
    ));
    assert!(
        invalid["learning_notice"]
            .as_str()
            .unwrap()
            .contains("memory could not be read")
    );
    invalid.as_object_mut().unwrap().remove("learning_notice");
    assert_eq!(invalid, full);
    assert_eq!(success(cli(dir.path(), &base_args)), baseline);
    let absent = dir.path().join("absent-memory");
    let empty = success(cli(
        dir.path(),
        &[
            "--db",
            "learning.db",
            "explain",
            &id,
            "--json",
            "--memory-dir",
            absent.to_str().unwrap(),
        ],
    ));
    assert_eq!(empty["learning"]["status"], "complete");
    assert!(empty["learning"]["nodes"].as_object().unwrap().is_empty());
    assert!(!absent.exists());
}

#[test]
fn imported_cli_routes_direction_relations_and_preserves_snapshot() {
    let dir = imported();
    let db = dir.path().join(".graf/index.db");
    let before = fs::read(&db).unwrap();
    let callers = success(cli(dir.path(), &["--json", "callers", "b"]));
    assert_eq!(ids(&callers), ["a", "b"]);
    let callees = success(cli(dir.path(), &["callees", "b", "--json"]));
    assert_eq!(ids(&callees), ["b", "c"]);
    let impact = success(cli(dir.path(), &["impact", "c", "--json"]));
    assert_eq!(ids(&impact["graph"]), ["a", "b", "c", "x"]);
    assert_eq!(impact["seeds"], json!(["c", "x"]));
    let show = success(cli(dir.path(), &["show", "c", "--json"]));
    assert_eq!(ids(&show), ["b", "c", "x"]);
    let query = success(cli(
        dir.path(),
        &[
            "query",
            "middle",
            "--direction",
            "in",
            "--relation",
            "calls",
            "--json",
        ],
    ));
    assert_eq!(ids(&query), ["a", "b"]);
    let path = success(cli(dir.path(), &["path", "a", "c", "--json"]));
    assert_eq!(path["found"], true);
    assert_eq!(path["graph"]["schema_version"], 1);
    let reverse = success(cli(dir.path(), &["path", "c", "a", "--json"]));
    assert_eq!(reverse["found"], false);
    let incoming = success(cli(
        dir.path(),
        &["path", "c", "a", "--direction", "in", "--json"],
    ));
    assert_eq!(incoming["found"], true);
    let bounded = success(cli(dir.path(), &["show", "b", "--limit", "1", "--json"]));
    assert_eq!(bounded["truncated"], true);
    assert!(bounded["nodes"].as_array().unwrap().len() <= 1);
    let error = failure(cli(dir.path(), &["show", "duplicate", "--json"]));
    assert!(error.contains("d1") && error.contains("d2"), "{error}");
    failure(cli(dir.path(), &["update", "--json"]));
    failure(cli(
        dir.path(),
        &["import", "graphify", "graph.json", "--json"],
    ));
    assert_eq!(fs::read(db).unwrap(), before);
    let human = cli(dir.path(), &["stats"]);
    assert!(human.status.success());
    assert!(String::from_utf8(human.stdout).unwrap().contains("6 nodes"));
}

#[test]
fn native_index_discovers_ancestors_and_update_uses_recorded_root() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("project");
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(
        root.join("demo.py"),
        "def leaf():\n    pass\n\ndef entry():\n    leaf()\n",
    )
    .unwrap();
    let initial = success(cli(dir.path(), &["index", "project", "--json"]));
    assert_eq!(initial["parsed_files"], 1);
    assert!(root.join(".graf/index.db").is_file());
    assert!(!dir.path().join(".graf").exists());
    let nested = root.join("nested");
    let initial_stats = success(cli(&nested, &["stats", "--json"]));
    assert_eq!(initial_stats["generation"], initial["generation"]);
    fs::write(root.join("demo.py"), "def replacement():\n    pass\n").unwrap();
    // Reads still see the indexed snapshot until update is explicitly requested.
    success(cli(&nested, &["show", "entry", "--json"]));
    let updated = success(cli(&nested, &["update", "--json"]));
    assert!(updated["generation"].as_u64().unwrap() > initial["generation"].as_u64().unwrap());
    success(cli(&nested, &["show", "replacement", "--json"]));
    failure(cli(&nested, &["show", "entry", "--json"]));
    let unchanged = success(cli(&nested, &["update", "--json"]));
    assert_eq!(unchanged["generation"], updated["generation"]);
}

#[test]
fn explicit_database_errors_and_limits_leave_stdout_and_disk_clean() {
    let dir = tempdir().unwrap();
    for command in ["stats", "update", "serve"] {
        failure(cli(dir.path(), &["--db", "missing.db", "--json", command]));
        assert!(!dir.path().join("missing.db").exists());
    }
    fs::write(dir.path().join("bad.json"), "{}").unwrap();
    failure(cli(
        dir.path(),
        &["import", "graphify", "bad.json", "--json"],
    ));
    assert!(!dir.path().join(".graf").exists());
    fs::write(
        dir.path().join("ok.json"),
        r#"{"directed":true,"multigraph":false,"nodes":[],"links":[]}"#,
    )
    .unwrap();
    success(cli(
        dir.path(),
        &[
            "--db",
            "custom.db",
            "import",
            "graphify",
            "ok.json",
            "--json",
        ],
    ));
    success(cli(dir.path(), &["stats", "--db", "custom.db", "--json"]));
    assert!(!dir.path().join(".graf").exists());
    for flags in [
        ["--depth", "7"],
        ["--limit", "0"],
        ["--limit", "501"],
        ["--direction", "sideways"],
    ] {
        failure(cli(
            dir.path(),
            &[
                "--db",
                "custom.db",
                "--json",
                "query",
                "x",
                flags[0],
                flags[1],
            ],
        ));
    }
}

fn assert_terminal_safe(text: &str) {
    assert!(text.chars().all(|c| !c.is_control() || c == '\n'));
    for c in ['\u{200b}', '\u{2028}', '\u{202e}', '\u{2066}', '\u{feff}'] {
        assert!(!text.contains(c));
    }
}

#[test]
fn human_output_escapes_controls_while_json_and_mcp_preserve_values() {
    let dir = tempdir().unwrap();
    let hostile = "normal\x1b[2J\x1b[H\n\r\t\u{85}\u{200b}\u{2028}\u{202e}\u{2066}\u{feff}";
    let escaped = r"normal\u{1b}[2J\u{1b}[H\n\r\t\u{85}\u{200b}\u{2028}\u{202e}\u{2066}\u{feff}";
    fs::write(
        dir.path().join("graph.json"),
        json!({
            "directed": true, "multigraph": false,
            "nodes": [
                {"id":"a", "label":hostile, "source_file":hostile, "file_type":hostile},
                {"id":hostile, "label":"café_東京"}
            ],
            "links": [{"source":"a", "target":hostile, "relation":hostile}]
        })
        .to_string(),
    )
    .unwrap();
    success(cli(
        dir.path(),
        &["import", "graphify", "graph.json", "--json"],
    ));
    let output = cli(dir.path(), &["show", "a"]);
    assert!(output.status.success());
    let human = String::from_utf8(output.stdout).unwrap();
    assert_terminal_safe(&human);
    assert!(human.contains(escaped));
    assert!(human.contains("café_東京"));
    let machine = success(cli(dir.path(), &["show", "a", "--json"]));
    let node = machine["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "a")
        .unwrap();
    assert_eq!(node["label"], hostile);
    assert_eq!(node["file"], hostile);
    assert_eq!(machine["edges"][0]["relation"], hostile);
    let mut client = Mcp::start(dir.path(), "2025-11-25");
    let response = client.call("show", json!({"symbol":"a"}));
    assert_eq!(response["result"]["structuredContent"], machine);

    for args in [
        vec!["show", "missing\x1b[2J\n\u{202e}"],
        vec!["query", "a", "--direction", "bad\x1b[2J\n\u{202e}"],
    ] {
        let error = failure(cli(dir.path(), &args));
        assert_terminal_safe(&error);
        assert!(error.contains(r"\u{1b}[2J"), "{error:?}");
    }
    let help = cli(dir.path(), &["--help"]);
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("Usage: ctx graph")
    );
}

#[cfg(unix)]
#[test]
fn human_diagnostics_and_root_escape_filename_controls() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("project\x1b[2J\n");
    fs::create_dir(&root).unwrap();
    let filename = "bad\x1b[2J\r\t\u{202e}.py";
    fs::write(root.join(filename), "def broken(:\n").unwrap();
    let output = cli(&root, &["index", "--json"]);
    let diagnostics = String::from_utf8(output.stderr.clone()).unwrap();
    assert_terminal_safe(&diagnostics);
    assert!(diagnostics.contains(r"bad\u{1b}[2J\r\t\u{202e}.py"));
    let report = success(output);
    assert_eq!(report["diagnostics"][0]["file"], filename);
    let stats = cli(&root, &["stats"]);
    assert!(stats.status.success());
    let human = String::from_utf8(stats.stdout).unwrap();
    assert_terminal_safe(&human);
    assert!(human.contains(r"project\u{1b}[2J\n"));
}

// A small stdio client fixture: the SDK owns all server protocol handling.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Mcp {
    fn start(cwd: &Path, version: &str) -> Self {
        let mut child = ctx_command(cwd)
            .current_dir(cwd)
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            lines,
        };
        let init = client.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":version,"capabilities":{},"clientInfo":{"name":"graf-test","version":"1"}
        }}));
        assert_eq!(init["result"]["serverInfo"]["name"], "ctx-graph");
        // The SDK negotiates legacy initialize; 2026-07-28 uses discovery.
        let negotiated = if version == "2026-07-28" {
            "2025-11-25"
        } else {
            version
        };
        assert_eq!(init["result"]["protocolVersion"], negotiated);
        client.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        client
    }

    fn send(&mut self, request: Value) {
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, request: Value) -> Value {
        let id = request["id"].clone();
        self.send(request);
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(10))
            .expect("MCP response timeout");
        let response: Value =
            serde_json::from_str(&line).expect("MCP stdout must contain only protocol JSON");
        assert_eq!(response["id"], id);
        response
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_stdio_has_typed_read_only_tools_and_honest_errors() {
    let dir = imported();
    let before = fs::read(dir.path().join(".graf/index.db")).unwrap();
    for version in ["2025-11-25", "2026-07-28"] {
        let mut client = Mcp::start(dir.path(), version);
        let listing = client.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        let tools = listing["result"]["tools"].as_array().unwrap();
        let mut names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort_unstable();
        for required in [
            "callees",
            "callers",
            "impact",
            "path",
            "query",
            "show",
            "stats",
            "graph_stats",
            "god_nodes",
            "get_community",
        ] {
            assert!(names.contains(&required), "missing MCP tool {required}");
        }
        for tool in tools {
            assert_eq!(tool["annotations"]["readOnlyHint"], true);
        }
        let query = tools.iter().find(|t| t["name"] == "query").unwrap();
        assert_eq!(query["inputSchema"]["properties"]["limit"]["maximum"], 500);
        assert_eq!(query["inputSchema"]["properties"]["depth"]["maximum"], 6);
        let result = client.call("callers", json!({"symbol":"b"}));
        assert_eq!(result["result"]["isError"], false);
        assert_eq!(ids(&result["result"]["structuredContent"]), ["a", "b"]);
        assert_eq!(result["result"]["structuredContent"]["schema_version"], 1);
        let error = client.call("show", json!({"symbol":"duplicate"}));
        assert_eq!(error["result"]["isError"], true);
        for args in [
            json!({"text":"a","depth":7}),
            json!({"text":"a","limit":0}),
            json!({"text":"a","limit":501}),
            json!({"text":" "}),
        ] {
            let error = client.call("query", args);
            assert!(
                error["error"].is_object() || error["result"]["isError"] == true,
                "{error}"
            );
        }
        let error = client.call("query", json!({"text":"a", "db":"other.db"}));
        assert!(error["error"].is_object() || error["result"]["isError"] == true);
        let error = client.call("update", json!({}));
        assert!(error["error"].is_object() || error["result"]["isError"] == true);
    }
    assert_eq!(fs::read(dir.path().join(".graf/index.db")).unwrap(), before);
    assert!(!dir.path().join("other.db").exists());
}
