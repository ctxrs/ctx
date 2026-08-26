use super::*;

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn root_scope_distinguishes_native_tasks_and_unqualified_is_unchanged() {
        for dialect in [TaskJsonNativeDialect::CLINE, TaskJsonNativeDialect::ROO] {
            let legacy = SourceKey::derive_provider_native(
                dialect.provider.as_str(),
                dialect.source_format,
                SOURCE_SCHEMA_VARIANT,
                1,
                SOURCE_ANCHOR_NAMESPACE,
                TypedKey::utf8("same-native-task").unwrap(),
            )
            .unwrap();
            let unqualified = task_source_key_for_id_scoped(
                dialect,
                "same-native-task",
                SourceAnchorScope::Unqualified,
            )
            .unwrap();
            let first = task_source_key_for_id_scoped(
                dialect,
                "same-native-task",
                SourceAnchorScope::Lineage([1; 32]),
            )
            .unwrap();
            let second = task_source_key_for_id_scoped(
                dialect,
                "same-native-task",
                SourceAnchorScope::Lineage([2; 32]),
            )
            .unwrap();

            assert!(legacy.exact_descriptor_eq(&unqualified));
            assert_ne!(first.identity(), second.identity());
            assert_ne!(
                derive_task_session_id(&first, "same-native-task").unwrap(),
                derive_task_session_id(&second, "same-native-task").unwrap()
            );
        }
    }
}

#[cfg(test)]
mod replay_tests {
    use super::*;
    use ctx_history_core::core_record_leaf_sha256;

    use super::super::super::normalize::{
        ClineEventComponent, ClineEventContext, ClineNativeItemKey, ClineSourceRecordEvidence,
        ClineTaskIdentity,
    };

    fn project_replay_record(dialect: TaskJsonNativeDialect, body: &str) -> CoreRecord {
        let source = SourceKey::derive(
            dialect.provider.as_str(),
            dialect.source_format,
            SOURCE_SCHEMA_VARIANT,
            1,
            SourceAnchor::provider_native(
                SOURCE_ANCHOR_NAMESPACE,
                TypedKey::utf8("task-json-replay-task").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let task = ClineTaskIdentity::new("task-json-replay-task");
        let item = ClineNativeItemKey::NativeId {
            native_id: "task-json-replay-event".into(),
            occurrence: 0,
        };
        let mut event = ClineEventRow::message(
            ClineEventContext {
                task: &task,
                component: ClineEventComponent::ApiHistory,
                item: &item,
                item_index: 3,
                role: ClineEventRole::Assistant,
                occurred_at_millis: Some(1_754_227_696_000),
            },
            0,
            ClineEventKind::Message,
            body.to_owned(),
        );
        event.source_record = Some(ClineSourceRecordEvidence {
            native_index: 3,
            byte_start: 128,
            byte_length: 64,
            record_digest: [0x3c; 32],
        });
        let session_id = derive_task_session_id(&source, task.as_str()).unwrap();
        project_event(
            dialect,
            &source,
            [0xa5; 32],
            session_id,
            task.as_str(),
            Some("/workspace/replay"),
            event,
        )
        .unwrap()
    }

    #[test]
    fn cline_and_roo_replay_preserve_current_revision_ids_and_records() {
        let cases = [
            (
                TaskJsonNativeDialect::CLINE,
                "f29a3a4b-8b02-8b15-ad30-22b8d3e245e5",
                "985dcf50-7cf6-85de-87cb-79a03269ff1e",
                "13ba5b2e-3b34-8fbd-97c7-6647718d8504",
                "04e0e12bcb5b23989cb3f80d920a12a6c6264fc3f3155c0eb44dc9dabd660439",
            ),
            (
                TaskJsonNativeDialect::ROO,
                "095b0fe0-c153-8364-b970-22637e99ce3e",
                "15349de7-8b56-8e85-b075-3a9d9e01d7a1",
                "7e7f3701-2c21-83b6-b6df-d9e4a7a4d805",
                "8d05f2a036adc00b43680ad9bb71da25c872760c3c18284b5af75d7ddd7f0ab2",
            ),
        ];
        for (dialect, event_id, session_id, source_id, record_leaf) in cases {
            assert_eq!(
                dialect.parser_revision,
                "task-json-source-backed-v6-closed-facts-agent-scope"
            );
            let initial = project_replay_record(dialect, "task-json replay body");
            let replay = project_replay_record(dialect, "task-json replay body");
            assert_eq!(replay, initial);
            assert_eq!(initial.agent_scope, Some(AgentScope::Primary));
            assert_eq!(
                initial.parser_revision,
                "task-json-source-backed-v6-closed-facts-agent-scope"
            );
            assert_eq!(initial.event_id.to_string(), event_id);
            assert_eq!(initial.session_id.to_string(), session_id);
            assert_eq!(initial.source.identity().to_string(), source_id);
            assert_eq!(
                core_record_leaf_sha256(&initial).unwrap(),
                record_leaf,
                "{:?}",
                dialect.provider
            );

            let replacement = project_replay_record(dialect, "task-json replacement body");
            assert_eq!(replacement.event_id, initial.event_id);
            assert_eq!(replacement.session_id, initial.session_id);
            assert_eq!(replacement.native_event_id, initial.native_event_id);
            assert_eq!(replacement.parser_revision, dialect.parser_revision);
            assert_eq!(
                replacement.content.meaningful_text(),
                "task-json replacement body"
            );
            assert_ne!(
                core_record_leaf_sha256(&replacement).unwrap(),
                core_record_leaf_sha256(&initial).unwrap()
            );
        }
    }

    #[test]
    fn cline_and_roo_conflicting_argument_aliases_are_explicitly_unavailable() {
        for dialect in [TaskJsonNativeDialect::CLINE, TaskJsonNativeDialect::ROO] {
            let source = SourceKey::derive(
                dialect.provider.as_str(),
                dialect.source_format,
                SOURCE_SCHEMA_VARIANT,
                1,
                SourceAnchor::provider_native(
                    SOURCE_ANCHOR_NAMESPACE,
                    TypedKey::utf8("task-json-alias-task").unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
            let task = ClineTaskIdentity::new("task-json-alias-task");
            let item = ClineNativeItemKey::NativeId {
                native_id: "task-json-alias-event".into(),
                occurrence: 0,
            };
            let mut event = ClineEventRow::tool_call(
                ClineEventContext {
                    task: &task,
                    component: ClineEventComponent::ApiHistory,
                    item: &item,
                    item_index: 0,
                    role: ClineEventRole::Assistant,
                    occurred_at_millis: None,
                },
                0,
                Some("call-1".to_owned()),
                Some("exact_tool".to_owned()),
                ActivityJsonCapture::Unavailable,
            );
            event.source_record = Some(ClineSourceRecordEvidence {
                native_index: 0,
                byte_start: 0,
                byte_length: 1,
                record_digest: [0x11; 32],
            });
            event.structured_content = serde_json::json!({
                "input": {"x": 1},
                "arguments": {"x": 2},
            });
            let session_id = derive_task_session_id(&source, task.as_str()).unwrap();
            let record = project_event(
                dialect,
                &source,
                [0x22; 32],
                session_id,
                task.as_str(),
                None,
                event,
            )
            .unwrap();
            assert_eq!(
                record
                    .content
                    .activity
                    .as_ref()
                    .and_then(|activity| activity.invocation.as_ref())
                    .unwrap()
                    .arguments,
                ActivityJsonCapture::Unavailable
            );
        }
    }

    #[test]
    fn cline_and_roo_nested_metadata_keys_never_escape_into_facts() {
        for dialect in [TaskJsonNativeDialect::CLINE, TaskJsonNativeDialect::ROO] {
            let source = SourceKey::derive(
                dialect.provider.as_str(),
                dialect.source_format,
                SOURCE_SCHEMA_VARIANT,
                1,
                SourceAnchor::provider_native(
                    SOURCE_ANCHOR_NAMESPACE,
                    TypedKey::utf8("task-json-closed-facts-task").unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
            let task = ClineTaskIdentity::new("task-json-closed-facts-task");
            let item = ClineNativeItemKey::NativeId {
                native_id: "task-json-closed-facts-event".into(),
                occurrence: 0,
            };
            let mut event = ClineEventRow::message(
                ClineEventContext {
                    task: &task,
                    component: ClineEventComponent::ApiHistory,
                    item: &item,
                    item_index: 0,
                    role: ClineEventRole::Assistant,
                    occurred_at_millis: None,
                },
                0,
                ClineEventKind::Message,
                "exact task JSON body".to_owned(),
            );
            event.source_record = Some(ClineSourceRecordEvidence {
                native_index: 0,
                byte_start: 0,
                byte_length: 1,
                record_digest: [0x33; 32],
            });
            event.structured_content = serde_json::json!({
                "content": "exact task JSON body",
                "metadata": {
                    "path": "src/task-json-decoy.rs",
                    "nested": {
                        "branch": "decoy-branch",
                        "commit": "decoy-commit",
                        "command": "decoy-command"
                    }
                }
            });
            let session_id = derive_task_session_id(&source, task.as_str()).unwrap();
            let record = project_event(
                dialect,
                &source,
                [0x44; 32],
                session_id,
                task.as_str(),
                Some("/schema-known-workspace"),
                event,
            )
            .unwrap();
            let facts = &record.content.activity.as_ref().unwrap().facts;
            assert_eq!(facts.len(), 1);
            assert_eq!(facts[0].kind, LiteralFactKind::SessionCwd);
            assert_eq!(facts[0].value, "/schema-known-workspace");
        }
    }
}
