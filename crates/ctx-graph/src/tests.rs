use super::*;
use clap::Parser;

fn parse(args: &[&str]) -> GraphArgs {
    cli::Cli::try_parse_from(std::iter::once("ctx graph").chain(args.iter().copied()))
        .unwrap()
        .graph
}

#[test]
fn root_and_embedded_schemas_share_the_graph_commands() {
    command().debug_assert();
    #[derive(Debug, clap::Parser)]
    struct Root {
        #[command(subcommand)]
        command: RootCommand,
    }
    #[derive(Debug, clap::Subcommand)]
    enum RootCommand {
        Graph(GraphArgs),
    }
    Root::command().debug_assert();
    assert!(Root::try_parse_from(["ctx", "graph", "search", "policy"]).is_ok());
    for spelling in ["search", "query"] {
        assert!(matches!(
            parse(&[spelling, "policy"]).command,
            Command::Query(_)
        ));
    }
    assert!(
        command()
            .try_get_matches_from(["graph", "search", "x", "--depth", "7"])
            .is_err()
    );
}

#[test]
fn argument_tail_preserves_help_and_usage_exit_codes_without_exiting() {
    assert_eq!(run([OsString::from("--help")]), 0);
    assert_eq!(run([OsString::from("--invalid-graph-option")]), 2);
}

#[test]
fn explicit_discovery_does_not_create_or_open_a_database() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing/project.db");
    assert_eq!(discover_database(Some(&missing)).unwrap(), missing);
    assert!(!missing.exists());
    let foreign = temp.path().join("foreign.db");
    std::fs::write(&foreign, b"not SQLite").unwrap();
    assert_eq!(discover_database(Some(&foreign)).unwrap(), foreign);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"not SQLite");
}

#[test]
fn native_graph_reads_stay_on_the_snapshot_until_explicit_update() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    std::fs::create_dir(&root).unwrap();
    let source = root.join("policy.py");
    std::fs::write(&source, "def old_policy():\n    return 1\n").unwrap();
    let db = root.join(".graf/index.db");
    index::run(&root, &db).unwrap();
    let options = ctx_graph_core::query::SearchOptions::default();
    let first = search(&db, "old_policy", &options).unwrap();
    assert!(
        first
            .graph
            .nodes
            .iter()
            .any(|node| node.label == "old_policy")
    );
    std::fs::write(&source, "def replacement():\n    return 2\n").unwrap();
    let stale = search(&db, "old_policy", &options).unwrap();
    assert_eq!(first.graph.generation, stale.graph.generation);
    assert!(
        stale
            .graph
            .nodes
            .iter()
            .any(|node| node.label == "old_policy")
    );
    run_parsed(parse(&["--db", db.to_str().unwrap(), "update"])).unwrap();
    let fresh = search(&db, "replacement", &options).unwrap();
    assert!(fresh.graph.generation > first.graph.generation);
    assert!(
        fresh
            .graph
            .nodes
            .iter()
            .any(|node| node.label == "replacement")
    );
}

#[test]
fn graph_install_help_and_rejected_legacy_flags_leave_project_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let help = cli::Cli::try_parse_from(["ctx graph", "install", "--help"])
        .unwrap_err()
        .to_string();
    assert!(help.contains("--platform"));
    assert!(help.contains("--project"));
    assert!(help.contains("claude, codebuddy, or gemini"));
    for flag in [
        "--skill",
        "--mcp",
        "--global",
        "--config-root",
        "--profile",
        "--tool-hooks",
    ] {
        assert!(!help.contains(flag), "obsolete flag in help: {flag}");
        let error = cli::Cli::try_parse_from([
            "ctx graph",
            "install",
            "--platform",
            "gemini",
            "--project",
            temp.path().to_str().unwrap(),
            flag,
        ])
        .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
    for args in [vec!["install"], vec!["install", "--platform", "agents"]] {
        assert!(cli::Cli::try_parse_from(std::iter::once("ctx graph").chain(args)).is_err());
    }
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn malformed_hook_input_is_still_an_allow_for_gemini() {
    assert_eq!(
        hook_context(hook_guard::Host::Gemini, None, None, &b"not json"[..]),
        Some(serde_json::json!({"decision": "allow"})),
    );
    assert_eq!(
        hook_context(hook_guard::Host::Claude, None, None, &b"not json"[..]),
        None
    );
}

#[test]
fn explicit_tool_hooks_preserve_user_settings_and_standalone_graf_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join(".gemini/settings.json");
    std::fs::create_dir(settings.parent().unwrap()).unwrap();
    let original = b"{\"userSetting\":true}\n";
    std::fs::write(&settings, original).unwrap();
    let old_receipt = temp.path().join(".graf/setup/gemini-tool-hooks.json");
    std::fs::create_dir_all(old_receipt.parent().unwrap()).unwrap();
    std::fs::write(&old_receipt, b"standalone Graf receipt").unwrap();
    let args = [
        "--platform",
        "gemini",
        "--project",
        temp.path().to_str().unwrap(),
    ];
    let install = [&["install"][..], &args].concat();
    run_parsed(parse(&install)).unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(value["userSetting"], true);
    let generated = value["hooks"]["BeforeTool"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(generated.starts_with("ctx graph hook-guard --platform gemini --project "));
    assert_eq!(
        std::fs::read(&old_receipt).unwrap(),
        b"standalone Graf receipt"
    );
    let uninstall = [&["uninstall"][..], &args].concat();
    run_parsed(parse(&uninstall)).unwrap();
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    assert_eq!(
        std::fs::read(&old_receipt).unwrap(),
        b"standalone Graf receipt"
    );
}

#[test]
fn git_refresh_hooks_remain_installable() {
    let temp = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(temp.path())
            .status()
            .unwrap()
            .success()
    );
    let project = temp.path().to_str().unwrap();
    run_parsed(parse(&["hook", "install", "--project", project])).unwrap();
    for name in ["post-commit", "post-checkout", "post-merge"] {
        assert!(temp.path().join(".git/hooks").join(name).is_file());
    }
    run_parsed(parse(&["hook", "uninstall", "--project", project])).unwrap();
}

#[test]
fn learning_omits_proof_from_a_snapshot_newer_than_the_selected_graph() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("policy.py"), "def policy():\n    return 1\n").unwrap();
    let db = dir.path().join("graph.db");
    index::run(&root, &db).unwrap();
    let initial = mcp::snapshot_for_learning(&db).unwrap();
    let id = &initial
        .nodes
        .iter()
        .find(|node| node.label == "policy" && node.kind == "function")
        .unwrap()
        .id;
    let selected = Store::open_read_only(&db)
        .unwrap()
        .neighbors_resolved(id, &ctx_graph_core::query::SearchOptions::default())
        .unwrap();
    std::fs::write(root.join("policy.py"), "def policy():\n    return 2\n").unwrap();
    index::run(&root, &db).unwrap();
    let current = mcp::snapshot_for_learning(&db).unwrap();
    assert_ne!(current.generation, selected.graph.generation);
    let memory = dir.path().join("memory");
    let annotated = learning_annotations(&memory, &selected.graph, Some(&current), None);
    assert!(annotated.get("learning").is_none());
    assert!(
        annotated["learning_notice"]
            .as_str()
            .unwrap()
            .contains("generation differs")
    );
    assert!(!memory.exists());
}
