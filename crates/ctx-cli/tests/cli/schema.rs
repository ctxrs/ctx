#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn docs_commands_expose_embedded_docs_and_man_pages() {
    let temp = tempdir();

    let list = json_output(ctx(&temp).args(["docs", "list", "--json"]));
    assert_eq!(list["schema_version"], 1);
    assert!(list["topics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|topic| topic["id"] == "cli-reference"));
    for topic_id in ["docs", "mcp", "sql", "upgrade"] {
        assert!(list["topics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|topic| topic["id"] == topic_id));
    }

    let search = json_output(ctx(&temp).args(["docs", "search", "upgrade", "--json"]));
    assert_eq!(search["schema_version"], 1);
    assert_eq!(search["query"], "upgrade");
    assert!(!search["results"].as_array().unwrap().is_empty());

    let sql_search = json_output(ctx(&temp).args(["docs", "search", "sql", "--json"]));
    assert_eq!(sql_search["results"][0]["id"], "sql");

    let mcp_search = json_output(ctx(&temp).args(["docs", "search", "mcp", "--json"]));
    assert_eq!(mcp_search["results"][0]["id"], "mcp");

    let upgrade_search = json_output(ctx(&temp).args(["docs", "search", "upgrade", "--json"]));
    assert_eq!(upgrade_search["results"][0]["id"], "upgrade");

    let weak_search = json_output(ctx(&temp).args(["docs", "search", "a", "--json"]));
    assert!(weak_search["results"].as_array().unwrap().is_empty());
    assert!(weak_search["suggested_next_commands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|command| command == "ctx docs list"));

    let show = json_output(ctx(&temp).args(["docs", "show", "cli-reference", "--format", "json"]));
    assert_eq!(show["schema_version"], 1);
    assert_eq!(show["id"], "cli-reference");
    assert!(show["body"].as_str().unwrap().contains("ctx search"));

    let mcp = json_output(ctx(&temp).args(["docs", "show", "mcp", "--format", "json"]));
    assert!(mcp["body"].as_str().unwrap().contains("ctx mcp serve"));

    let upgrade = json_output(ctx(&temp).args(["docs", "show", "upgrade", "--format", "json"]));
    assert!(upgrade["body"]
        .as_str()
        .unwrap()
        .contains("ctx upgrade status"));

    let missing_topic = failure_stderr(ctx(&temp).args(["docs", "show", "cli"]));
    assert!(missing_topic.contains("unknown ctx docs topic: cli"));
    assert!(missing_topic.contains("nearest topics:"));
    assert!(missing_topic.contains("ctx docs list"));
    assert!(missing_topic.contains("ctx docs search cli"));

    let man = ctx(&temp)
        .args(["docs", "man", "--print", "ctx"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let man = String::from_utf8(man).unwrap();
    assert!(man.contains(".TH ctx"));
    assert!(man.contains("Search local agent history"));
}

#[cfg(unix)]
#[test]
pub(crate) fn json_commands_do_not_spawn_background_upgrade() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");

    let status = json_output(fake_release_env(
        ctx(&temp).args(["status", "--json"]),
        &release,
    ));
    assert_eq!(status["schema_version"], 1);
    assert_eq!(
        fs::read_to_string(&release.target).unwrap(),
        format!("#!/bin/sh\nprintf 'ctx {}\\n'\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(
        !temp.path().join("upgrade-state.json").exists(),
        "JSON status must not start a background upgrade"
    );
}

#[test]
pub(crate) fn doctor_reports_missing_store_without_creating_it() {
    let temp = tempdir();

    let doctor = json_output(ctx(&temp).args(["doctor", "--json"]));

    assert_eq!(doctor["schema_version"], 1);
    assert_eq!(doctor["ok"], false);
    assert!(doctor["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| {
            finding
                .as_str()
                .unwrap()
                .contains("ctx store is not initialized")
        }));
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "doctor should not create the ctx store"
    );
}
