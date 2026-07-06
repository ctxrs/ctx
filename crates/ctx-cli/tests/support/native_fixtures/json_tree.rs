use serde_json::json;
use std::{fs, path::Path};
use tempfile::TempDir;

pub(crate) fn write_pi_session_jsonl(path: &Path, id: &str, query: &str) {
    fs::write(
        path,
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "version": 3,
                "id": id,
                "timestamp": "2026-06-24T12:00:00.000Z",
                "cwd": "/workspace"
            }),
            json!({
                "type": "message",
                "id": format!("{id}-user"),
                "timestamp": "2026-06-24T12:00:01.000Z",
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": query}]
                }
            })
        ),
    )
    .unwrap();
}

pub(crate) fn write_native_claude_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-claude/projects/-workspace");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("claude-cli-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "sessionId": "claude-cli-native",
                "timestamp": "2026-06-24T12:00:00Z",
                "cwd": "/workspace",
                "version": "test",
                "type": "user",
                "message": {"role": "user", "content": [{"type": "text", "text": query}]},
                "uuid": "claude-cli-native-user"
            }),
            json!({
                "sessionId": "claude-cli-native",
                "timestamp": "2026-06-24T12:00:01Z",
                "cwd": "/workspace",
                "version": "test",
                "type": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "native import ok"}]},
                "uuid": "claude-cli-native-assistant"
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-claude/projects")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_mistral_vibe_fixture(temp: &TempDir, query: &str) -> String {
    let session_dir = temp
        .path()
        .join("native-mistral-vibe/logs/session/session_20260704_160000_vibecli");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("meta.json"),
        json!({
            "session_id": "mistral-vibe-cli-native",
            "parent_session_id": null,
            "start_time": "2026-07-04T16:00:00Z",
            "end_time": "2026-07-04T16:00:03Z",
            "git_commit": "2222222222222222222222222222222222222222",
            "git_branch": "main",
            "environment": {"working_directory": "/workspace/mistral-vibe"},
            "username": "fixture-user",
            "loops": [],
            "title": "Mistral Vibe CLI native",
            "title_source": "auto",
            "total_messages": 4,
            "stats": {"total_tokens": 64, "total_cost": 0.0},
            "agent_profile": {"name": "default", "overrides": {}}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session_dir.join("messages.jsonl"),
        format!(
            "{}\n{}\n{}\n{}\n",
            json!({
                "role": "user",
                "content": query,
                "message_id": "msg-mistral-vibe-user"
            }),
            json!({
                "role": "assistant",
                "content": "mistral vibe native import ok",
                "message_id": "msg-mistral-vibe-tool",
                "tool_calls": [{
                    "id": "call-mistral-vibe-cli",
                    "type": "function",
                    "function": {
                        "name": "write_file",
                        "arguments": "{\"path\":\"src/mistral_vibe_native.rs\",\"content\":\"proof\"}"
                    }
                }]
            }),
            json!({
                "role": "tool",
                "content": "wrote src/mistral_vibe_native.rs",
                "tool_call_id": "call-mistral-vibe-cli",
                "name": "write_file"
            }),
            json!({
                "role": "assistant",
                "content": "Mistral Vibe import finished",
                "message_id": "msg-mistral-vibe-final"
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-mistral-vibe/logs/session")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_rovodev_fixture(temp: &TempDir, query: &str) -> String {
    let session = temp
        .path()
        .join("native-rovodev/sessions/rovodev-cli-native");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("metadata.json"),
        json!({
            "session_id": "rovodev-cli-native",
            "title": "Rovo Dev CLI native",
            "workspace_path": "/workspace/rovodev",
            "created_at": "2026-07-04T18:20:00Z",
            "updated_at": "2026-07-04T18:20:02Z"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session.join("session_context.json"),
        json!({
            "message_history": [
                {
                    "id": "rovodev-cli-native-user",
                    "role": "user",
                    "created_at": "2026-07-04T18:20:00Z",
                    "parts": [{"kind": "text", "text": query}]
                },
                {
                    "id": "rovodev-cli-native-assistant",
                    "role": "assistant",
                    "created_at": "2026-07-04T18:20:01Z",
                    "parts": [
                        {"kind": "text", "text": "rovodev native import ok"},
                        {"kind": "tool_use", "name": "Write", "input": {"path": "src/rovodev_cli_native.txt", "content": "proof"}}
                    ]
                },
                {
                    "id": "rovodev-cli-native-tool",
                    "role": "tool",
                    "created_at": "2026-07-04T18:20:02Z",
                    "parts": [{"kind": "tool_result", "content": "wrote src/rovodev_cli_native.txt"}]
                }
            ]
        })
        .to_string(),
    )
    .unwrap();
    temp.path()
        .join("native-rovodev/sessions")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_auggie_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-auggie/sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("01K0AUGGIENATIVE0000000000.json"),
        serde_json::to_string_pretty(&json!({
            "sessionId": "01K0AUGGIENATIVE0000000000",
            "created": "2026-07-04T20:00:00.000Z",
            "modified": "2026-07-04T20:00:04.000Z",
            "workspaceId": "workspace-auggie-native",
            "workspaceRoot": "/workspace/auggie",
            "agentState": {
                "userGuidelines": "",
                "workspaceGuidelines": ""
            },
            "chatHistory": [
                {
                    "exchange": {
                        "request_message": query,
                        "response_text": "native Auggie import ok",
                        "request_id": "req-auggie-native-1"
                    },
                    "completed": true,
                    "sequenceId": 1,
                    "finishedAt": "2026-07-04T20:00:02.000Z",
                    "changedFiles": [],
                    "changedFilesSkipped": [],
                    "changedFilesSkippedCount": 0,
                    "isHistorySummary": false,
                    "historySummaryVersion": 0,
                    "source": "remote"
                },
                {
                    "exchange": {
                        "request_nodes": [{
                            "type": 0,
                            "text_node": {
                                "content": format!("{query} node")
                            }
                        }],
                        "response_nodes": [{
                            "type": 0,
                            "text_node": {
                                "content": "native Auggie node response"
                            }
                        }],
                        "request_id": "req-auggie-native-2"
                    },
                    "completed": true,
                    "sequenceId": 2,
                    "finishedAt": "2026-07-04T20:00:04.000Z",
                    "changedFiles": [],
                    "changedFilesSkipped": [],
                    "changedFilesSkippedCount": 0,
                    "isHistorySummary": false,
                    "historySummaryVersion": 0,
                    "source": "remote"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_junie_fixture(temp: &TempDir, query: &str) -> String {
    let sessions = temp.path().join("native-junie/sessions");
    let session_id = "session-260607-120000-native";
    let session = sessions.join(session_id);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        sessions.join("index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": session_id,
                "createdAt": 1783348800000i64,
                "updatedAt": 1783348920000i64,
                "taskName": "Junie native CLI fixture",
                "projectDir": "/workspace/junie-native"
            })
        ),
    )
    .unwrap();
    fs::write(
        session.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "kind": "UserPromptEvent",
                "prompt": query
            }),
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1783348920000i64,
                "event": {
                    "agentEvent": {
                        "kind": "ResultBlockUpdatedEvent",
                        "stepId": "result-1",
                        "result": format!("Junie answered {query}")
                    }
                }
            })
        ),
    )
    .unwrap();
    sessions.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_mux_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-mux/sessions");
    let session_dir = root.join("mux-cli-native");
    let child_dir = session_dir
        .join("subagent-transcripts")
        .join("mux-cli-child");
    fs::create_dir_all(&child_dir).unwrap();
    fs::write(
        session_dir.join("metadata.json"),
        json!({
            "workspaceId": "mux-cli-native",
            "projectPath": "/workspace/mux",
            "model": "gpt-5-test"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session_dir.join("chat.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({
                "id": "msg-mux-cli-user",
                "role": "user",
                "parts": [{"type": "text", "text": query, "timestamp": 1783180800000_i64}],
                "createdAt": "2026-07-04T16:00:00.000Z",
                "metadata": {"historySequence": 0, "timestamp": 1783180800000_i64, "model": "gpt-5-test"},
                "workspaceId": "mux-cli-native"
            }),
            json!({
                "id": "msg-mux-cli-tool-call",
                "role": "assistant",
                "parts": [
                    {"type": "text", "text": "mux cli native import ok", "timestamp": 1783180801000_i64},
                    {
                        "type": "dynamic-tool",
                        "toolCallId": "call-mux-cli",
                        "toolName": "file_write",
                        "input": {"path": "src/mux_native.rs", "content": "proof"},
                        "state": "input-available",
                        "timestamp": 1783180801000_i64
                    }
                ],
                "createdAt": "2026-07-04T16:00:01.000Z",
                "metadata": {"historySequence": 1, "timestamp": 1783180801000_i64, "model": "gpt-5-test"},
                "workspaceId": "mux-cli-native"
            }),
            json!({
                "id": "msg-mux-cli-tool-output",
                "role": "assistant",
                "parts": [{
                    "type": "dynamic-tool",
                    "toolCallId": "call-mux-cli",
                    "toolName": "file_write",
                    "input": {"path": "src/mux_native.rs", "content": "proof"},
                    "state": "output-available",
                    "output": {"path": "src/mux_native.rs", "ok": true},
                    "timestamp": 1783180802000_i64
                }],
                "createdAt": "2026-07-04T16:00:02.000Z",
                "metadata": {"historySequence": 2, "timestamp": 1783180802000_i64, "model": "gpt-5-test"},
                "workspaceId": "mux-cli-native"
            })
        ),
    )
    .unwrap();
    fs::write(
        session_dir.join("partial.json"),
        json!({
            "id": "msg-mux-cli-partial",
            "role": "assistant",
            "parts": [{"type": "text", "text": "mux cli partial searchable", "timestamp": 1783180803000_i64}],
            "createdAt": "2026-07-04T16:00:03.000Z",
            "metadata": {"historySequence": 3, "timestamp": 1783180803000_i64, "model": "gpt-5-test", "partial": true},
            "workspaceId": "mux-cli-native"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        child_dir.join("metadata.json"),
        json!({
            "childTaskId": "mux-cli-child",
            "parentWorkspaceId": "mux-cli-native",
            "projectPath": "/workspace/mux",
            "model": "gpt-5-test"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        child_dir.join("chat.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "id": "msg-mux-cli-child-user",
                "role": "user",
                "parts": [{"type": "text", "text": "mux child prompt", "timestamp": 1783180804000_i64}],
                "createdAt": "2026-07-04T16:00:04.000Z",
                "metadata": {"historySequence": 0, "timestamp": 1783180804000_i64, "model": "gpt-5-test"},
                "workspaceId": "mux-cli-child"
            }),
            json!({
                "id": "msg-mux-cli-child-assistant",
                "role": "assistant",
                "parts": [{"type": "text", "text": "mux child finished", "timestamp": 1783180805000_i64}],
                "createdAt": "2026-07-04T16:00:05.000Z",
                "metadata": {"historySequence": 1, "timestamp": 1783180805000_i64, "model": "gpt-5-test"},
                "workspaceId": "mux-cli-child"
            })
        ),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_gemini_fixture(temp: &TempDir, query: &str) -> String {
    let chats = temp.path().join("native-gemini/.gemini/tmp/project/chats");
    fs::create_dir_all(&chats).unwrap();
    fs::write(
        chats.join("session-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "sessionId": "gemini-cli-native",
                "startTime": "2026-06-24T12:00:00Z",
                "kind": "main",
                "directories": ["/workspace"]
            }),
            json!({
                "id": "gemini-cli-native-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "type": "user",
                "content": query
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-gemini/.gemini")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_cursor_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp
        .path()
        .join("native-cursor/projects/sanitized-workspace/agent-transcripts/cursor-cli-native");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("cursor-cli-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-06-24T12:00:00Z",
                "role": "user",
                "message": {"role": "user", "content": [{"type": "text", "text": query}]}
            }),
            json!({
                "timestamp": "2026-06-24T12:00:01Z",
                "role": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "native import ok"}]}
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-cursor/projects")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_windsurf_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-windsurf/transcripts");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("windsurf-cli-native.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({
                "status": "done",
                "type": "user_input",
                "user_input": {"user_response": query}
            }),
            json!({
                "status": "done",
                "type": "planner_response",
                "planner_response": {"response": "native import ok"}
            }),
            json!({
                "status": "done",
                "type": "code_action",
                "code_action": {
                    "path": "src/windsurf_cli_native.py",
                    "new_content": "print('native import ok')\n"
                }
            })
        ),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_qoder_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp
        .path()
        .join("native-qoder/projects/sanitized-workspace/transcript");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("qoder-cli-native.jsonl"),
        format!(
            "{}\n{}\n{}\n{}\n{}\n",
            json!({
                "type": "session_meta",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-meta",
                "timestamp": "2026-07-01T12:00:00Z",
                "cwd": "/workspace/qoder-cli",
                "data": {
                    "meta_type": "session_info",
                    "content": {"mode": "agent", "session_type": "assistant"}
                }
            }),
            json!({
                "type": "user",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-user",
                "timestamp": "2026-07-01T12:00:01Z",
                "cwd": "/workspace/qoder-cli",
                "message": {"role": "user", "content": query}
            }),
            json!({
                "type": "assistant",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-assistant",
                "timestamp": "2026-07-01T12:00:02Z",
                "cwd": "/workspace/qoder-cli",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "qoder native import ok"}]
                }
            }),
            json!({
                "type": "assistant",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-tool",
                "timestamp": "2026-07-01T12:00:03Z",
                "cwd": "/workspace/qoder-cli",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call-qoder-cli-read",
                        "name": "read_file",
                        "input": {"file_path": "src/qoder_cli_native.py"}
                    }]
                }
            }),
            json!({
                "type": "user",
                "sessionId": "qoder-cli-native",
                "uuid": "qoder-cli-tool-result",
                "timestamp": "2026-07-01T12:00:04Z",
                "cwd": "/workspace/qoder-cli",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call-qoder-cli-read",
                        "content": "native qoder fixture result",
                        "is_error": false
                    }]
                },
                "toolUseResult": "native qoder fixture result"
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-qoder/projects")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_openhands_fixture(temp: &TempDir, query: &str) -> String {
    let conversation = temp
        .path()
        .join("native-openhands/local-user/v1_conversations/12345678123456781234567812345678");
    fs::create_dir_all(&conversation).unwrap();
    fs::write(
        conversation.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json"),
        serde_json::to_string_pretty(&json!({
            "id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "timestamp": "2026-06-24T12:00:00Z",
            "source": "user",
            "llm_message": {
                "role": "user",
                "content": [{"type": "text", "text": query}]
            },
            "activated_microagents": [],
            "extended_content": []
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        conversation.join("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.json"),
        serde_json::to_string_pretty(&json!({
            "id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "timestamp": "2026-06-24T12:00:01Z",
            "source": "agent",
            "action": {
                "kind": "FileEditorAction",
                "command": "str_replace",
                "path": "openhands-cli-native-oracle.txt",
                "file_text": null,
                "old_str": "old",
                "new_str": "new",
                "insert_line": null,
                "view_range": null
            },
            "tool_name": "FileEditor",
            "tool_call_id": "call-openhands-file",
            "tool_call": {
                "id": "call-openhands-file",
                "type": "function",
                "function": {
                    "name": "FileEditor",
                    "arguments": "{\"command\":\"str_replace\"}"
                }
            },
            "llm_response_id": "response-openhands-file",
            "security_risk": "LOW",
            "thought": []
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        conversation.join("cccccccccccccccccccccccccccccccc.json"),
        serde_json::to_string_pretty(&json!({
            "id": "cccccccccccccccccccccccccccccccc",
            "timestamp": "2026-06-24T12:00:02Z",
            "source": "environment",
            "observation": {
                "kind": "FileEditorObservation",
                "command": "str_replace",
                "output": "Edited openhands-cli-native-oracle.txt",
                "path": "openhands-cli-native-oracle.txt",
                "prev_exist": true,
                "old_content": "old",
                "new_content": "new",
                "error": null
            },
            "tool_name": "FileEditor",
            "tool_call_id": "call-openhands-file",
            "action_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        }))
        .unwrap(),
    )
    .unwrap();
    temp.path()
        .join("native-openhands")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_copilot_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp
        .path()
        .join("native-copilot/session-state/copilot-cli-native");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "id": "copilot-cli-native-start",
                "timestamp": "2026-06-24T12:00:00Z",
                "type": "session.start",
                "data": {
                    "sessionId": "copilot-cli-native",
                    "startTime": "2026-06-24T12:00:00Z",
                    "selectedModel": "gpt-5-mini",
                    "context": {"cwd": "/workspace"}
                }
            }),
            json!({
                "id": "copilot-cli-native-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "type": "user.message",
                "data": {"content": query}
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-copilot/session-state")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_factory_droid_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-droid/sessions/project");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("droid-cli-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session_start",
                "sessionId": "droid-cli-native",
                "timestamp": "2026-06-24T12:00:00Z",
                "cwd": "/workspace",
                "model": "factory/droid"
            }),
            json!({
                "type": "message",
                "id": "droid-cli-native-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "role": "user",
                "content": [{"type": "text", "text": query}]
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-droid/sessions")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_qwen_fixture(temp: &TempDir, query: &str) -> String {
    let chats = temp
        .path()
        .join("native-qwen/.qwen/projects/workspace-qwen/chats");
    fs::create_dir_all(&chats).unwrap();
    fs::write(
        chats.join("qwen-cli-native.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({
                "uuid": "qwen-cli-native-user",
                "parentUuid": null,
                "sessionId": "qwen-cli-native",
                "timestamp": "2026-07-04T12:00:00Z",
                "type": "user",
                "cwd": "/workspace/qwen",
                "version": "test",
                "gitBranch": "main",
                "message": {"role": "user", "content": [{"type": "text", "text": query}]},
                "model": "qwen3-coder"
            }),
            json!({
                "uuid": "qwen-cli-native-assistant",
                "parentUuid": "qwen-cli-native-user",
                "sessionId": "qwen-cli-native",
                "timestamp": "2026-07-04T12:00:01Z",
                "type": "assistant",
                "cwd": "/workspace/qwen",
                "version": "test",
                "gitBranch": "main",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "native Qwen import ok"},
                        {"type": "tool_use", "id": "tool-1", "name": "Write", "input": {"path": "src/qwen_cli_native.txt", "content": "proof"}}
                    ]
                },
                "usageMetadata": {"inputTokens": 5, "outputTokens": 7},
                "model": "qwen3-coder"
            }),
            json!({
                "uuid": "qwen-cli-native-tool",
                "parentUuid": "qwen-cli-native-assistant",
                "sessionId": "qwen-cli-native",
                "timestamp": "2026-07-04T12:00:02Z",
                "type": "tool_result",
                "cwd": "/workspace/qwen",
                "version": "test",
                "gitBranch": "main",
                "message": {"role": "tool", "content": [{"type": "tool_result", "tool_use_id": "tool-1", "content": "wrote src/qwen_cli_native.txt"}]},
                "toolCallResult": {"tool": "Write", "path": "src/qwen_cli_native.txt", "output": "ok"},
                "model": "qwen3-coder"
            })
        ),
    )
    .unwrap();
    temp.path()
        .join("native-qwen/.qwen/projects")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn write_native_kimi_fixture(temp: &TempDir, query: &str) -> String {
    let home = temp.path().join("native-kimi/.kimi-code");
    let session = home.join("sessions/wd_demo_abc123/kimi-cli-native");
    let main = session.join("agents/main");
    let child = session.join("agents/agent-1");
    fs::create_dir_all(&main).unwrap();
    fs::create_dir_all(&child).unwrap();
    fs::write(
        home.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": "kimi-cli-native",
                "sessionDir": session.display().to_string(),
                "workDir": "/workspace/kimi"
            })
        ),
    )
    .unwrap();
    fs::write(
        session.join("state.json"),
        json!({
            "createdAt": "2026-07-04T13:00:00Z",
            "updatedAt": "2026-07-04T13:00:05Z",
            "title": "Kimi native CLI",
            "lastPrompt": query,
            "agents": {
                "main": {"homedir": "/fixture/agents/main", "type": "main", "parentAgentId": null},
                "agent-1": {"homedir": "/fixture/agents/agent-1", "type": "coder", "parentAgentId": "main"}
            }
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        main.join("wire.jsonl"),
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n",
            json!({"type": "metadata", "protocol_version": "1.4", "created_at": 1783170000000i64}),
            json!({"type": "turn.prompt", "time": 1783170001000i64, "input": [{"type": "text", "text": query}], "origin": {"kind": "user"}}),
            json!({"type": "context.append_message", "time": 1783170002000i64, "message": {"role": "assistant", "content": [{"type": "text", "text": "native Kimi import ok"}]}}),
            json!({"type": "context.append_loop_event", "time": 1783170003000i64, "event": {"type": "tool.call", "toolName": "Write", "input": {"path": "src/kimi_cli_native.txt", "content": "proof"}}}),
            json!({"type": "context.append_loop_event", "time": 1783170004000i64, "event": {"type": "tool.result", "toolName": "Write", "output": "wrote src/kimi_cli_native.txt"}}),
            json!({"type": "usage.record", "time": 1783170005000i64, "model": "kimi-k2", "usage": {"input_tokens": 11, "output_tokens": 13}})
        ),
    )
    .unwrap();
    fs::write(
        child.join("wire.jsonl"),
        concat!(
            "{\"type\":\"metadata\",\"protocol_version\":\"1.4\",\"created_at\":1783170006000}\n",
            "{\"type\":\"turn.prompt\",\"time\":1783170007000,\"input\":[{\"type\":\"text\",\"text\":\"child inspect\"}]}\n",
            "{\"type\":\"context.append_message\",\"time\":1783170008000,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"child done\"}]}}\n",
        ),
    )
    .unwrap();
    home.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_codebuddy_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-codebuddy/CodeBuddyExtension");
    let project = root.join("Data/VSCode/default/history/11112222333344445555666677778888");
    let session = project.join("session-cli");
    let messages = session.join("messages");
    fs::create_dir_all(&messages).unwrap();
    fs::write(
        project.join("index.json"),
        json!({
            "conversations": [{
                "id": "session-cli",
                "type": "chat",
                "name": "CodeBuddy CLI fixture",
                "createdAt": "2026-07-04T14:00:00Z",
                "lastMessageAt": "2026-07-04T14:00:02Z"
            }],
            "current": "session-cli"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session.join("index.json"),
        json!({
            "messages": [
                {"id": "msg-user", "role": "user", "type": "message"},
                {"id": "msg-assistant", "role": "assistant", "type": "message"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        messages.join("msg-user.json"),
        json!({
            "id": "msg-user",
            "role": "user",
            "message": json!({
                "content": [{"type": "text", "text": query}],
                "createdAt": "2026-07-04T14:00:01Z"
            }).to_string()
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        messages.join("msg-assistant.json"),
        json!({
            "id": "msg-assistant",
            "role": "assistant",
            "message": json!({
                "content": "CodeBuddy CLI native import ok",
                "createdAt": "2026-07-04T14:00:02Z"
            }).to_string()
        })
        .to_string(),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_openclaw_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-openclaw");
    let sessions = root.join("agents/personal-agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("sessions.json"),
        serde_json::to_string(&json!({
            "openclaw-cli-native": {
                "sessionId": "openclaw-cli-native",
                "sessionFile": sessions.join("openclaw-cli-native.jsonl"),
                "sessionStartedAt": "2026-06-24T12:00:00Z",
                "modelProvider": "openai",
                "model": "gpt-5-mini",
                "lastChannel": "telegram"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        sessions.join("openclaw-cli-native.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({
                "type": "session",
                "version": 1,
                "id": "openclaw-cli-native",
                "timestamp": "2026-06-24T12:00:00Z",
                "cwd": "/workspace"
            }),
            json!({
                "type": "message",
                "id": "openclaw-cli-native-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "message": {"role": "user", "content": query}
            }),
            json!({
                "type": "message",
                "id": "openclaw-cli-native-assistant",
                "parentId": "openclaw-cli-native-user",
                "timestamp": "2026-06-24T12:00:02Z",
                "message": {"role": "assistant", "content": "native import ok"}
            })
        ),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}

pub(crate) fn write_native_continue_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-continue/sessions");
    fs::create_dir_all(&root).unwrap();
    let session_id = "continue-cli-native";
    fs::write(
        root.join("sessions.json"),
        serde_json::to_string_pretty(&json!([
            {
                "sessionId": session_id,
                "title": "native continue",
                "dateCreated": "2026-06-24T12:00:00Z",
                "workspaceDirectory": "/workspace",
                "messageCount": 1
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        root.join(format!("{session_id}.json")),
        serde_json::to_string_pretty(&json!({
            "sessionId": session_id,
            "title": "native continue",
            "workspaceDirectory": "/workspace",
            "history": [
                {
                    "id": "continue-cli-native-user",
                    "timestamp": "2026-06-24T12:00:01Z",
                    "message": {
                        "role": "user",
                        "content": query
                    },
                    "contextItems": [
                        {
                            "name": "fixture.rs",
                            "content": "Continue context item marker"
                        }
                    ],
                    "editorState": query
                },
                {
                    "id": "continue-cli-native-assistant",
                    "timestamp": "2026-06-24T12:00:02Z",
                    "message": {
                        "role": "assistant",
                        "content": "native Continue import ok"
                    },
                    "toolCallStates": [
                        {
                            "toolCallId": "tool-continue-read",
                            "toolCall": {
                                "id": "tool-continue-read",
                                "type": "function",
                                "function": {
                                    "name": "readFile",
                                    "arguments": "{\"filepath\":\"fixture.rs\"}"
                                }
                            },
                            "status": "done",
                            "output": [
                                {
                                    "name": "Result",
                                    "description": "",
                                    "content": "Continue tool output marker"
                                }
                            ]
                        }
                    ]
                }
            ],
            "usage": {
                "totalCost": 0,
                "promptTokens": 12,
                "completionTokens": 8
            }
        }))
        .unwrap(),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}
