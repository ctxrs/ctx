use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, CORE_ACTIVITY_REVISION, CoreActivity, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, SessionIdentityInput, SourceAnchor,
    SourceKey, TypedKey, derive_event_id, derive_session_id,
};
use serde_json::{Value, json};

use super::*;

fn record(
    provider: &str,
    event_sequence: u64,
    tool: &str,
    arguments: Value,
    normalized_body: String,
) -> CoreRecord {
    let source = SourceKey::derive(
        provider,
        format!("{provider}-test"),
        format!("{provider}-test-v1"),
        1,
        SourceAnchor::provider_native(
            format!("{provider}.test"),
            TypedKey::utf8("source").expect("source key"),
        )
        .expect("source anchor"),
    )
    .expect("source");
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &NativeSessionKey::native_id(
            "session",
            TypedKey::utf8("session").expect("session key"),
        )
        .expect("native session"),
    })
    .expect("session");
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "tool_call",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(event_sequence))
            .expect("native event"),
        subrecord_selector: None,
    })
    .expect("event");
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source,
        event_sequence,
        "tool_call",
        "adapter-file-invocation-test-v1",
        normalized_body,
    )
    .expect("record");
    record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8(format!("call-{event_sequence}")).expect("call id")),
        invocation: Some(ActivityInvocation {
            protocol: None,
            server: None,
            tool: tool.to_owned(),
            arguments: ActivityJsonCapture::Present { value: arguments },
            started_at_unix_ms: None,
        }),
        result: None,
        facts: Vec::new(),
    });
    record
}

fn extraction(record: &CoreRecord) -> FileInvocationExtraction {
    exact_file_invocations(
        record,
        record
            .content
            .activity
            .as_ref()
            .and_then(|activity| activity.invocation.as_ref())
            .expect("test invocation"),
    )
}

#[test]
fn released_provider_forms_preserve_exact_names_paths_kinds_and_ordinals() {
    let claude_args = json!({"old_path": "src/old.rs", "new_path": "src/new.rs"});
    let claude = record(
        "claude",
        (8 << 16) + 7,
        "Rename",
        claude_args.clone(),
        serde_json::to_string(&claude_args).expect("arguments"),
    );
    let FileInvocationExtraction::Exact(claude) = extraction(&claude) else {
        panic!("expected Claude invocation");
    };
    assert_eq!(claude.len(), 1);
    assert_eq!(claude[0].operation_ordinal, 7);
    assert_eq!(claude[0].tool_name.as_deref(), Some("Rename"));
    assert_eq!(claude[0].path, "src/new.rs");
    assert_eq!(claude[0].prior_path.as_deref(), Some("src/old.rs"));
    assert_eq!(claude[0].kind, RepositoryFileInvocationKind::Rename);
    assert_eq!(claude[0].normalized_text_range.unwrap().start, 0);

    let cursor_args = json!({"paths": ["src/a.rs", "src/b.rs"], "contents": "exact"});
    let cursor_unit = serde_json::to_string(&cursor_args).expect("arguments");
    let cursor = record(
        "cursor",
        (4 << 16) + 3,
        "write_file",
        cursor_args,
        format!("{{\"input\":{cursor_unit},\"type\":\"tool_use\"}}"),
    );
    let FileInvocationExtraction::Exact(cursor) = extraction(&cursor) else {
        panic!("expected Cursor invocations");
    };
    assert_eq!(cursor.len(), 2);
    assert!(cursor.iter().all(|item| {
        item.operation_ordinal == 3
            && item.tool_name.as_deref() == Some("write_file")
            && item.kind == RepositoryFileInvocationKind::Write
            && item.normalized_text_range.is_some()
    }));

    let gemini_args = json!({
        "file_path": "src/gemini.rs",
        "old_string": "old",
        "new_string": "new"
    });
    let gemini_unit = serde_json::to_string(&gemini_args).expect("arguments");
    let gemini = record(
        "gemini",
        (9_u64 << 32) + 2,
        "replace",
        gemini_args,
        format!("replace\n{gemini_unit}"),
    );
    let FileInvocationExtraction::Exact(gemini) = extraction(&gemini) else {
        panic!("expected Gemini invocation");
    };
    assert_eq!(gemini[0].operation_ordinal, 2);
    assert_eq!(gemini[0].path, "src/gemini.rs");
    assert_eq!(gemini[0].kind, RepositoryFileInvocationKind::Modify);

    let claw_args = json!({"oldPath": "src/before.rs", "newPath": "src/after.rs"});
    let claw_unit = serde_json::to_string(&claw_args).expect("arguments");
    let openclaw = record(
        "openclaw",
        (5_u64 << 32) + 4,
        "rename_file",
        claw_args,
        format!("{{\"type\":\"toolCall\",\"arguments\":{claw_unit}}}"),
    );
    let FileInvocationExtraction::Exact(openclaw) = extraction(&openclaw) else {
        panic!("expected OpenClaw invocation");
    };
    assert_eq!(openclaw[0].operation_ordinal, 4);
    assert_eq!(openclaw[0].path, "src/after.rs");
    assert_eq!(openclaw[0].prior_path.as_deref(), Some("src/before.rs"));

    let opencode = record(
        "opencode",
        99,
        "write_file",
        json!({"path": "src/opencode.rs", "content": "exact"}),
        "tool call: write_file".to_owned(),
    );
    let FileInvocationExtraction::Exact(opencode) = extraction(&opencode) else {
        panic!("expected OpenCode invocation");
    };
    assert_eq!(opencode[0].operation_ordinal, 0);
    assert_eq!(opencode[0].path, "src/opencode.rs");
    assert_eq!(opencode[0].kind, RepositoryFileInvocationKind::Write);
    assert_eq!(opencode[0].normalized_text_range, None);
}

#[test]
fn codex_native_file_patch_and_nested_exec_forms_are_exact() {
    let rename_object = json!({"prior_path": "src/old.rs", "path": "src/new.rs"});
    let rename_unit = serde_json::to_string(&rename_object).expect("arguments");
    let native = record(
        "codex",
        10,
        "rename",
        Value::String(rename_unit.clone()),
        format!("rename: {rename_unit}"),
    );
    let FileInvocationExtraction::Exact(native) = extraction(&native) else {
        panic!("expected Codex native invocation");
    };
    assert_eq!(native[0].operation_ordinal, 0);
    assert_eq!(native[0].kind, RepositoryFileInvocationKind::Rename);
    assert_eq!(native[0].path, "src/new.rs");
    assert_eq!(native[0].prior_path.as_deref(), Some("src/old.rs"));
    assert!(native[0].normalized_text_range.is_some());

    let patch = concat!(
        "*** Begin Patch\n",
        "*** Add File: src/new.rs\n",
        "+new\n",
        "*** Update File: src/old.rs\n",
        "*** Move to: src/moved.rs\n",
        "@@\n",
        "-old\n",
        "+moved\n",
        "*** Delete File: src/gone.rs\n",
        "*** End Patch"
    );
    let direct = record(
        "codex",
        11,
        "apply_patch",
        Value::String(patch.to_owned()),
        format!("apply_patch: {patch}"),
    );
    let FileInvocationExtraction::Exact(direct) = extraction(&direct) else {
        panic!("expected Codex patch invocations");
    };
    assert_eq!(direct.len(), 3);
    assert_eq!(
        direct
            .iter()
            .map(|item| (item.operation_ordinal, item.kind, item.path.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (0, RepositoryFileInvocationKind::Create, "src/new.rs"),
            (1, RepositoryFileInvocationKind::Rename, "src/moved.rs"),
            (2, RepositoryFileInvocationKind::Delete, "src/gone.rs"),
        ]
    );
    assert_eq!(direct[1].prior_path.as_deref(), Some("src/old.rs"));
    assert!(
        direct
            .iter()
            .all(|item| item.normalized_text_range.is_some())
    );

    let patch_literal = serde_json::to_string(patch).expect("patch literal");
    let source = format!(
        "const action = 'status';\n\
         const patch = {patch_literal};\n\
         const pending = [\n\
           tools.exec_command({{cmd: `git ${{action}}`}}),\n\
           tools.apply_patch(patch),\n\
         ];\n\
         const results = await Promise.all(pending);\n\
         text(JSON.stringify(results, null, 2));"
    );
    let nested = record(
        "codex",
        12,
        "exec",
        Value::String(source),
        "nested exec".to_owned(),
    );
    let FileInvocationExtraction::Exact(nested) = extraction(&nested) else {
        panic!("expected nested Codex patch invocations");
    };
    assert_eq!(nested.len(), 3);
    assert_eq!(nested[0].operation_ordinal, 1);
    assert!(
        nested
            .iter()
            .all(|item| item.normalized_text_range.is_none())
    );
}

#[test]
fn ambiguous_malformed_oversized_and_generic_near_misses_fail_closed() {
    let near_misses = [
        record(
            "gemini",
            1,
            "custom_writer",
            json!({"file_path": "src/no.rs"}),
            "custom".to_owned(),
        ),
        record(
            "claude",
            2,
            "Read",
            json!({"path": "src/a.rs", "file_path": "src/b.rs"}),
            "ambiguous".to_owned(),
        ),
        record(
            "cursor",
            3,
            "write_file",
            json!({"paths": ["src/a.rs", "src/a.rs"]}),
            "duplicate".to_owned(),
        ),
        record(
            "opencode",
            4,
            "read_file",
            json!({"path": "x".repeat(MAX_PATH_BYTES + 1)}),
            "oversized".to_owned(),
        ),
        record(
            "codex",
            5,
            "read",
            Value::String("{not-json".to_owned()),
            "malformed".to_owned(),
        ),
    ];
    for near_miss in &near_misses {
        assert_eq!(extraction(near_miss), FileInvocationExtraction::Rejected);
    }

    let mut mcp = record(
        "openclaw",
        6,
        "read_file",
        json!({"path": "src/no.rs"}),
        "mcp".to_owned(),
    );
    mcp.content
        .activity
        .as_mut()
        .and_then(|activity| activity.invocation.as_mut())
        .expect("invocation")
        .protocol = Some("mcp".to_owned());
    assert_eq!(extraction(&mcp), FileInvocationExtraction::Rejected);

    let unsupported = record(
        "pi",
        7,
        "read_file",
        json!({"path": "src/no.rs"}),
        "unsupported provider".to_owned(),
    );
    assert_eq!(
        extraction(&unsupported),
        FileInvocationExtraction::NotApplicable
    );
}

#[test]
fn normalized_range_requires_one_complete_exact_argument_unit() {
    let arguments = json!({"file_path": "src/exact.rs"});
    let unit = serde_json::to_string(&arguments).expect("arguments");
    let exact = record(
        "claude",
        1,
        "Read",
        arguments.clone(),
        format!("prefix\n{unit}\nsuffix"),
    );
    let FileInvocationExtraction::Exact(exact) = extraction(&exact) else {
        panic!("expected exact invocation");
    };
    let range = exact[0].normalized_text_range.expect("exact range");
    assert_eq!(
        &format!("prefix\n{unit}\nsuffix")[range.start as usize..range.end as usize],
        unit
    );

    let ambiguous = record("claude", 2, "Read", arguments, format!("{unit}\n{unit}"));
    let FileInvocationExtraction::Exact(ambiguous) = extraction(&ambiguous) else {
        panic!("expected invocation without range");
    };
    assert_eq!(ambiguous[0].normalized_text_range, None);
}
