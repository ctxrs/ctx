use super::*;

#[test]
fn gemini_retains_messages_invocations_results_and_notices() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({"id":"user-1","type":"user","content":"hello"}),
            json!({"id":"assistant-1","type":"gemini","content":"hi"}),
            json!({"id":"request-1","type":"gemini","toolCalls":[{
                "id":"call-1","name":"write_file","args":{"path":"safe.txt"}
            }]}),
            json!({"id":"result-1","type":"gemini","toolCalls":[{
                "id":"call-1","name":"write_file","result":{
                    "content":"exact output","unknown":{"kept":true},"path":"out.txt"
                }
            }]}),
            json!({"id":"state-1","$set":{"summary":"checkpoint"}}),
        ],
    );
    let source = rediscover(&root, &path);
    let (outcome, rows) = scan_collect(&source, None);

    assert_eq!(rows.len(), 5);
    assert_eq!(
        rows.iter().map(|row| row.event_type).collect::<Vec<_>>(),
        [
            EventType::Message,
            EventType::Message,
            EventType::ToolCall,
            EventType::ToolOutput,
            EventType::Notice,
        ]
    );
    assert!(matches!(
        &rows[3].body,
        GeminiEventBody::ToolResult { native_content, result: Some(result), .. }
            if native_content.pointer("/result/unknown/kept") == Some(&json!(true))
                && result.pointer("/content") == Some(&json!("exact output"))
    ));
    assert_eq!(outcome.metrics.native_result_records_observed, 1);

    let records = project_gemini_test_events(&source, rows).unwrap();
    let invocation = records[2]
        .content
        .activity
        .as_ref()
        .unwrap()
        .invocation
        .as_ref()
        .unwrap();
    assert_eq!(invocation.tool, "write_file");
    assert_eq!(invocation.protocol, None);
    assert_eq!(invocation.server, None);
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: json!({"path":"safe.txt"})
        }
    );
    assert_eq!(
        records[3].content.structured_content.as_ref().unwrap(),
        &json!({
            "id":"call-1","name":"write_file","result":{
                "content":"exact output","unknown":{"kept":true},"path":"out.txt"
            }
        })
    );
}

#[test]
fn gemini_projects_multi_call_invocations_as_ordered_subrecords() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("multi", "main"),
            json!({"id":"record","type":"gemini","toolCalls":[
                {"id":"first","name":"one","args":{"command":" c1 "}},
                {"id":"second","name":"two","args":{"command":" c2 "}}
            ]}),
        ],
    );
    let source = rediscover(&root, &path);
    let (_, rows) = scan_collect(&source, None);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].native_order.sub_ordinal, 0);
    assert_eq!(rows[1].native_order.sub_ordinal, 1);
    assert_ne!(rows[0].identity, rows[1].identity);

    let records = project_gemini_test_events(&source, rows).unwrap();
    assert_eq!(records[0].event_sequence + 1, records[1].event_sequence);
    for (record, id, tool, command) in [
        (&records[0], "first", "one", " c1 "),
        (&records[1], "second", "two", " c2 "),
    ] {
        let activity = record.content.activity.as_ref().unwrap();
        assert_eq!(activity.provider_call_id, Some(TypedKey::utf8(id).unwrap()));
        let invocation = activity.invocation.as_ref().unwrap();
        assert_eq!(invocation.tool, tool);
        assert_eq!(
            invocation.arguments,
            ActivityJsonCapture::Present {
                value: json!({"command":command})
            }
        );
    }
}

#[test]
fn gemini_flattened_mcp_name_is_not_interpreted_but_exact_fields_are() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("mcp", "main"),
            json!({"id":"flat","type":"gemini","toolCalls":[{
                "id":"flat-call","name":"mcp__forge__read","args":{}
            }]}),
            json!({"id":"exact","type":"gemini","toolCalls":[{
                "id":"exact-call","name":"mcp__forge__read","protocol":"mcp",
                "server":"forge","tool":"read","args":{}
            }]}),
        ],
    );
    let source = rediscover(&root, &path);
    let (_, rows) = scan_collect(&source, None);
    let records = project_gemini_test_events(&source, rows).unwrap();
    let flat = records[0]
        .content
        .activity
        .as_ref()
        .unwrap()
        .invocation
        .as_ref()
        .unwrap();
    assert_eq!(flat.tool, "mcp__forge__read");
    assert_eq!(
        (flat.protocol.as_deref(), flat.server.as_deref()),
        (None, None)
    );
    let exact = records[1]
        .content
        .activity
        .as_ref()
        .unwrap()
        .invocation
        .as_ref()
        .unwrap();
    assert_eq!(exact.tool, "read");
    assert_eq!(
        (exact.protocol.as_deref(), exact.server.as_deref()),
        (Some("mcp"), Some("forge"))
    );
}

#[test]
fn gemini_duplicate_selectors_retain_events_and_withhold_affected_channels() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[header("duplicates", "main")]);
    bytes.extend_from_slice(br#"{"id":"record","type":"gemini","toolCalls":[{"id":"call","name":"tool","args":{"path":"one"},"args":{"path":"two"}}]}
"#);
    bytes.extend_from_slice(br#"{"id":"result","type":"gemini","toolCalls":[{"id":"one","id":"two","name":"tool","result":"first"}]}
"#);
    bytes.extend_from_slice(br#"{"id":"invocation-call-id","type":"gemini","toolCalls":[{"id":"one","id":"two","name":"tool","args":{}}]}
"#);
    fs::write(&path, bytes).unwrap();
    let source = rediscover(&root, &path);
    let (_, rows) = scan_collect(&source, None);
    assert_eq!(rows.len(), 3);
    let records = project_gemini_test_events(&source, rows).unwrap();

    let invocation = records[0]
        .content
        .activity
        .as_ref()
        .unwrap()
        .invocation
        .as_ref()
        .unwrap();
    assert_eq!(invocation.arguments, ActivityJsonCapture::Unavailable);
    assert!(records[0].content.structured_content.is_none());
    let result_activity = records[1].content.activity.as_ref().unwrap();
    assert!(result_activity.provider_call_id.is_none());
    assert!(result_activity.result.is_none());
    assert_eq!(records[1].content.normalized_body.as_deref(), Some("first"));
    assert!(records[1].content.structured_content.is_none());
    let invocation_activity = records[2].content.activity.as_ref().unwrap();
    assert!(invocation_activity.provider_call_id.is_none());
    assert!(invocation_activity.invocation.is_none());
    assert!(records[2].content.structured_content.is_none());
}

#[test]
fn gemini_duplicate_record_discriminators_are_rejected_before_classification_or_splitting() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[
        header("record-duplicates", "main"),
        json!({"id":"before","type":"user","content":"before"}),
    ]);
    let duplicate_records = [
        br#"{"sessionId":"first","sessionId":"MUST_NOT_EMIT","kind":"main"}"#.as_slice(),
        br#"{"sessionId":"same","sessionId":"same","kind":"main"}"#.as_slice(),
        br#"{"sessionId":"identity","projectHash":"first","projectHash":"MUST_NOT_EMIT","kind":"main"}"#.as_slice(),
        br#"{"sessionId":"identity","startTime":"first","startTime":"MUST_NOT_EMIT","kind":"main"}"#.as_slice(),
        br#"{"sessionId":"identity","kind":"main","kind":"MUST_NOT_EMIT"}"#.as_slice(),
        br#"{"id":"type-conflict","type":"user","type":"gemini","content":"MUST_NOT_EMIT"}"#.as_slice(),
        br#"{"id":"type-identical","type":"gemini","type":"gemini","content":"MUST_NOT_EMIT"}"#.as_slice(),
        br#"{"id":"calls-conflict","type":"gemini","toolCalls":[{"id":"first","name":"tool","args":{}}],"toolCalls":[{"id":"final","name":"tool","args":{"command":"MUST_NOT_EMIT"}}]}"#.as_slice(),
        br#"{"id":"calls-identical","type":"gemini","toolCalls":[],"toolCalls":[]}"#.as_slice(),
        br#"{"id":"set-conflict","$set":{"summary":"first"},"$set":{"summary":"MUST_NOT_EMIT"}}"#.as_slice(),
        br#"{"id":"set-identical","$set":{"summary":"same"},"$set":{"summary":"same"}}"#.as_slice(),
        br#"{"id":"rewind-conflict","$rewindTo":"first","$rewindTo":"MUST_NOT_EMIT"}"#.as_slice(),
        br#"{"id":"rewind-identical","$rewindTo":"same","$rewindTo":"same"}"#.as_slice(),
        br#"{"id":"result-conflict","result":"first","result":"MUST_NOT_EMIT"}"#.as_slice(),
        br#"{"id":"result-identical","result":"same","result":"same"}"#.as_slice(),
        br#"{"id":"nested-result-conflict","toolCalls":[{"id":"call","result":"first","result":"MUST_NOT_EMIT"}]}"#.as_slice(),
        br#"{"id":"nested-result-identical","toolCalls":[{"id":"call","result":"same","result":"same"}]}"#.as_slice(),
    ];
    for record in duplicate_records {
        bytes.extend_from_slice(record);
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(&jsonl(&[
        json!({"id":"after","type":"gemini","content":"after"}),
    ]));
    fs::write(&path, bytes).unwrap();

    let source = rediscover(&root, &path);
    let (outcome, rows) = scan_collect(&source, None);
    assert_eq!(outcome.rejected_records, duplicate_records.len() as u64);
    assert_eq!(rows.len(), 2);
    let serialized = serde_json::to_string(&rows).unwrap();
    assert!(serialized.contains("before"));
    assert!(serialized.contains("after"));
    assert!(!serialized.contains("MUST_NOT_EMIT"));
}

#[test]
fn gemini_duplicate_record_ids_do_not_poison_following_valid_same_id_records() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[header("duplicate-identity", "main")]);
    for record in [
        br#"{"id":"first","id":"shared-conflict","type":"gemini","content":"MUST_NOT_EMIT_CONFLICT"}"#.as_slice(),
        br#"{"id":"shared-conflict","type":"gemini","content":"valid-after-conflict"}"#.as_slice(),
        br#"{"id":"shared-identical","id":"shared-identical","type":"gemini","content":"MUST_NOT_EMIT_IDENTICAL"}"#.as_slice(),
        br#"{"id":"shared-identical","type":"gemini","content":"valid-after-identical"}"#.as_slice(),
    ] {
        bytes.extend_from_slice(record);
        bytes.push(b'\n');
    }
    fs::write(&path, bytes).unwrap();

    let source = rediscover(&root, &path);
    let (outcome, rows) = scan_collect(&source, None);
    assert_eq!(outcome.rejected_records, 2);
    assert_eq!(rows.len(), 2);
    assert!(matches!(
        &rows[0].identity,
        GeminiEventIdentity::NativeRecordId(id) if id == "shared-conflict"
    ));
    assert!(matches!(
        &rows[1].identity,
        GeminiEventIdentity::NativeRecordId(id) if id == "shared-identical"
    ));
    let serialized = serde_json::to_string(&rows).unwrap();
    assert!(serialized.contains("valid-after-conflict"));
    assert!(serialized.contains("valid-after-identical"));
    assert!(!serialized.contains("MUST_NOT_EMIT"));
}

#[test]
fn gemini_duplicate_tool_call_and_result_timestamps_are_rejected() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[header("duplicate-time", "main")]);
    for record in [
        br#"{"id":"call-time-conflict","timestamp":"2026-08-16T12:00:00Z","timestamp":"2026-08-16T13:00:00Z","type":"gemini","toolCalls":[{"id":"call","name":"tool","args":{"command":"MUST_NOT_EMIT"}}]}"#.as_slice(),
        br#"{"id":"call-time-identical","timestamp":"2026-08-16T12:00:00Z","timestamp":"2026-08-16T12:00:00Z","type":"gemini","toolCalls":[{"id":"call","name":"tool","args":{"command":"MUST_NOT_EMIT"}}]}"#.as_slice(),
        br#"{"id":"result-time-conflict","timestamp":"2026-08-16T12:00:00Z","timestamp":"2026-08-16T13:00:00Z","toolCalls":[{"id":"call","result":"MUST_NOT_EMIT"}]}"#.as_slice(),
        br#"{"id":"result-time-identical","timestamp":"2026-08-16T12:00:00Z","timestamp":"2026-08-16T12:00:00Z","toolCalls":[{"id":"call","result":"MUST_NOT_EMIT"}]}"#.as_slice(),
    ] {
        bytes.extend_from_slice(record);
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(&jsonl(&[
        json!({"id":"after","type":"gemini","content":"valid-after-times"}),
    ]));
    fs::write(&path, bytes).unwrap();

    let source = rediscover(&root, &path);
    let (outcome, rows) = scan_collect(&source, None);
    assert_eq!(outcome.rejected_records, 4);
    assert_eq!(rows.len(), 1);
    let serialized = serde_json::to_string(&rows).unwrap();
    assert!(serialized.contains("valid-after-times"));
    assert!(!serialized.contains("MUST_NOT_EMIT"));
}

#[test]
fn gemini_literal_facts_preserve_raw_order_and_duplicates_abstain() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[header("facts", "main")]);
    bytes.extend_from_slice(br#"{"id":"ordered","type":"gemini","toolCalls":[{"id":"call","name":"tool","args":{"command":" c ","path":" p ","url":" u ","path_list":["ignored"]}}]}
"#);
    bytes.extend_from_slice(br#"{"id":"duplicate","type":"gemini","toolCalls":[{"id":"dup","name":"tool","args":{"path":"one","path":"two"}}]}
"#);
    fs::write(&path, bytes).unwrap();
    let source = rediscover(&root, &path);
    let (_, rows) = scan_collect(&source, None);
    let records = project_gemini_test_events(&source, rows).unwrap();
    let facts = &records[0].content.activity.as_ref().unwrap().facts;
    assert_eq!(
        facts
            .iter()
            .map(|fact| (fact.kind, fact.value.as_str()))
            .collect::<Vec<_>>(),
        [
            (LiteralFactKind::SessionCwd, "/workspace/project"),
            (LiteralFactKind::Command, " c "),
            (LiteralFactKind::File, " p "),
            (LiteralFactKind::Url, " u "),
        ]
    );
    assert_eq!(
        records[1].content.activity.as_ref().unwrap().facts,
        [ctx_history_core::ProviderDeclaredFact {
            kind: LiteralFactKind::SessionCwd,
            value: "/workspace/project".to_owned(),
        }]
    );
}

#[test]
fn gemini_malformed_record_is_local_and_incomplete_tail_is_nonterminal() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[
        header("malformed", "main"),
        json!({"id":"before","type":"user","content":"before"}),
    ]);
    bytes.extend_from_slice(b"{not-json}\n");
    bytes.extend_from_slice(&jsonl(&[
        json!({"id":"after","type":"gemini","content":"after"}),
    ]));
    let complete_prefix_end = bytes.len() as u64;
    bytes.extend_from_slice(br#"{"id":"partial","type":"gemini","content":"unfinished"#);
    fs::write(&path, bytes).unwrap();
    let source = rediscover(&root, &path);
    let (outcome, rows) = scan_collect(&source, None);
    assert_eq!(rows.len(), 2);
    assert_eq!(outcome.rejected_records, 1);
    assert_eq!(outcome.checkpoint.complete_prefix_end, complete_prefix_end);
    assert!(!outcome.checkpoint.terminal);
}

#[test]
fn gemini_incomplete_append_resumes_at_exact_boundary() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("append", "main"),
            json!({"id":"one","type":"user","content":"one"}),
        ],
    );
    let source = rediscover(&root, &path);
    let (baseline, _) = scan_collect(&source, None);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(br#"{"id":"two","type":"gemini","content":"tw"#)
        .unwrap();
    drop(file);
    let source = rediscover(&root, &path);
    let (partial, rows) = scan_collect(&source, Some(&previous(&baseline, true)));
    assert!(rows.is_empty());
    assert_eq!(
        partial.checkpoint.complete_prefix_end,
        baseline.checkpoint.complete_prefix_end
    );
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"o\"}\n").unwrap();
    drop(file);
    let source = rediscover(&root, &path);
    let (_, rows) = scan_collect(&source, Some(&previous(&partial, true)));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].native_order.raw_ordinal, 2);
}
