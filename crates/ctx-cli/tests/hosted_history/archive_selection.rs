use super::support::{failure, seed_history_with, success, Sandbox};
use ctx_history_core::{
    derive_event_id, derive_session_id, CoreActivity, EventIdentityInput, LiteralFactKind,
    NativeItemKey, NativeSessionKey, ProviderDeclaredFact, SessionIdentityInput, TypedKey,
    CORE_ACTIVITY_REVISION,
};

const FILTERS: &[&str] = &[
    "archive",
    "export",
    "--origin",
    "synthetic-laptop",
    "--workspace",
    "ALPHA",
    "--since",
    "1970-01-01T00:00:00.100Z",
    "--until",
    "1970-01-01T00:00:00.200Z",
];

fn seed_selection(sandbox: &Sandbox) {
    seed_selection_records(
        sandbox,
        [
            ("selected", Some(99), "other"),
            ("selected", Some(100), "alpha"),
            ("selected", None, "alpha"),
            ("crossed", Some(50), "alpha"),
            ("crossed", Some(150), "other"),
            ("sibling", Some(150), "other"),
        ]
        .map(|(session, time, workspace)| (session.into(), time, workspace.into())),
    );
}

pub(super) fn seed_selection_records(
    sandbox: &Sandbox,
    records: impl IntoIterator<Item = (String, Option<i64>, String)>,
) {
    seed_history_with(sandbox, |template| {
        records
            .into_iter()
            .enumerate()
            .map(|(sequence, (session, time, workspace))| {
                let mut record = template.clone();
                let session_key =
                    NativeSessionKey::native_id("session", TypedKey::utf8(&session).unwrap())
                        .unwrap();
                record.session_id = derive_session_id(SessionIdentityInput {
                    source: &record.source,
                    logical_session_kind: "thread",
                    native_session_key: &session_key,
                })
                .unwrap();
                let event_key = NativeItemKey::native_id(
                    "message",
                    TypedKey::utf8(sequence.to_string()).unwrap(),
                )
                .unwrap();
                record.event_id = derive_event_id(EventIdentityInput {
                    source: &record.source,
                    session_id: record.session_id,
                    logical_item_kind: "message",
                    native_item_key: &event_key,
                    subrecord_selector: None,
                })
                .unwrap();
                record.event_sequence = sequence as u64;
                record.provider_session_id = Some(session);
                record.occurred_at_unix_ms = time;
                record.content.activity = Some(CoreActivity {
                    revision: CORE_ACTIVITY_REVISION,
                    provider_call_id: None,
                    invocation: None,
                    result: None,
                    facts: vec![ProviderDeclaredFact {
                        kind: LiteralFactKind::SessionCwd,
                        value: format!("/work/{workspace}"),
                    }],
                });
                record
            })
            .collect()
    });
}

#[test]
fn preview_and_export_agree_on_whole_conversations_and_reexport_identity() {
    let source = Sandbox::new();
    seed_selection(&source);
    let preview = success(
        source
            .command()
            .args(FILTERS)
            .args(["--preview", "--format=json", "--output"])
            .arg(source.path("unused")),
    );
    assert!(!source.path("unused").exists());
    assert_eq!(preview["operation"], "archive_export_preview");
    assert_eq!(preview["status"], "preview");
    assert!(preview["archive"].is_null());
    let selection = &preview["selection"];
    assert_eq!(selection["selection_complete"], true);
    assert_eq!(selection["label_authority"], "archive_creator");
    assert_eq!(selection["selected_conversations"], 1);
    assert_eq!(selection["scope"]["matched_records"], 1);
    assert_eq!(selection["scope"]["retained_records"], 3);
    assert_eq!(selection["scope"]["records_before_window"], 1);
    assert_eq!(selection["scope"]["untimed_records"], 1);
    assert_eq!(
        selection["conversations"][0]["provider_session_id"],
        "selected"
    );
    let output = source
        .command()
        .args(FILTERS)
        .arg("--preview")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "Creator-supplied",
        "whole conversations",
        "1 before",
        "1 untimed",
        "semantic topic",
        "ctx show session",
        "selected",
        "synthetic-laptop",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    let exported = success(
        source
            .command()
            .args(FILTERS)
            .args(["--format=json", "--output"])
            .arg(source.path("snapshot")),
    );
    assert_eq!(exported["selection"], *selection);
    assert_eq!(exported["manifest"]["members"], 1);
    assert_eq!(exported["manifest"]["records"], 3);
    assert_eq!(exported["manifest"]["selected_subset"], true);

    let destination = Sandbox::new();
    success(
        destination
            .command()
            .args(["archive", "restore"])
            .arg(source.path("snapshot"))
            .arg("--format=json"),
    );
    let reexported = success(
        destination
            .command()
            .args(FILTERS)
            .args(["--format=json", "--output"])
            .arg(destination.path("snapshot")),
    );
    assert_eq!(
        reexported["manifest"]["identity"],
        exported["manifest"]["identity"]
    );
    assert_eq!(
        reexported["manifest"]["inventory_sha256"],
        exported["manifest"]["inventory_sha256"]
    );
}

#[test]
fn empty_selection_is_complete_and_creates_no_archive() {
    let sandbox = Sandbox::new();
    seed_selection(&sandbox);
    for preview in [true, false] {
        let mut command = sandbox.command();
        command
            .args([
                "archive",
                "export",
                "--origin",
                "synthetic-laptop",
                "--workspace",
                "absent",
                "--format=json",
                "--output",
            ])
            .arg(sandbox.path("empty"));
        if preview {
            command.arg("--preview");
        }
        let result = success(&mut command);
        assert_eq!(result["status"], "empty");
        assert_eq!(result["selection"]["selection_complete"], true);
        assert_eq!(result["selection"]["selected_conversations"], 0);
        assert!(result["archive"].is_null());
        assert!(!sandbox.path("empty").exists());
    }
}

#[test]
fn unicode_workspaces_stay_distinct_through_search_preview_export_and_restore() {
    let source = Sandbox::new();
    seed_selection_records(
        &source,
        [
            ("uppercase".into(), Some(100), "ÉCOLE".into()),
            ("lowercase".into(), Some(100), "école".into()),
        ],
    );
    for (workspace, other, session) in [
        ("/work/ÉCOLE", "/work/école", "uppercase"),
        ("/work/école", "/work/ÉCOLE", "lowercase"),
    ] {
        let search_args = [
            "search",
            "observatory",
            "--workspace",
            workspace,
            "--backend=lexical",
            "--refresh=off",
            "--format=json",
        ];
        let search = success(source.command().args(search_args));
        assert_eq!(search["results"].as_array().unwrap().len(), 1);
        let export_args = [
            "archive",
            "export",
            "--origin",
            "synthetic-laptop",
            "--workspace",
            workspace,
            "--format=json",
        ];
        let preview = success(source.command().args(export_args).arg("--preview"));
        let selection = &preview["selection"];
        assert_eq!(selection["selected_conversations"], 1);
        let conversation = &selection["conversations"][0];
        assert_eq!(
            conversation["ctx_session_id"],
            search["results"][0]["ctx_session_id"]
        );
        assert_eq!(conversation["provider_session_id"], session);
        assert_eq!(conversation["workspace_values"][0]["value"], workspace);
        assert_eq!(
            conversation["workspace_values"][0]["matches_workspace"],
            true
        );
        let snapshot = source.path(session);
        let exported = success(
            source
                .command()
                .args(export_args)
                .arg("--output")
                .arg(&snapshot),
        );
        assert_eq!(exported["selection"], *selection);
        assert_eq!(exported["manifest"]["records"], 1);
        let destination = Sandbox::new();
        success(
            destination
                .command()
                .args(["archive", "restore"])
                .arg(snapshot)
                .arg("--format=json"),
        );
        let restored = success(destination.command().args(search_args));
        assert_eq!(restored["results"].as_array().unwrap().len(), 1);
        let missing = success(destination.command().args([
            "search",
            "observatory",
            "--workspace",
            other,
            "--backend=lexical",
            "--refresh=off",
            "--format=json",
        ]));
        assert!(missing["results"].as_array().unwrap().is_empty());
    }
}

#[test]
fn invalid_or_incomplete_filters_fail_without_publishing() {
    let sandbox = Sandbox::new();
    seed_selection(&sandbox);
    for filters in [
        vec!["--since", "1970-01-01T00:00:00.100Z"],
        vec!["--until", "1970-01-01T00:00:00.200Z"],
        vec![
            "--since",
            "1970-01-01T00:00:00.200Z",
            "--until",
            "1970-01-01T00:00:00.100Z",
        ],
        vec!["--since", "1d", "--until", "1970-01-01T00:00:00.200Z"],
        vec![
            "--since",
            "1970-01-01T00:00:00.1001Z",
            "--until",
            "1970-01-01T00:00:00.200Z",
        ],
        vec!["--workspace", "   "],
    ] {
        let result = failure(
            sandbox
                .command()
                .args([
                    "archive",
                    "export",
                    "--origin",
                    "synthetic-laptop",
                    "--format=json",
                    "--output",
                ])
                .arg(sandbox.path("invalid"))
                .args(filters),
        );
        assert_eq!(result["error"]["code"], "invalid_range");
        assert!(!sandbox.path("invalid").exists());
    }
}
