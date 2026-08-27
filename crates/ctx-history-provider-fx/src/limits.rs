use std::io::Read;

use crate::{
    CanonicalState, CredentialSource, FxProviderError, FxProviderResult, HistoryTurnKind,
    ProviderId, RecoveryCheckpoint, SessionPreferences, UsageSnapshot,
};

const MAX_WORKSPACE_ROOT_BYTES: usize = 4096;
const MAX_MODEL_BYTES: usize = 1024;
const MAX_CONVERSATION_LANGUAGE_BYTES: usize = 24;
pub const MAX_LEGACY_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayLimits {
    pub max_committed_bytes: u64,
    pub max_events: u64,
    pub max_turns: u64,
    pub max_replacement_decoded_bytes: u64,
    pub max_scratch_bytes: u64,
    pub max_legacy_snapshot_bytes: u64,
    pub max_tool_items: u64,
    pub max_images: u64,
    pub max_files: u64,
    pub max_nested_items: u64,
    pub max_string_bytes: u64,
    pub max_json_depth: u32,
}

impl Default for ReplayLimits {
    fn default() -> Self {
        Self {
            max_committed_bytes: 512 * 1024 * 1024,
            max_events: 1_000_000,
            max_turns: 100_000,
            max_replacement_decoded_bytes: 256 * 1024 * 1024,
            max_scratch_bytes: 256 * 1024 * 1024,
            max_legacy_snapshot_bytes: MAX_LEGACY_SNAPSHOT_BYTES,
            max_tool_items: 1_000_000,
            max_images: 100_000,
            max_files: 250_000,
            max_nested_items: 2_000_000,
            max_string_bytes: 256 * 1024 * 1024,
            max_json_depth: 64,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct JsonShape {
    pub string_bytes: u64,
    pub maximum_depth: u32,
    pub nested_items: u64,
}

pub(crate) fn inspect_json(bytes: &[u8], limits: ReplayLimits) -> FxProviderResult<JsonShape> {
    let mut inspector = JsonInspector::new(limits);
    inspector.feed(bytes)?;
    Ok(inspector.shape)
}

pub(crate) fn inspect_json_reader(
    reader: &mut dyn Read,
    limits: ReplayLimits,
) -> FxProviderResult<JsonShape> {
    let mut inspector = JsonInspector::new(limits);
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        inspector.feed(&buffer[..read])?;
    }
    Ok(inspector.shape)
}

struct JsonInspector {
    limits: ReplayLimits,
    shape: JsonShape,
    depth: u32,
    in_string: bool,
    escaped: bool,
}

impl JsonInspector {
    fn new(limits: ReplayLimits) -> Self {
        Self {
            limits,
            shape: JsonShape::default(),
            depth: 0,
            in_string: false,
            escaped: false,
        }
    }

    fn feed(&mut self, bytes: &[u8]) -> FxProviderResult<()> {
        for byte in bytes {
            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                    self.shape.string_bytes = self.shape.string_bytes.saturating_add(1);
                } else if *byte == b'\\' {
                    self.escaped = true;
                } else if *byte == b'"' {
                    self.in_string = false;
                } else {
                    self.shape.string_bytes = self.shape.string_bytes.saturating_add(1);
                }
                continue;
            }
            match *byte {
                b'"' => self.in_string = true,
                b'{' | b'[' => {
                    self.depth = self.depth.saturating_add(1);
                    self.shape.nested_items = self.shape.nested_items.saturating_add(1);
                    self.shape.maximum_depth = self.shape.maximum_depth.max(self.depth);
                    check_limit(
                        "JSON depth",
                        u64::from(self.shape.maximum_depth),
                        u64::from(self.limits.max_json_depth),
                    )?;
                }
                b',' => {
                    self.shape.nested_items = self.shape.nested_items.saturating_add(1);
                    check_limit(
                        "nested items",
                        self.shape.nested_items,
                        self.limits.max_nested_items,
                    )?;
                }
                b'}' | b']' => self.depth = self.depth.saturating_sub(1),
                _ => {}
            }
            check_limit(
                "JSON string bytes",
                self.shape.string_bytes,
                self.limits.max_string_bytes,
            )?;
            check_limit(
                "nested items",
                self.shape.nested_items,
                self.limits.max_nested_items,
            )?;
        }
        Ok(())
    }
}

pub fn validate_canonical_state(
    state: &CanonicalState,
    limits: ReplayLimits,
) -> FxProviderResult<()> {
    if !crate::dto::validate_session_id(&state.id) {
        return Err(FxProviderError::InvalidState("invalid native session id"));
    }
    validate_workspace_root(&state.origin_workspace_root)?;
    validate_workspace_root(&state.workspace_root)?;
    validate_conversation_language(&state.conversation_language)?;
    validate_preferences(&state.preferences)?;
    if state.created_at_ms < 0
        || state.updated_at_ms < 0
        || state.context_history_start > state.history.len() as u64
    {
        return Err(FxProviderError::InvalidState(
            "invalid canonical session metadata",
        ));
    }
    if let Some(work_id) = &state.last_subagent_work_id {
        crate::history::validate_work_id(work_id)?;
    }
    check_limit(
        "durable turns",
        state.history.len() as u64,
        limits.max_turns,
    )?;
    let mut tools = 0_u64;
    let mut images = 0_u64;
    let mut files = 0_u64;
    let mut nested = state.permission_state.rules.len() as u64;
    let mut strings = top_level_string_bytes(state);
    for (index, turn) in state.history.iter().enumerate() {
        if turn.kind() == HistoryTurnKind::CompactedSummary && index != 0 {
            return Err(FxProviderError::InvalidState(
                "compacted summary is not the leading turn",
            ));
        }
        let stats = turn.stats();
        tools = tools.saturating_add(stats.tool_count);
        images = images.saturating_add(stats.image_count);
        files = files.saturating_add(stats.file_count);
        nested = nested.saturating_add(stats.nested_items);
        strings = strings.saturating_add(stats.string_bytes);
    }
    if state.permission_state.schema_version != 2
        || state.permission_state.next_generation == 0
        || state.permission_state.rules.len() > 1024
    {
        return Err(FxProviderError::InvalidState("invalid permission state"));
    }
    for rule in &state.permission_state.rules {
        if rule.id == 0
            || rule.generation == 0
            || rule.id > rule.generation
            || rule.id >= state.permission_state.next_generation
            || rule.generation >= state.permission_state.next_generation
            || rule.canonical.accounted_bytes() == 0
            || rule.canonical.accounted_bytes() > 4096
            || rule.display_identity.accounted_bytes() == 0
            || rule.display_identity.accounted_bytes() > 4096
        {
            return Err(FxProviderError::InvalidState("invalid permission rule"));
        }
    }
    if let Some(usage) = &state.usage {
        validate_usage_snapshot(usage)?;
        nested = nested
            .saturating_add(usage.models.len() as u64)
            .saturating_add(usage.pending.len() as u64);
        for model in &usage.models {
            if model.model.is_empty()
                || model.model.len() > 1024
                || !model.total_cost.is_finite()
                || model.total_cost < 0.0
            {
                return Err(FxProviderError::InvalidState("invalid usage model"));
            }
            strings = strings.saturating_add(model.model.len() as u64);
        }
    }
    if let Some(checkpoint) = &state.recovery_checkpoint {
        validate_recovery_checkpoint(checkpoint)?;
        images = images.saturating_add(checkpoint.user.images.len() as u64);
        tools = tools.saturating_add(checkpoint.execution.aggregate_items());
        files = files.saturating_add(checkpoint.execution.files.len() as u64);
        nested = nested
            .saturating_add(checkpoint.user.images.len() as u64)
            .saturating_add(checkpoint.execution.aggregate_items());
    }
    check_limit("tool items", tools, limits.max_tool_items)?;
    check_limit("images", images, limits.max_images)?;
    check_limit("files", files, limits.max_files)?;
    check_limit("nested items", nested, limits.max_nested_items)?;
    check_limit("JSON string bytes", strings, limits.max_string_bytes)
}

pub(crate) fn validate_workspace_root(value: &str) -> FxProviderResult<()> {
    if value.is_empty()
        || value.len() > MAX_WORKSPACE_ROOT_BYTES
        || !std::path::Path::new(value).is_absolute()
    {
        return Err(FxProviderError::InvalidState("invalid workspace root"));
    }
    Ok(())
}

pub(crate) fn validate_preferences(value: &SessionPreferences) -> FxProviderResult<()> {
    validate_model(&value.model)?;
    if !valid_effort(&value.effort) {
        return Err(FxProviderError::InvalidState("invalid reasoning effort"));
    }
    Ok(())
}

fn validate_model(value: &str) -> FxProviderResult<()> {
    if value.is_empty()
        || value.len() > MAX_MODEL_BYTES
        || value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_whitespace)
        || value.as_bytes().last().is_some_and(u8::is_ascii_whitespace)
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(FxProviderError::InvalidState("invalid model preference"));
    }
    Ok(())
}

fn valid_effort(value: &str) -> bool {
    if value.eq_ignore_ascii_case("auto")
        || value.eq_ignore_ascii_case("adaptive")
        || value.eq_ignore_ascii_case("default")
    {
        return true;
    }
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(crate) fn validate_conversation_language(value: &str) -> FxProviderResult<()> {
    let trimmed = value.trim_matches([' ', '\t', '\r', '\n']);
    if value.is_empty()
        || value.len() > MAX_CONVERSATION_LANGUAGE_BYTES
        || value != trimmed
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(FxProviderError::InvalidState(
            "invalid conversation language",
        ));
    }
    Ok(())
}

pub(crate) fn validate_usage_snapshot(usage: &UsageSnapshot) -> FxProviderResult<()> {
    if !usage.total_cost.is_finite()
        || usage.total_cost < 0.0
        || usage.next_sequence == 0
        || usage.models.len() > 32
        || usage.pending.len() > 16
    {
        return Err(FxProviderError::InvalidState("invalid usage snapshot"));
    }
    for model in &usage.models {
        validate_model(&model.model)?;
        if !model.total_cost.is_finite() || model.total_cost < 0.0 {
            return Err(FxProviderError::InvalidState("invalid usage model"));
        }
    }
    Ok(())
}

pub(crate) fn validate_recovery_checkpoint(
    checkpoint: &RecoveryCheckpoint,
) -> FxProviderResult<()> {
    checkpoint.execution.validate()?;
    if checkpoint.version != 2
        || checkpoint.turn_id == 0
        || checkpoint.max_provider_attempts == 0
        || checkpoint.consumed_provider_attempts > checkpoint.max_provider_attempts
        || (checkpoint.outstanding_reservation
            && checkpoint.consumed_provider_attempts >= checkpoint.max_provider_attempts)
    {
        return Err(FxProviderError::InvalidState("invalid recovery checkpoint"));
    }
    if let Some(work_id) = &checkpoint.user.work_id {
        crate::history::validate_work_id(work_id)?;
    }
    match &checkpoint.authority.model {
        crate::DurableBytes::Utf8(model) => validate_model(model)?,
        crate::DurableBytes::NonUtf8Base64(_) => {
            return Err(FxProviderError::InvalidState("recovery model is not UTF-8"));
        }
    }
    if checkpoint.authority.credential_identity.is_some()
        && checkpoint.authority.credential_source.is_none()
    {
        return Err(FxProviderError::InvalidState(
            "credential identity has no source",
        ));
    }
    if let Some(source) = checkpoint.authority.credential_source {
        let authorized = match checkpoint.authority.provider {
            ProviderId::Gateway => !matches!(
                source,
                CredentialSource::ChatgptSubscription | CredentialSource::GrokSubscription
            ),
            ProviderId::Codex => source == CredentialSource::ChatgptSubscription,
            ProviderId::Grok => source == CredentialSource::GrokSubscription,
        };
        if !authorized {
            return Err(FxProviderError::InvalidState(
                "provider does not authorize credential source",
            ));
        }
    }
    Ok(())
}

fn top_level_string_bytes(state: &CanonicalState) -> u64 {
    [
        state.id.len(),
        state.origin_workspace_root.len(),
        state.workspace_root.len(),
        state.conversation_language.len(),
        state.preferences.model.len(),
        state.preferences.effort.len(),
        state.last_subagent_work_id.as_ref().map_or(0, String::len),
    ]
    .into_iter()
    .fold(0_u64, |total, value| total.saturating_add(value as u64))
}

pub(crate) fn check_limit(
    resource: &'static str,
    actual: u64,
    maximum: u64,
) -> FxProviderResult<()> {
    if actual > maximum {
        return Err(FxProviderError::LimitExceeded {
            resource,
            actual,
            maximum,
        });
    }
    Ok(())
}
