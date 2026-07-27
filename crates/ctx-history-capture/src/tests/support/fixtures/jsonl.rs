use crate::MAX_PROVIDER_JSONL_LINE_BYTES;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub(in crate::tests) fn write_oversized_jsonl_line(path: &Path) {
    fs::write(path, vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES + 1]).unwrap();
}

pub(in crate::tests) fn oversized_jsonl_line() -> Vec<u8> {
    let mut line = vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES + 1];
    line.push(b'\n');
    line
}

pub(in crate::tests) fn jsonl_line(value: Value) -> String {
    serde_json::to_string(&value).unwrap() + "\n"
}

pub(in crate::tests) fn write_claude_smoke_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("claude/projects/-workspace");
    let subagents = root.join("claude-native-parent/subagents");
    fs::create_dir_all(&subagents).unwrap();
    fs::write(
            root.join("claude-native-parent.jsonl"),
            concat!(
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Run a smoke tool.\"}]},\"uuid\":\"claude-parent-1\"}\n",
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Bash\",\"input\":{\"command\":\"true\"}}]},\"uuid\":\"claude-parent-2\"}\n",
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"created commit abcdef0123456789abcdef0123456789abcdef01 https://github.com/ctxrs/ctx/pull/123?token=claude-secret#fragment CLAUDE_RESULT_NARRATIVE_MUST_NOT_RETAIN\"}]},\"uuid\":\"claude-parent-3\"}\n",
            ),
        )
        .unwrap();
    fs::write(
            subagents.join("agent-scout.jsonl"),
            concat!(
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"isSidechain\":true,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"inspect\"},\"uuid\":\"claude-child-1\"}\n",
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"isSidechain\":true,\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"done\"},\"uuid\":\"claude-child-2\"}\n",
            ),
        )
        .unwrap();
    temp.path().join("claude/projects")
}

pub(in crate::tests) fn write_nanoclaw_smoke_project(temp: &TempDir, query: &str) -> PathBuf {
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
            "insert into agent_groups values ('ag-1', 'Personal', '/workspace/nanoclaw', 'codex')",
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
            [json!({"text": "nanoclaw native import ok"}).to_string()],
        )
        .unwrap();
    root
}

pub(in crate::tests) fn write_gemini_smoke_fixture(temp: &TempDir) -> PathBuf {
    let chats = temp.path().join("gemini/.gemini/tmp/project/chats");
    let child_dir = chats.join("gemini-root");
    fs::create_dir_all(&child_dir).unwrap();
    fs::write(
            chats.join("session-root.jsonl"),
            concat!(
                "{\"sessionId\":\"gemini-root\",\"startTime\":\"2026-06-24T12:00:00Z\",\"kind\":\"main\",\"directories\":[\"/workspace\"]}\n",
                "{\"id\":\"gemini-user\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"user\",\"content\":\"gemini jsonl oracle prompt\"}\n",
                "{\"id\":\"gemini-tool\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"type\":\"gemini\",\"toolCalls\":[{\"id\":\"call-1\",\"name\":\"run_subagent\"}]}\n",
                "{\"id\":\"gemini-tool-result\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"type\":\"gemini\",\"toolCalls\":[{\"id\":\"call-1\",\"name\":\"run_subagent\",\"result\":{\"content\":\"GEMINI_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH\"}}]}\n",
            ),
        )
        .unwrap();
    fs::write(
            child_dir.join("gemini-child.jsonl"),
            concat!(
                "{\"sessionId\":\"gemini-child\",\"startTime\":\"2026-06-24T12:00:04Z\",\"kind\":\"subagent\",\"directories\":[\"/workspace\"]}\n",
                "{\"id\":\"gemini-child-user\",\"timestamp\":\"2026-06-24T12:00:05Z\",\"type\":\"user\",\"content\":\"gemini child oracle prompt\"}\n",
            ),
        )
        .unwrap();
    temp.path().join("gemini/.gemini")
}

pub(in crate::tests) fn write_droid_smoke_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("droid/sessions/project");
    fs::create_dir_all(&root).unwrap();
    fs::write(
            root.join("droid-root.jsonl"),
            concat!(
                "{\"type\":\"session_start\",\"id\":\"droid-root\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\"}\n",
                "{\"type\":\"message\",\"id\":\"droid-user\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"droid jsonl oracle prompt\"}]}}\n",
                "{\"type\":\"message\",\"id\":\"droid-tool\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"droid_worker\"}]}}\n",
                "{\"type\":\"message\",\"id\":\"droid-tool-result\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"message\":{\"role\":\"tool\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"DROID_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH\"}]}}\n",
            ),
        )
        .unwrap();
    fs::write(
            root.join("droid-child.jsonl"),
            concat!(
                "{\"type\":\"session_start\",\"id\":\"droid-child\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\",\"parent\":\"droid-root\",\"decompSessionType\":\"worker\"}\n",
                "{\"type\":\"message\",\"id\":\"droid-child-user\",\"timestamp\":\"2026-06-24T12:00:05Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"droid child oracle prompt\"}]}}\n",
            ),
        )
        .unwrap();
    temp.path().join("droid/sessions")
}

pub(in crate::tests) fn write_copilot_smoke_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("copilot/session-state/copilot-root");
    fs::create_dir_all(&root).unwrap();
    fs::write(
            root.join("events.jsonl"),
            concat!(
                "{\"id\":\"copilot-1\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"type\":\"session.start\",\"data\":{\"sessionId\":\"copilot-root\",\"startTime\":\"2026-06-24T12:00:00Z\",\"selectedModel\":\"gpt-5-mini\",\"context\":{\"cwd\":\"/workspace\"}}}\n",
                "{\"id\":\"copilot-2\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"user.message\",\"data\":{\"content\":\"status\"}}\n",
                "{\"id\":\"copilot-3\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"type\":\"assistant.message\",\"data\":{\"content\":\"running\",\"toolRequests\":[{\"toolCallId\":\"tool-1\",\"name\":\"bash\"}]}}\n",
                "{\"id\":\"copilot-4\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"type\":\"tool.execution_start\",\"data\":{\"toolCallId\":\"tool-1\",\"toolName\":\"bash\"}}\n",
                "{\"id\":\"copilot-5\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"type\":\"tool.execution_complete\",\"data\":{\"toolCallId\":\"tool-1\",\"success\":true,\"result\":{\"content\":\"COPILOT_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH\"}}}\n",
            ),
        )
        .unwrap();
    let child = temp.path().join("copilot/session-state/copilot-child");
    fs::create_dir_all(&child).unwrap();
    fs::write(
            child.join("events.jsonl"),
            concat!(
                "{\"id\":\"copilot-child-1\",\"timestamp\":\"2026-06-24T12:00:05Z\",\"type\":\"session.start\",\"data\":{\"sessionId\":\"copilot-child\",\"startTime\":\"2026-06-24T12:00:05Z\",\"selectedModel\":\"gpt-5-mini\",\"context\":{\"cwd\":\"/workspace\"}}}\n",
                "{\"id\":\"copilot-child-2\",\"timestamp\":\"2026-06-24T12:00:06Z\",\"type\":\"user.message\",\"data\":{\"content\":\"copilot child oracle prompt\"}}\n",
            ),
        )
        .unwrap();
    temp.path().join("copilot/session-state")
}
