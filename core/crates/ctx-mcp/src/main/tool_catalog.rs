use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ToolCatalogCapabilities {
    pub(super) subagents: bool,
    pub(super) artifacts: bool,
    pub(super) merge_queue_submit: bool,
}

impl ToolCatalogCapabilities {
    pub(super) fn from_mcp_context(context: &super::ResolvedMcpContext) -> Self {
        Self {
            subagents: context.has_capability("subagents"),
            artifacts: context.has_capability("artifacts"),
            merge_queue_submit: context.has_capability("merge_queue_submit"),
        }
    }

    pub(super) fn disabled_tool_message(self, name: &str) -> Option<String> {
        if is_subagent_tool(name) && !self.subagents {
            return Some(format!(
                "tool disabled: {name} requires an explicit scoped MCP subagents capability"
            ));
        }
        if name == "artifacts_set" && !self.artifacts {
            return Some(
                "tool disabled: artifacts_set requires an explicit scoped MCP artifacts capability"
                    .to_string(),
            );
        }
        if name == "merge_queue_submit" && !self.merge_queue_submit {
            return Some(
                "tool disabled: merge_queue_submit requires an explicit scoped MCP capability"
                    .to_string(),
            );
        }
        None
    }
}

fn is_subagent_tool(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent"
            | "send_input"
            | "archive_agent"
            | "interrupt_agent"
            | "list_agents"
            | "get_agent"
            | "wait_agent"
    )
}

pub(super) fn tools_list_response(capabilities: ToolCatalogCapabilities) -> Value {
    let mut resp = json!({
        "tools": [
            {
                "name": "ping",
                "title": "ctx Ping",
                "description": "Returns ok=true if ctx MCP is reachable.",
                "inputSchema": { "type": "object", "additionalProperties": false }
            },
            {
                "name": "merge_queue_submit",
                "title": "Merge Queue Submit",
                "description": "Submit the current worktree to the merge queue and wait for completion.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target_branch": { "type": "string" },
                        "message": { "type": "string" }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "spawn_agent",
                "title": "Spawn Agent",
                "description": "Creates a durable child agent for the current session and starts its first run.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_label": { "type": "string", "description": "Stable human-readable label for the agent." },
                        "prompt": { "type": "string", "description": "Initial work request for the new agent." },
                        "worktree": { "type": "string", "enum": ["inherit", "new"], "description": "`inherit` shares the caller worktree; `new` creates a dedicated child worktree." },
                        "harness": { "type": "string", "description": "Optional provider override." },
                        "model": { "type": "string", "description": "Optional model override." },
                        "reasoning_effort": { "type": "string", "description": "Optional reasoning effort override." }
                    },
                    "required": ["task_label", "prompt", "worktree"],
                    "additionalProperties": false
                }
            },
            {
                "name": "send_input",
                "title": "Send Input",
                "description": "Queues follow-up work for an existing child agent. Optionally interrupts the current run first.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string", "description": "Opaque agent reference returned by spawn_agent/list_agents/get_agent." },
                        "message": { "type": "string", "description": "Message to queue for the agent." },
                        "interrupt": { "type": "boolean", "description": "Interrupt the current run before queueing the new message." }
                    },
                    "required": ["agent_id", "message"],
                    "additionalProperties": false
                }
            },
            {
                "name": "archive_agent",
                "title": "Archive Agent",
                "description": "Archives an idle child agent so it no longer counts toward the active child limit. Dedicated child worktrees are reclaimed; inherited parent worktrees are preserved.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string", "description": "Opaque agent reference returned by spawn_agent/list_agents/get_agent." }
                    },
                    "required": ["agent_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "wait_agent",
                "title": "Wait Agent",
                "description": "Waits in a bounded way for agent updates or terminal outcomes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string", "description": "Single opaque agent reference." },
                        "agent_ids": { "type": "array", "items": { "type": "string" }, "description": "Multiple opaque agent references." },
                        "timeout_ms": { "type": "integer", "minimum": 0, "description": "Maximum wait time in milliseconds. 0 means poll." },
                        "mode": { "type": "string", "enum": ["any", "all"], "description": "Whether any or all targets must satisfy the wait condition." },
                        "until": { "type": "string", "enum": ["terminal", "update"], "description": "What kind of condition to wait for." },
                        "since_seq": { "type": "integer", "description": "Optional event-sequence cursor for single-agent update waits." }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "interrupt_agent",
                "title": "Interrupt Agent",
                "description": "Requests interruption for a running child agent.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string", "description": "Opaque agent reference returned by spawn_agent/list_agents/get_agent." }
                    },
                    "required": ["agent_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "list_agents",
                "title": "List Agents",
                "description": "Lists child agents for the current session as cheap summaries.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false
                }
            },
            {
                "name": "get_agent",
                "title": "Get Agent",
                "description": "Returns durable state and latest result details for a child agent.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string", "description": "Opaque agent reference returned by spawn_agent/list_agents/get_agent." }
                    },
                    "required": ["agent_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "artifacts_set",
                "title": "Set Session Artifacts",
                "description": "Sets the ordered list of artifacts for the current session. Paths must stay inside the session worktree or that session's tool-output spool subtree. Video artifacts such as mp4, webm, and mov are supported.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "artifacts": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "absoluteFilePath": { "type": "string", "description": "Absolute file path to the artifact." },
                                    "name": { "type": "string", "description": "Optional display name." },
                                    "mimeType": { "type": "string", "description": "Optional MIME type override." }
                                },
                                "required": ["absoluteFilePath"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["artifacts"],
                    "additionalProperties": false
                }
            },
            // TODO: Re-enable web session MCP tool definitions.
            /*
            {
                "name": "session_create",
                "title": "Create Session",
                "description": "Creates a new session (currently supports kind=web).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "description": "Session kind (web)." },
                        "target": {
                            "type": "object",
                            "properties": {
                                "url": { "type": "string" }
                            },
                            "required": ["url"],
                            "additionalProperties": true
                        },
                        "viewport": {
                            "type": "object",
                            "properties": {
                                "width": { "type": "integer", "minimum": 1 },
                                "height": { "type": "integer", "minimum": 1 }
                            },
                            "additionalProperties": false
                        },
                        "fps": { "type": "integer", "minimum": 1 },
                    },
                    "required": ["kind", "target"],
                    "additionalProperties": false
                }
            },
            {
                "name": "session_list",
                "title": "List Sessions",
                "description": "Lists active sessions (currently web only).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string" }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "session_info",
                "title": "Get Session Info",
                "description": "Fetches session details by id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_ref": { "type": "string" }
                    },
                    "required": ["session_ref"],
                    "additionalProperties": false
                }
            },
            {
                "name": "session_run",
                "title": "Run Session Script",
                "description": "Runs a script against a session (default timeout 5m).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_ref": { "type": "string" },
                        "code": { "type": "string" },
                        "script_path": { "type": "string" },
                        "timeout_ms": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["session_ref"],
                    "additionalProperties": false
                }
            },
            {
                "name": "session_eval",
                "title": "Eval Session Script",
                "description": "Evaluates code against a session (default timeout 5m).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_ref": { "type": "string" },
                        "code": { "type": "string" },
                        "script_path": { "type": "string" },
                        "timeout_ms": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["session_ref"],
                    "additionalProperties": false
                }
            },
            {
                "name": "session_close",
                "title": "Close Session",
                "description": "Closes a session and tears down resources.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_ref": { "type": "string" }
                    },
                    "required": ["session_ref"],
                    "additionalProperties": false
                }
            }
            */
        ]
    });

    if !super::dev_tools_enabled() {
        if let Some(tools) = resp.get_mut("tools").and_then(|v| v.as_array_mut()) {
            tools.retain(|tool| {
                let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
                name != "ping"
            });
        }
    }

    if let Some(tools) = resp.get_mut("tools").and_then(|v| v.as_array_mut()) {
        tools.retain(|tool| {
            let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
            capabilities.disabled_tool_message(name).is_none()
        });
    }

    resp
}
