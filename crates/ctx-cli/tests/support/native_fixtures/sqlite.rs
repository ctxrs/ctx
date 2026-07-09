use rusqlite::{params, Connection};
use serde_json::json;
use std::{fs, path::Path};
use tempfile::TempDir;

use crate::support::{provider_history_fixture, sqlite_column_text};

pub(crate) fn write_native_opencode_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-opencode.db");
    write_opencode_family_sqlite_fixture(
        &path,
        query,
        "opencode-cli-native",
        "opencode",
        "opencode-test",
        "OpenCode assistant response",
        "src/opencode_native.rs",
    );
    path.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_mimocode_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-mimocode.db");
    write_mimocode_sqlite_fixture(&path, query, "mimocode-cli-native");
    path.to_str().unwrap().to_owned()
}

pub(crate) fn write_mimocode_sqlite_fixture(path: &Path, query: &str, session_id: &str) {
    write_opencode_family_sqlite_fixture(
        path,
        query,
        session_id,
        "mimo",
        "mimo-code-test",
        "MiMo Code assistant response",
        "src/mimocode_native.rs",
    );
}

fn write_opencode_family_sqlite_fixture(
    path: &Path,
    query: &str,
    session_id: &str,
    provider_id: &str,
    model_id: &str,
    assistant_prefix: &str,
    output_path: &str,
) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let conn = Connection::open(path).unwrap();
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
        ) values (?1, 'project-1', null, 'native', '/workspace', 'native', '0.8.0',
            'default', 1782259200000, 1782259200000)",
        [session_id],
    )
    .unwrap();
    let child_session_id = format!("{session_id}-child");
    conn.execute(
        "insert into session (
            id, project_id, parent_id, slug, directory, title, version, permission,
            time_created, time_updated
        ) values (?1, 'project-1', ?2, 'native-child', '/workspace', 'native child', '0.8.0',
            'default', 1782259202000, 1782259202000)",
        params![child_session_id, session_id],
    )
    .unwrap();
    conn.execute(
        "insert into message values (?1, ?2, 1782259200000, 1782259200000, ?3)",
        params![
            format!("{session_id}-user"),
            session_id,
            json!({
                "role": "user",
                "time": { "created": 1782259200000_i64 },
                "text": query
            })
            .to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into message values (?1, ?2, 1782259201000, 1782259201000, ?3)",
        params![
            format!("{session_id}-assistant"),
            session_id,
            json!({
                "role": "assistant",
                "time": { "created": 1782259201000_i64 },
                "providerID": provider_id,
                "modelID": model_id
            })
            .to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into part values (?1, ?2, ?3, 1782259201001, 1782259201001, ?4)",
        params![
            format!("{session_id}-assistant-text"),
            format!("{session_id}-assistant"),
            session_id,
            json!({
                "type": "text",
                "text": format!("{assistant_prefix} for {query}")
            })
            .to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into part values (?1, ?2, ?3, 1782259201002, 1782259201002, ?4)",
        params![
            format!("{session_id}-assistant-tool"),
            format!("{session_id}-assistant"),
            session_id,
            json!({
                "type": "tool",
                "tool": "write_file",
                "state": {
                    "status": "completed",
                    "metadata": {
                        "outputPath": output_path,
                        "exit": 0
                    }
                },
                "input": { "path": "src/tool_arg_should_not_touch.txt" }
            })
            .to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into message values (?1, ?2, 1782259202000, 1782259202000, ?3)",
        params![
            format!("{session_id}-child-message"),
            child_session_id,
            json!({
                "role": "assistant",
                "time": { "created": 1782259202000_i64 },
                "text": "MiMo child session answer"
            })
            .to_string(),
        ],
    )
    .unwrap();
}

pub(crate) fn write_native_kilo_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-kilo.db");
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
            model text,
            agent text,
            cost real not null default 0,
            tokens_input integer not null default 0,
            tokens_output integer not null default 0,
            tokens_reasoning integer not null default 0,
            tokens_cache_read integer not null default 0,
            tokens_cache_write integer not null default 0,
            time_created integer not null,
            time_updated integer not null
        );
        create table session_message (
            id text primary key,
            session_id text not null,
            type text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null
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
        );
        create table todo (
            session_id text not null,
            content text not null,
            status text not null,
            priority text not null,
            position integer not null,
            time_created integer not null,
            time_updated integer not null
        );
        create table permission (
            project_id text primary key,
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into session (
            id, project_id, parent_id, slug, directory, title, version, model, agent,
            time_created, time_updated
        ) values (?1, 'project-1', null, 'native', '/workspace', 'native', '0.8.0',
            '{\"id\":\"kilo-auto/free\",\"providerID\":\"kilo\"}', 'build',
            1782259200000, 1782259200000)",
        ["kilo-cli-native"],
    )
    .unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'user', 1782259200000, 1782259200000, ?3)",
        [
            "kilo-cli-native-user",
            "kilo-cli-native",
            &format!(r#"{{"time":{{"created":1782259200000}},"text":"{query}"}}"#),
        ],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_kiro_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-kiro.sqlite3");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
            key text primary key,
            value text
        );
        create table conversations_v2 (
            key text not null,
            conversation_id text not null,
            value text not null,
            created_at integer not null,
            updated_at integer not null,
            primary key (key, conversation_id)
        );
        create index idx_conversations_v2_key_updated on conversations_v2(key, updated_at desc);
        create index idx_conversations_v2_updated_at on conversations_v2(updated_at desc);",
    )
    .unwrap();
    let value = json!({
        "conversation_id": "kiro-cli-native",
        "history": [
            {
                "user": {
                    "timestamp": "2026-06-25T20:10:00Z",
                    "content": {
                        "Prompt": {
                            "prompt": query,
                        },
                    },
                },
                "assistant": {
                    "timestamp": "2026-06-25T20:10:03Z",
                    "Response": {
                        "content": format!("Kiro CLI response for {query}"),
                    },
                },
            },
            {
                "assistant": {
                    "timestamp": "2026-06-25T20:10:05Z",
                    "ToolUse": {
                        "content": "Inspecting Kiro CLI fixture state.",
                        "tool_uses": [
                            {
                                "id": "toolu_kiro_cli_native_1",
                                "name": "grep",
                                "args": {
                                    "pattern": query,
                                    "path": "/workspace/kiro-cli-native",
                                },
                            },
                        ],
                    },
                },
            },
        ],
    });
    conn.execute(
        "insert into conversations_v2 (key, conversation_id, value, created_at, updated_at)
         values ('/workspace/kiro-cli-native', 'kiro-cli-native', ?1, 1782418200000, 1782418205000)",
        [value.to_string()],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_forgecode_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-forgecode.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE conversations (
            conversation_id TEXT PRIMARY KEY NOT NULL,
            title TEXT,
            workspace_id BIGINT NOT NULL,
            context TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP,
            metrics TEXT
        );",
    )
    .unwrap();
    let context = json!({
        "conversation_id": "forgecode-cli-native",
        "initiator": "forgecode",
        "messages": [
            {
                "message": {
                    "text": {
                        "role": "User",
                        "content": query
                    }
                }
            },
            {
                "message": {
                    "text": {
                        "role": "Assistant",
                        "content": "forgecode native import ok",
                        "tool_calls": [{
                            "name": "write",
                            "call_id": "call-forgecode-cli",
                            "arguments": {
                                "path": "src/forgecode_cli_native.rs",
                                "content": "proof"
                            }
                        }],
                        "model": "forge/test-model"
                    }
                }
            },
            {
                "message": {
                    "tool": {
                        "name": "write",
                        "call_id": "call-forgecode-cli",
                        "output": {
                            "is_error": false,
                            "values": [{"text": "wrote src/forgecode_cli_native.rs"}]
                        }
                    }
                }
            }
        ],
        "tools": [{"name": "write", "input_schema": {"type": "object"}}],
        "tool_choice": {"Call": "write"},
        "stream": true
    });
    let metrics = json!({
        "started_at": "2026-06-24T12:00:01Z",
        "files_changed": {
            "src/forgecode_cli_native.rs": {
                "lines_added": 1,
                "lines_removed": 0,
                "tool": "write",
                "content_hash": "cli-fixture"
            }
        },
        "files_accessed": ["src/forgecode_cli_input.rs"]
    });
    conn.execute(
        "INSERT INTO conversations (
            conversation_id, title, workspace_id, context, created_at, updated_at, metrics
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            "forgecode-cli-native",
            "ForgeCode CLI native",
            42_i64,
            serde_json::to_string(&context).unwrap(),
            "2026-06-24 12:00:00",
            "2026-06-24 12:00:03",
            serde_json::to_string(&metrics).unwrap()
        ],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_firebender_fixture(temp: &TempDir, query: &str) -> String {
    let project = temp.path().join("native-firebender/project");
    let db = project
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::copy(
        provider_history_fixture("firebender/v1/.idea/firebender/chat_history.db"),
        &db,
    )
    .unwrap();
    let conn = Connection::open(&db).unwrap();
    let messages = sqlite_column_text(
        &conn,
        "SELECT messages_json FROM chat_sessions WHERE id = 'firebender-fixture-session'",
    )
    .replace("firebender fixture oracle prompt", query);
    conn.execute(
        "UPDATE chat_sessions SET messages_json = ?1 WHERE id = 'firebender-fixture-session'",
        params![messages],
    )
    .unwrap();
    project.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_lingma_fixture(temp: &TempDir, query: &str) -> String {
    let db = temp.path().join("native-lingma/local.db");
    write_lingma_sqlite_fixture(&db, query);
    db.to_str().unwrap().to_owned()
}

pub(crate) fn write_lingma_sqlite_fixture(path: &Path, query: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE chat_record (
            session_id TEXT NOT NULL,
            request_id TEXT,
            chat_prompt TEXT,
            summary TEXT,
            error_result TEXT,
            gmt_create INTEGER,
            extra TEXT
        );
        "#,
    )
    .unwrap();
    conn.execute(
        r#"
        INSERT INTO chat_record
            (session_id, request_id, chat_prompt, summary, error_result, gmt_create, extra)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
        params![
            "lingma-cli-session",
            "lingma-cli-request",
            query,
            "Lingma CLI assistant summary import ok",
            "{}",
            1_783_166_400_000_i64,
            json!({"model": "lingma-cli-fixture"}).to_string(),
        ],
    )
    .unwrap();
}

pub(crate) fn write_native_hermes_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-hermes-state.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            source text not null,
            model text,
            model_config text,
            parent_session_id text,
            started_at real not null,
            ended_at real,
            message_count integer default 0,
            tool_call_count integer default 0,
            input_tokens integer default 0,
            output_tokens integer default 0,
            cwd text,
            title text,
            archived integer default 0
        );
        create table messages (
            id integer primary key autoincrement,
            session_id text not null,
            role text not null,
            content text,
            tool_calls text,
            tool_call_id text,
            tool_name text,
            timestamp real not null,
            active integer not null default 1,
            compacted integer not null default 0
        );",
    )
    .unwrap();
    conn.execute(
        "insert into sessions (
            id, source, model, model_config, started_at, message_count, cwd, title
        ) values (?1, 'acp', 'gpt-5-mini', ?2, 1782259200.0, 2, '/workspace', 'native hermes')",
        [
            "hermes-cli-native",
            r#"{"cwd":"/workspace","provider":"openai"}"#,
        ],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'user', ?2, 1782259201.0)",
        ["hermes-cli-native", query],
    )
    .unwrap();
    conn.execute(
        "insert into messages (session_id, role, content, timestamp) values (?1, 'assistant', 'native import ok', 1782259202.0)",
        ["hermes-cli-native"],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_nanoclaw_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-nanoclaw");
    let data = root.join("data");
    let session_dir = data.join("v2-sessions/ag-1/session-1");
    fs::create_dir_all(&session_dir).unwrap();
    let central = Connection::open(data.join("v2.db")).unwrap();
    central
        .execute_batch(
            "create table agent_groups (
                id text primary key,
                name text,
                folder text,
                agent_provider text
            );
            create table messaging_groups (
                id text primary key,
                channel_type text,
                platform_id text,
                instance text,
                name text
            );
            create table sessions (
                id text primary key,
                agent_group_id text not null,
                messaging_group_id text,
                thread_id text,
                agent_provider text,
                status text,
                container_status text,
                last_active integer,
                created_at integer
            );",
        )
        .unwrap();
    central
        .execute(
            "insert into agent_groups values ('ag-1', 'Personal', '/workspace', 'codex')",
            [],
        )
        .unwrap();
    central
        .execute(
            "insert into messaging_groups values ('mg-1', 'telegram', 'chat-1', 'default', 'DM')",
            [],
        )
        .unwrap();
    central
        .execute(
            "insert into sessions values (
                'session-1', 'ag-1', 'mg-1', 'thread-1', 'codex', 'active',
                'running', 1782259202000, 1782259200000
            )",
            [],
        )
        .unwrap();
    let inbound = Connection::open(session_dir.join("inbound.db")).unwrap();
    inbound
        .execute_batch(
            "create table messages_in (
                id text primary key,
                seq integer,
                kind text,
                timestamp integer,
                status text,
                trigger text,
                platform_id text,
                channel_type text,
                thread_id text,
                content text,
                source_session_id text,
                on_wake integer
            );",
        )
        .unwrap();
    inbound
        .execute(
            "insert into messages_in values (
                'in-1', 1, 'chat', 1782259201000, 'done', 'message',
                'chat-1', 'telegram', 'thread-1', ?1, null, 0
            )",
            [json!({"text": query}).to_string()],
        )
        .unwrap();
    let outbound = Connection::open(session_dir.join("outbound.db")).unwrap();
    outbound
        .execute_batch(
            "create table messages_out (
                id text primary key,
                seq integer,
                in_reply_to text,
                timestamp integer,
                kind text,
                platform_id text,
                channel_type text,
                thread_id text,
                content text
            );",
        )
        .unwrap();
    outbound
        .execute(
            "insert into messages_out values (
                'out-1', 2, 'in-1', 1782259202000, 'chat',
                'chat-1', 'telegram', 'thread-1', ?1
            )",
            [json!({"text": "native import ok"}).to_string()],
        )
        .unwrap();
    root.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_astrbot_fixture(temp: &TempDir, query: &str) -> String {
    let data = temp.path().join("native-astrbot/data");
    fs::create_dir_all(&data).unwrap();
    let path = data.join("data_v4.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
            id integer primary key,
            inner_conversation_id text,
            conversation_id text,
            platform_id text,
            user_id text,
            content text not null,
            title text,
            persona_id text,
            token_usage text,
            created_at integer,
            updated_at integer
        );
        create table preferences (
            scope text,
            key text,
            value text
        );
        create table platform_message_history (
            id integer primary key,
            platform_id text,
            user_id text,
            sender_id text,
            sender_name text,
            content text,
            llm_checkpoint_id text,
            created_at integer
        );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
            1, 'umo-1', 'conv-1', 'webchat', 'user-1', ?1, 'native astrbot',
            'default', ?2, 1782259200000, 1782259202000
        )",
        [
            json!([
                {"role": "user", "content": query},
                {"type": "_checkpoint", "id": "checkpoint-1"},
                {"role": "assistant", "content": "native import ok"}
            ])
            .to_string(),
            json!({"prompt": 1, "completion": 1}).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "insert into preferences values ('umo', 'sel_conv_id', 'conv-1')",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into platform_message_history values (
            1, 'webchat', 'user-1', 'user-1', 'User', ?1, 'checkpoint-1', 1782259201000
        )",
        [json!({"text": query}).to_string()],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_shelley_fixture(temp: &TempDir, query: &str) -> String {
    let path = temp.path().join("native-shelley.db");
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
            draft text not null default ''
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
        );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
            'shelley-cli-native', 'native shelley', 1, '2026-06-24 12:00:00',
            '2026-06-24 12:00:01', '/workspace', 0, null, 'claude-opus-4-7',
            '{}', 1, 0, '[]', 0, ''
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, user_data, created_at
        ) values (
            'shelley-cli-native-user', 'shelley-cli-native', 1, 'user', ?1,
            '2026-06-24 12:00:01'
        )",
        [json!({"Content": [{"Type": 2, "Text": query}]}).to_string()],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
            message_id, conversation_id, sequence_id, type, llm_data, usage_data,
            created_at, llm_api_url, model_name
        ) values (
            'shelley-cli-native-agent', 'shelley-cli-native', 2, 'agent', ?1, ?2,
            '2026-06-24 12:00:02', 'https://api.anthropic.com/v1/messages',
            'claude-opus-4-7'
        )",
        [
            json!({"Content": [{"Type": 2, "Text": "native Shelley import ok"}]}).to_string(),
            json!({"input_tokens": 12, "output_tokens": 8, "cost_usd": 0.001}).to_string(),
        ],
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}
