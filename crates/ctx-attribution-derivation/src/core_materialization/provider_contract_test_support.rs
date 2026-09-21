//! Synthetic Core consumer witnesses, not provider captures or support qualification.
//! Tuples are literal copies of reviewed public emitters, not private policy constants.

use crate::protocol::{
    AgentScope, CoreRecord, EventIdentityInput, NativeItemKey, NativeSessionKey,
    ProviderNativeSessionRelationship, SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
    derive_event_id, derive_session_id,
};

pub const PROVIDERS: [&str; 6] = ["gemini", "mux", "openclaw", "crush", "opencode", "zed"];

pub fn provider_record(
    provider: &str,
    released: bool,
) -> Result<CoreRecord, Box<dyn std::error::Error>> {
    let (format, schema, old, current) = match provider {
        "gemini" => (
            "gemini_cli_chat_recording_jsonl",
            "gemini-nativepath-jsonl-v0",
            "gemini-nativepath-core-activity-v1",
            "gemini-nativepath-core-activity-v2-record-rejections",
        ),
        "mux" => (
            "mux_session_jsonl",
            "mux-session-tree-source-backed-v2",
            "mux-source-backed-v7-core-activity",
            "mux-source-backed-v16-explicit-root-only",
        ),
        "openclaw" => (
            "openclaw_session_jsonl_tree",
            "openclaw-legacy-jsonl-v2",
            "openclaw-source-backed-v13-core-activity",
            "openclaw-source-backed-v20-direct-parent-explicit-root",
        ),
        "crush" => (
            "crush_sqlite",
            "crush-project-sqlite-v0",
            "crush-sqlite-source-backed-v3-neutral-core",
            "crush-sqlite-source-backed-v5-record-rejections",
        ),
        "opencode" => (
            "opencode_sqlite",
            "opencode-family-message_part-v1",
            "opencode-family-source-backed-v9-neutral-core",
            "opencode-family-source-backed-v12-known-file-carriers",
        ),
        "zed" => (
            "zed_threads_sqlite",
            "zed-nativepath-sqlite-v0",
            "zed-nativepath-source-backed-v3-neutral-core",
            "zed-nativepath-source-backed-v5-neutral-core-agent-scope-optional-admission",
        ),
        _ => return Err(std::io::Error::other("unknown provider witness").into()),
    };
    let unresolved_gemini = provider == "gemini" && !released;
    let source = SourceKey::derive(
        provider,
        format,
        schema,
        if unresolved_gemini { 2 } else { 1 },
        SourceAnchor::provider_native("consumer-witness", TypedKey::utf8("source")?)?,
    )?;
    let session = |id: &str| {
        derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "session",
            native_session_key: &NativeSessionKey::native_id("session", TypedKey::utf8(id)?)?,
        })
    };
    let session_id = session("child")?;
    let parent = session("parent")?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "event",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::utf8("event")?)?,
        subrecord_selector: None,
    })?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source,
        1,
        "message",
        if released { old } else { current },
        "retained provider child",
    )?;
    record.provider_session_id = Some("child".to_owned());
    record.native_event_id = Some(TypedKey::utf8("event")?);
    record.agent_scope = Some(AgentScope::Subagent);
    if !unresolved_gemini {
        record.parent_session_id = Some(parent);
        record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    }
    if released && matches!(provider, "mux" | "openclaw") {
        record.root_session_id = Some(parent);
    }
    record.validate_contract()?;
    Ok(record)
}
