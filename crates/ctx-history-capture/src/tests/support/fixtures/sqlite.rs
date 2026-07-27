use rusqlite::Connection;
use serde_json::json;
use std::path::PathBuf;
use tempfile::TempDir;

pub(in crate::tests) fn write_opencode_smoke_db(temp: &TempDir, malformed: bool) -> PathBuf {
    let path = temp.path().join(if malformed {
        "opencode-malformed.db"
    } else {
        "opencode.db"
    });
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
            "create table session (
                id text primary key, parent_id text, title text not null, directory text not null,
                model text, agent text, time_created integer not null, time_updated integer not null,
                tokens_input integer not null, tokens_output integer not null,
                tokens_reasoning integer not null, tokens_cache_read integer not null,
                tokens_cache_write integer not null
            );
            create table session_message (
                id text primary key, session_id text not null, type text not null, seq integer not null,
                time_created integer not null, time_updated integer not null, data text not null
            );",
        )
        .unwrap();
    conn.execute(
            "insert into session values (?1, null, 'root', '/workspace', '{\"id\":\"test\"}', 'build', 1782259200000, 1782259200000, 1, 1, 0, 0, 0)",
            ["opencode-root"],
        )
        .unwrap();
    conn.execute(
            "insert into session values (?1, ?2, 'child', '/workspace', '{\"id\":\"test\"}', 'scout', 1782259201000, 1782259201000, 1, 1, 0, 0, 0)",
            ["opencode-child", "opencode-root"],
        )
        .unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'user', 1, 1782259200000, 1782259200000, ?3)",
        [
            "msg-user",
            "opencode-root",
            "{\"time\":{\"created\":1782259200000},\"text\":\"inspect\"}",
        ],
    )
    .unwrap();
    conn.execute(
            "insert into session_message values (?1, ?2, 'assistant', 2, 1782259201000, 1782259201000, ?3)",
            ["msg-assistant", "opencode-root", "{\"time\":{\"created\":1782259201000},\"content\":[{\"type\":\"tool\",\"name\":\"bash\"}]}"],
        )
        .unwrap();
    let child_data = if malformed {
        "{\"time\":{\"created\":1782259202000},\"text\":"
    } else {
        "{\"time\":{\"created\":1782259202000},\"text\":\"child done\"}"
    };
    conn.execute(
            "insert into session_message values (?1, ?2, 'assistant', 1, 1782259202000, 1782259202000, ?3)",
            ["msg-child", "opencode-child", child_data],
        )
        .unwrap();
    path
}

pub(in crate::tests) fn write_hermes_smoke_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("hermes-state.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (
                id text primary key,
                source text not null,
                started_at real not null
            );
            create table messages (
                id integer primary key autoincrement,
                session_id text not null,
                role text not null,
                content text,
                timestamp real not null,
                active integer not null default 1,
                compacted integer not null default 0
            );",
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, 'acp', 1782259200.0)",
        ["hermes-root"],
    )
    .unwrap();
    conn.execute(
            "insert into messages (session_id, role, content, timestamp) values (?1, 'user', 'bad timestamp', 1782259201.0)",
            ["hermes-root"],
        )
        .unwrap();
    conn.execute(
            "insert into messages (session_id, role, content, timestamp) values (?1, 'assistant', 'good timestamp', 1782259202.0)",
            ["hermes-root"],
        )
        .unwrap();
    path
}

pub(in crate::tests) fn write_opencode_message_part_db(
    temp: &TempDir,
    name: &str,
    session_id: &str,
    oracle_text: &str,
) -> PathBuf {
    let path = temp.path().join(name);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
                id text primary key,
                project_id text not null,
                parent_id text,
                slug text not null,
                directory text not null,
                title text not null,
                version text not null,
                share_url text,
                summary_additions integer,
                summary_deletions integer,
                summary_files integer,
                summary_diffs text,
                revert text,
                permission text,
                time_created integer not null,
                time_updated integer not null,
                time_compacting integer,
                time_archived integer,
                workspace_id text
            );
            create table message (
                id text primary key,
                session_id text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );
            create table part (
                id text primary key,
                message_id text not null,
                session_id text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );",
    )
    .unwrap();
    conn.execute(
        "insert into session (
                id, project_id, parent_id, slug, directory, title, version, permission,
                time_created, time_updated
            ) values (?1, 'project-1', null, ?1, '/workspace', 'part root', '0.8.0',
                'default', 1782259200000, 1782259200000)",
        [session_id],
    )
    .unwrap();
    conn.execute(
        "insert into message values (?1, ?2, 1782259201000, 1782259201000, ?3)",
        [
            "part-message",
            session_id,
            &json!({
                "role": "assistant",
                "time": { "created": 1782259201000_i64 },
                "providerID": "anthropic",
                "modelID": "claude-sonnet-4"
            })
            .to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into part values (?1, 'part-message', ?2, 1782259201001, 1782259201001, ?3)",
        [
            "part-text",
            session_id,
            &json!({
                "type": "text",
                "text": oracle_text
            })
            .to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into part values (?1, 'part-message', ?2, 1782259201002, 1782259201002, ?3)",
        [
            "part-tool",
            session_id,
            &json!({
                "type": "tool",
                "tool": "write_file",
                "state": {
                    "status": "completed",
                    "metadata": {
                        "exit": 0,
                        "outputPath": "src/tool_arg_should_not_touch.txt",
                        "truncated": false
                    }
                },
                "input": { "path": "src/tool_arg_should_not_touch.txt" }
            })
            .to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into part values (?1, 'part-message', ?2, 1782259201003, 1782259201003, ?3)",
        [
            "part-patch",
            session_id,
            &json!({
                "type": "patch",
                "status": "completed",
                "path": "src/opencode_part.txt",
                "files": ["src/opencode_part_from_files.txt"],
                "patch": "*** Begin Patch\n*** Update File: src/opencode_part.txt\n@@\n-raw-opencode-patch-needle\n+new\n*** End Patch"
            })
            .to_string(),
        ],
    )
    .unwrap();
    path
}

pub(in crate::tests) fn write_shelley_smoke_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("shelley.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
                conversation_id text primary key,
                slug text,
                user_initiated boolean not null default true,
                created_at datetime not null default current_timestamp,
                updated_at datetime not null default current_timestamp,
                cwd text,
                archived boolean not null default false,
                parent_conversation_id text,
                model text,
                conversation_options text not null default '{}',
                current_generation integer not null default 1,
                agent_working boolean not null default false,
                tags text not null default '[]',
                is_draft boolean not null default false,
                draft text not null default '',
                queued_messages text not null default '[]'
            );
            create table messages (
                message_id text primary key,
                conversation_id text not null,
                sequence_id integer not null,
                type text not null,
                llm_data text,
                user_data text,
                usage_data text,
                created_at datetime not null default current_timestamp,
                display_data text,
                excluded_from_context boolean not null default false,
                generation integer not null default 1,
                llm_api_url text,
                model_name text,
                forked_from_message_id text
            );
            create index idx_messages_conversation_sequence
                on messages(conversation_id, sequence_id);",
    )
    .unwrap();
    conn.execute(
            "insert into conversations values (
                'shelley-root', 'root-slug', 1, '2026-06-24 12:00:00',
                '2026-06-24 12:05:00', '/workspace/shelley', 0, null,
                'claude-opus-4-7', ?1, 2, 0, ?2, 0, '', ?3
            )",
            [
                r#"{"thinking_level":"high","subagent_backend":"shelley"}"#,
                r#"["native","ctx"]"#,
                r#"[{"id":"queued-1","llm":{"Content":[{"Type":2,"Text":"queued oracle"}]},"created_at":"2026-06-24T12:00:04Z","model":"claude-opus-4-7"}]"#,
            ],
        )
        .unwrap();
    conn.execute(
        "insert into conversations values (
                'shelley-child', 'child-slug', 0, '2026-06-24 12:01:00',
                '2026-06-24 12:02:00', '/workspace/shelley', 0, 'shelley-root',
                'claude-sonnet-4-5', '{}', 1, 0, '[]', 0, '', '[]'
            )",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
                'shelley-draft', 'old-draft', 1, '2026-06-24 11:00:00',
                '2026-06-24 11:01:00', '/workspace/archive', 1, null,
                null, '{}', 1, 0, '[]', 1, 'draft body', '[]'
            )",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
                message_id, conversation_id, sequence_id, type, user_data, created_at
            ) values ('msg-user', 'shelley-root', 1, 'user', ?1, '2026-06-24 12:00:01')",
        [json!({
            "Content": [
                {"Type": 2, "Text": "please run shelley search oracle"}
            ]
        })
        .to_string()],
    )
    .unwrap();
    conn.execute(
            "insert into messages (
                message_id, conversation_id, sequence_id, type, llm_data, usage_data,
                created_at, generation, llm_api_url, model_name
            ) values (
                'msg-agent', 'shelley-root', 2, 'agent', ?1, ?2,
                '2026-06-24 12:00:02', 2, 'https://api.anthropic.com/v1/messages',
                'claude-opus-4-7'
            )",
            [
                json!({
                    "Role": 1,
                    "Content": [
                        {"Type": 3, "Thinking": "thinking through the search"},
                        {"Type": 2, "Text": "I will inspect the source."},
                        {"Type": 5, "ID": "toolu_1", "ToolName": "bash", "ToolInput": {"command": "rg shelley"}}
                    ],
                    "EndOfTurn": false
                })
                .to_string(),
                json!({
                    "input_tokens": 100,
                    "cache_read_input_tokens": 25,
                    "output_tokens": 40,
                    "cost_usd": 0.0123,
                    "model": "claude-opus-4-7",
                    "url": "https://api.anthropic.com/v1/messages"
                })
                .to_string(),
            ],
        )
        .unwrap();
    conn.execute(
            "insert into messages (
                message_id, conversation_id, sequence_id, type, user_data, display_data,
                created_at, forked_from_message_id
            ) values (
                'msg-tool-result', 'shelley-root', 3, 'user', ?1, ?2,
                '2026-06-24 12:00:03', 'source-msg-tool-result'
            )",
            [
                json!({
                    "Role": 0,
                    "Content": [
                        {"Type": 6, "ToolUseID": "toolu_1", "ToolResult": [{"Type": 2, "Text": "Created commit 0123456789abcdef0123456789abcdef01234567; https://github.com/ctxrs/ctx/pull/123"}]}
                    ]
                })
                .to_string(),
                json!({
                    "stdout": "Created commit 0123456789abcdef0123456789abcdef01234567; https://github.com/ctxrs/ctx/pull/123",
                    "exit_code": 0
                })
                .to_string(),
            ],
        )
        .unwrap();
    conn.execute(
        "insert into messages (
                message_id, conversation_id, sequence_id, type, llm_data, created_at
            ) values ('msg-child', 'shelley-child', 1, 'agent', ?1, '2026-06-24 12:01:01')",
        [json!({
            "Content": [
                {"Type": 2, "Text": "subagent result from Shelley"}
            ]
        })
        .to_string()],
    )
    .unwrap();
    path
}
