
#[path = "../build_identity.rs"]
mod build_identity;

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use uuid::Uuid;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Parser, Debug)]
#[command(name = "ctx-mcp")]
struct Cli {
    /// Run as an MCP stdio server (newline-delimited JSON-RPC).
    #[arg(long)]
    stdio: bool,
}

fn ctx_env(name: &str) -> std::result::Result<String, std::env::VarError> {
    std::env::var(format!("CTX_{name}"))
}

fn ctx_env_opt(name: &str) -> Option<String> {
    ctx_env(name).ok()
}

fn dev_tools_enabled() -> bool {
    ctx_env_opt("MCP_DEV_MODE")
        .as_deref()
        .and_then(ctx_core::boolish::parse_boolish)
        .unwrap_or(false)
}

#[derive(Default)]
struct InteractiveSessionRefMap {
    by_session: HashMap<String, String>,
    by_ref: HashMap<String, String>,
}

impl InteractiveSessionRefMap {
    fn session_ref_for_session_id(&mut self, session_id: &str) -> String {
        if let Some(existing) = self.by_session.get(session_id) {
            return existing.clone();
        }
        let session_ref = format!("session-ref-{}", Uuid::new_v4());
        self.by_session
            .insert(session_id.to_string(), session_ref.clone());
        self.by_ref
            .insert(session_ref.clone(), session_id.to_string());
        session_ref
    }

    fn session_id_for_ref(&self, session_ref: &str) -> Option<String> {
        self.by_ref.get(session_ref).cloned()
    }
}

static INTERACTIVE_SESSION_REFS: OnceLock<Mutex<InteractiveSessionRefMap>> = OnceLock::new();

fn interactive_session_refs() -> &'static Mutex<InteractiveSessionRefMap> {
    INTERACTIVE_SESSION_REFS.get_or_init(|| Mutex::new(InteractiveSessionRefMap::default()))
}

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>, name: &str) -> std::sync::MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(mutex = name, "mutex poisoned; recovering");
            poisoned.into_inner()
        }
    }
}

fn session_ref_for_session_id(session_id: &str) -> String {
    let mut map = lock_or_recover(interactive_session_refs(), "interactive session ref map");
    map.session_ref_for_session_id(session_id)
}

fn session_id_for_ref(session_ref: &str) -> Option<String> {
    let map = lock_or_recover(interactive_session_refs(), "interactive session ref map");
    map.session_id_for_ref(session_ref)
}

const INTERNAL_KEYS: [&str; 16] = [
    "child_session_id",
    "ctx_session_id",
    "host_id",
    "invocation_id",
    "message_id",
    "parent_session_id",
    "parent_turn_id",
    "plan_id",
    "provider_session_ref",
    "run_id",
    "session_id",
    "task_id",
    "tool_call_id",
    "turn_id",
    "workspace_id",
    "worktree_id",
];

fn scrub_internal_fields(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                scrub_internal_fields(item);
            }
        }
        Value::Object(obj) => {
            for key in INTERNAL_KEYS {
                if key == "run_id"
                    && obj
                        .get(key)
                        .and_then(|value| value.as_str())
                        .is_some_and(|value| value.starts_with("run_"))
                {
                    continue;
                }
                obj.remove(key);
            }
            for item in obj.values_mut() {
                scrub_internal_fields(item);
            }
        }
        _ => {}
    }
}

fn map_interactive_session(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    if let Some(session_id) = obj.remove("id") {
        if let Some(session_id) = session_id.as_str() {
            let session_ref = session_ref_for_session_id(session_id);
            obj.insert("session_ref".to_string(), Value::String(session_ref));
        } else {
            obj.insert("session_ref".to_string(), session_id);
        }
    }
    obj.remove("session_id");
    obj.remove("ctx_session_id");
    obj.remove("workspace_id");
    obj.remove("worktree_id");
}

fn map_interactive_sessions(value: &mut Value) {
    if let Some(items) = value.as_array_mut() {
        for item in items {
            map_interactive_session(item);
        }
        return;
    }
    map_interactive_session(value);
}
