use super::*;
use ctx_history_index::{CoreEventRangeFilters, CoreEventRangeSelection};

fn event(session: &str, sequence: u64, time: Option<i64>, workspace: Option<&str>) -> CoreRecord {
    let mut event = record(&source(), session, sequence, "synthetic project history");
    event.provider_session_id = Some(session.into());
    event.occurred_at_unix_ms = time;
    if let Some(workspace) = workspace {
        event.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: None,
            invocation: None,
            result: None,
            facts: vec![ProviderDeclaredFact {
                kind: LiteralFactKind::SessionCwd,
                value: workspace.into(),
            }],
        });
    }
    event
}

fn query() -> CoreEventRangeSelection {
    CoreEventRangeSelection::with_filters(
        100,
        200,
        CoreEventRangeFilters {
            workspace: Some("/workspace/ALPHA".into()),
            ..CoreEventRangeFilters::default()
        },
    )
    .unwrap()
}

#[test]
fn selection_is_same_event_whole_conversation_and_generation_pinned() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("root");
    let mut expected = vec![
        event("selected", 1, Some(99), Some("/workspace/beta")),
        event("selected", 2, Some(100), Some("/workspace/alpha")),
        event("selected", 3, Some(199), None),
        event("selected", 4, Some(200), Some("/workspace/alpha")),
        event("selected", 5, None, Some("/workspace/alpha")),
    ];
    expected[1].content.discovery_exclusion = Some(CoreDiscoveryExclusion::CtxRetrievalDerived);
    expected[1]
        .content
        .activity
        .as_mut()
        .unwrap()
        .facts
        .push(ProviderDeclaredFact {
            kind: LiteralFactKind::ToolWorkdir,
            value: "/workspace/other-tool".into(),
        });
    let mut all = expected.clone();
    all.extend([
        event("crossed", 6, Some(50), Some("/workspace/alpha")),
        event("crossed", 7, Some(150), Some("/workspace/beta")),
        event("sibling", 8, Some(150), Some("/workspace/beta")),
        event("untimed", 9, None, Some("/workspace/alpha")),
    ]);
    publish(&root.join("search/lexical"), &all, 1);
    let plan = plan_export(&root, identity(), &Selection::default(), &query()).unwrap();
    let preview = plan.preview();
    assert!(preview.selection_complete);
    assert_eq!(preview.selected_conversations, 1);
    let scope = &preview.scope;
    assert_eq!((scope.matched_records, scope.retained_records), (1, 5));
    assert_eq!(
        (
            scope.records_before_window,
            scope.records_after_window,
            scope.untimed_records
        ),
        (1, 1, 1)
    );
    assert_eq!(
        (
            scope.records_without_workspace,
            scope.records_outside_workspace,
            scope.records_with_other_workspaces
        ),
        (1, 1, 2)
    );
    let conversation = &preview.conversations[0];
    assert_eq!(
        conversation.ctx_session_id,
        expected[0].session_id.as_uuid().to_string()
    );
    assert_eq!(
        (
            conversation.first_occurred_at_unix_ms,
            conversation.last_occurred_at_unix_ms
        ),
        (Some(99), Some(200))
    );
    assert!(conversation
        .workspace_values
        .iter()
        .any(|value| value.value == "/workspace/other-tool" && !value.matches_workspace));
    let generation = preview.generation_id.clone();
    // A later generation cannot change membership or bytes of this plan.
    publish(
        &root.join("search/lexical"),
        &[event("later", 10, Some(150), Some("/workspace/alpha"))],
        2,
    );
    let archive = temp.path().join("selected");
    let receipt = plan.export(&archive).unwrap();
    assert_eq!(receipt.manifest.generation, generation);
    assert!(receipt.manifest.selected_subset);
    expected.sort_by_key(|record| record.event_id.digest());
    assert_eq!(archive_records(&archive), expected);

    let restored = temp.path().join("restored");
    restore(&archive, &restored, &options()).unwrap();
    let plan = plan_export(&restored, identity(), &Selection::default(), &query()).unwrap();
    let reexport = temp.path().join("reexport");
    let again = plan.export(&reexport).unwrap();
    assert_eq!(again.manifest.identity, identity());
    assert_eq!(
        again.manifest.inventory_sha256,
        receipt.manifest.inventory_sha256
    );
    assert_eq!(archive_records(&reexport), expected);
    assert_eq!(
        restore(&reexport, &restored, &options())
            .unwrap()
            .unchanged_members,
        1
    );
}

#[test]
fn empty_intersection_and_interrupted_selection_never_publish_all_history() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("root");
    let record = event("session", 1, Some(150), Some("/workspace/alpha"));
    publish(&root.join("search/lexical"), &[record], 1);
    let plan = plan_export(
        &root,
        identity(),
        &Selection {
            sessions: ["0".repeat(64)].into(),
            ..Selection::default()
        },
        &query(),
    )
    .unwrap();
    assert!(plan.preview().selection_complete);
    assert_eq!(plan.preview().selected_conversations, 0);
    let destination = temp.path().join("empty");
    assert!(plan
        .export(&destination)
        .unwrap_err()
        .to_string()
        .contains("no matching conversations"));
    assert!(!destination.exists());
    let mut calls = 0;
    let result =
        plan_export_with_control(&root, identity(), &Selection::default(), &query(), || {
            calls += 1;
            if calls == 3 {
                Err(invalid("interrupted"))
            } else {
                Ok(())
            }
        });
    assert!(matches!(result, Err(ArchiveError::Invalid(message)) if message == "interrupted"));
    assert!(!destination.exists());
}

#[test]
fn selection_exceeds_query_default_and_preview_limit_without_limiting_export() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("root");
    let mut events: Vec<_> = (0..10_001)
        .map(|i| event("large", i, Some(150), Some("/workspace/alpha")))
        .collect();
    events.extend((0..100).map(|i| {
        event(
            &format!("conversation-{i}"),
            i + 10_001,
            Some(150),
            Some("/workspace/alpha"),
        )
    }));
    publish(&root.join("search/lexical"), &events, 1);
    let plan = plan_export(&root, identity(), &Selection::default(), &query()).unwrap();
    let preview = plan.preview();
    assert_eq!(preview.selected_conversations, 101);
    assert_eq!(preview.scope.matched_records, 10_101);
    assert_eq!(preview.conversations.len(), 100);
    assert!(preview.conversations_truncated && preview.selection_complete);
    let receipt = plan.export(&temp.path().join("selected")).unwrap();
    assert_eq!(
        (receipt.manifest.members, receipt.manifest.records),
        (101, 10_101)
    );
}

#[test]
fn mixed_origin_selection_preserves_declared_identity() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("native");
    publish(
        &root.join("search/lexical"),
        &[event("one", 1, Some(150), Some("/workspace/alpha"))],
        1,
    );
    let other = ArchiveIdentity {
        origin: "synthetic-machine-b".into(),
        ..identity()
    };
    let restored = temp.path().join("restored");
    for (name, identity) in [("a", identity()), ("b", other.clone())] {
        let path = temp.path().join(name);
        export_data_root(&root, &path, identity.clone(), &Selection::default()).unwrap();
        let mut options = options();
        options.binding.identity = identity;
        restore(&path, &restored, &options).unwrap();
    }
    let plan = plan_export(&restored, identity(), &Selection::default(), &query()).unwrap();
    assert_eq!(plan.preview().selected_conversations, 1);
    assert_eq!(plan.preview().excluded_identities, vec![other]);
    let output = temp.path().join("selected");
    assert_eq!(plan.export(&output).unwrap().manifest.identity, identity());
    assert_eq!(
        archive_records(&output),
        archive_records(&temp.path().join("a"))
    );
}
