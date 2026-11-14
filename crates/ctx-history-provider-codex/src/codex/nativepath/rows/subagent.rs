use super::{CompletedItemProjection, EventType, SourceBackedSemantic, Value};

pub(super) fn subagent_activity_semantic(
    item: &serde_json::Map<String, Value>,
) -> Result<SourceBackedSemantic, CompletedItemProjection> {
    // A closed shape prevents future binary/private fields from being retained
    // merely because this lifecycle discriminator is already recognized.
    if item.keys().any(|key| {
        !matches!(
            key.as_str(),
            "id" | "type"
                | "kind"
                | "agent_thread_id"
                | "agent_path"
                | "model"
                | "reasoning_effort"
        )
    }) {
        return Err(CompletedItemProjection::Unsupported);
    }
    let Some(kind) = item.get("kind").and_then(Value::as_str) else {
        return Err(CompletedItemProjection::Malformed);
    };
    if !matches!(kind, "started" | "interacted" | "interrupted" | "completed") {
        return Err(CompletedItemProjection::Unsupported);
    }
    let valid_thread = item
        .get("agent_thread_id")
        .and_then(Value::as_str)
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
    let valid_path = item
        .get("agent_path")
        .and_then(Value::as_str)
        .is_some_and(|path| !path.is_empty());
    let valid_optional = ["model", "reasoning_effort"].iter().all(|key| {
        item.get(*key)
            .is_none_or(|value| value.is_null() || value.is_string())
    });
    if !valid_thread || !valid_path || !valid_optional {
        return Err(CompletedItemProjection::Malformed);
    }
    Ok(SourceBackedSemantic {
        event_type: EventType::Notice,
        role: None,
        lexical_body: serde_json::to_string(item).expect("JSON values serialize"),
    })
}
