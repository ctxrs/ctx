use super::*;
use crate::common::json::{exact_bounded_string_alias, ExactJsonStringAlias};
use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use std::fmt;

pub(super) const TERMINAL_CALL_ID_DOMAIN: &[u8] = b"ctx/openclaw/terminal-call-id/v1\0";
pub(super) const TERMINAL_INVOCATION_ID_DOMAIN: &[u8] = b"ctx/openclaw/terminal-invocation-id/v1\0";

pub(super) type OpenClawTerminalAuthority = JsonlTerminalAuthority;

fn observe_terminal(
    authority: &mut OpenClawTerminalAuthority,
    call_id: &str,
    region: JsonlTerminalObservationRegion,
) {
    if !authority.exhausted() && !call_id.is_empty() {
        authority.observe(
            TERMINAL_CALL_ID_DOMAIN,
            call_id,
            region,
            MAX_TERMINAL_LINKAGE_IDS,
        );
    }
}

pub(super) fn observe_terminal_record(
    authority: &mut OpenClawTerminalAuthority,
    record: &[u8],
    region: JsonlTerminalObservationRegion,
) {
    if !crate::common::json::raw_object_keys_are_unique(record) {
        authority.observe_ambiguous_terminal();
    }
    let Ok(value) = serde_json::from_slice::<Value>(record) else {
        return;
    };
    for call in native_tool_calls(&value) {
        if let Some(call_id) = call.call_id {
            if !authority.exhausted() && !call_id.is_empty() {
                authority.observe(
                    TERMINAL_INVOCATION_ID_DOMAIN,
                    call_id,
                    region,
                    MAX_TERMINAL_LINKAGE_IDS,
                );
            }
        }
    }
    if let Some(result) = native_tool_result(&value) {
        if result.ambiguous_linkage {
            authority.observe_ambiguous_terminal();
        } else if let Some(call_id) = result.call_id {
            observe_terminal(authority, call_id, region);
        }
    }
}

pub(super) struct NativeToolCall<'a> {
    pub(super) block: &'a Value,
    pub(super) block_index: usize,
    pub(super) call_id: Option<&'a str>,
    pub(super) tool_name: Option<&'a str>,
    pub(super) command: Option<String>,
    pub(super) declared_workdir: Option<String>,
    pub(super) file_references: Vec<String>,
}

pub(super) struct NativeToolResult<'a> {
    pub(super) message: &'a Value,
    pub(super) call_id: Option<&'a str>,
    pub(super) ambiguous_linkage: bool,
    pub(super) output: &'a Value,
}

pub(super) fn native_tool_calls(value: &Value) -> Vec<NativeToolCall<'_>> {
    let message = value.get("message").unwrap_or(value);
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, block)| block.get("type").and_then(Value::as_str) == Some("toolCall"))
        .map(|(block_index, block)| native_tool_call_block(block, block_index))
        .collect()
}

fn native_tool_call_block(block: &Value, block_index: usize) -> NativeToolCall<'_> {
    let arguments = block.get("arguments").and_then(Value::as_object);
    let string = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| arguments?.get(*key).and_then(Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    let command = string(&["command"]);
    let declared_workdir = string(&["workdir", "cwd"]);
    let tool_name = block.get("name").and_then(Value::as_str);
    let file_references = ["path", "file_path", "filePath"]
        .into_iter()
        .filter_map(|key| arguments?.get(key).and_then(Value::as_str))
        .filter(|path| !path.is_empty() && path.len() <= 16 * 1024)
        .map(str::to_owned)
        .collect();
    NativeToolCall {
        block,
        block_index,
        call_id: block.get("id").and_then(Value::as_str),
        tool_name,
        command,
        declared_workdir,
        file_references,
    }
}

pub(super) fn native_tool_result(value: &Value) -> Option<NativeToolResult<'_>> {
    let message = value.get("message").unwrap_or(value);
    let role = message.get("role").and_then(Value::as_str)?;
    if !matches!(role, "tool" | "toolResult") {
        return None;
    }
    let details = message.get("details");
    let output = details
        .or_else(|| message.get("content"))
        .unwrap_or(message);
    let call_id = message
        .as_object()
        .map_or(ExactJsonStringAlias::Missing, |object| {
            exact_bounded_string_alias(
                object,
                &["toolCallId", "tool_call_id"],
                MAX_SELECTOR_CALL_ID_BYTES,
            )
        });
    Some(NativeToolResult {
        message,
        call_id: match call_id {
            ExactJsonStringAlias::Exact(call_id) => Some(call_id),
            ExactJsonStringAlias::Missing | ExactJsonStringAlias::Ambiguous => None,
        },
        ambiguous_linkage: matches!(call_id, ExactJsonStringAlias::Ambiguous),
        output,
    })
}

pub(super) struct CompoundAdmission {
    pub(super) index: Value,
    pub(super) index_file: Option<OpenedProviderSourceFile>,
    pub(super) native_session_family: OpenClawNativeSessionFamily,
}

fn openclaw_raw_lineage_is_ambiguous(path: &Path, bytes: &[u8]) -> serde_json::Result<bool> {
    let mut ambiguous = false;
    let agent_id = super::super::super::openclaw_agent_id(path);
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    OpenClawRawLineageSeed {
        ambiguous: &mut ambiguous,
        agent_id: agent_id.as_deref(),
    }
    .deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(ambiguous)
}

struct OpenClawRawLineageSeed<'state, 'agent> {
    ambiguous: &'state mut bool,
    agent_id: Option<&'agent str>,
}

impl<'de> DeserializeSeed<'de> for OpenClawRawLineageSeed<'_, '_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<(), D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(OpenClawRawLineageVisitor {
            ambiguous: self.ambiguous,
            agent_id: self.agent_id,
        })
    }
}

struct OpenClawRawLineageVisitor<'state, 'agent> {
    ambiguous: &'state mut bool,
    agent_id: Option<&'agent str>,
}

impl<'de> Visitor<'de> for OpenClawRawLineageVisitor<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an OpenClaw session index JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_str<E>(self, _: &str) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_string<E>(self, _: String) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_none<E>(self) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> std::result::Result<(), E> {
        Ok(())
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<(), D::Error>
    where
        D: Deserializer<'de>,
    {
        OpenClawRawLineageSeed {
            ambiguous: self.ambiguous,
            agent_id: self.agent_id,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<S>(self, mut sequence: S) -> std::result::Result<(), S::Error>
    where
        S: SeqAccess<'de>,
    {
        while sequence
            .next_element_seed(OpenClawRawLineageSeed {
                ambiguous: &mut *self.ambiguous,
                agent_id: self.agent_id,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<(), M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut spawned_by = OpenClawRawLineageClaim::default();
        let mut parent = OpenClawRawLineageClaim::default();
        let mut root = OpenClawRawLineageClaim::default();
        let mut session_id = OpenClawRawLineageClaim::default();
        let mut id = OpenClawRawLineageClaim::default();
        while let Some(key) = map.next_key::<String>()? {
            if let Some(kind) = openclaw_lineage_kind(&key) {
                let value = map.next_value::<Value>()?;
                let claim = match kind {
                    OpenClawRawLineageKind::SpawnedBy => &mut spawned_by,
                    OpenClawRawLineageKind::Parent => &mut parent,
                    OpenClawRawLineageKind::Root => &mut root,
                    OpenClawRawLineageKind::SessionId => &mut session_id,
                    OpenClawRawLineageKind::Id => &mut id,
                };
                claim.observe(value, kind, self.agent_id, self.ambiguous);
            } else {
                let nested_agent_id = openclaw_index_entry_agent_id(&key).or(self.agent_id);
                map.next_value_seed(OpenClawRawLineageSeed {
                    ambiguous: &mut *self.ambiguous,
                    agent_id: nested_agent_id,
                })?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct OpenClawRawLineageClaim {
    value: Option<String>,
    saw_null: bool,
}

impl OpenClawRawLineageClaim {
    fn observe(
        &mut self,
        value: Value,
        kind: OpenClawRawLineageKind,
        agent_id: Option<&str>,
        ambiguous: &mut bool,
    ) {
        match value {
            Value::String(claim) if !claim.trim().is_empty() => {
                let claim = kind.native_value(agent_id, &claim);
                if self.saw_null || self.value.as_ref().is_some_and(|current| current != &claim) {
                    *ambiguous = true;
                } else if self.value.is_none() {
                    self.value = Some(claim);
                }
            }
            Value::Null => {
                if self.saw_null || self.value.is_some() {
                    *ambiguous = true;
                }
                self.saw_null = true;
            }
            _ => *ambiguous = true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum OpenClawRawLineageKind {
    SpawnedBy,
    Parent,
    Root,
    SessionId,
    Id,
}

impl OpenClawRawLineageKind {
    fn native_value(self, agent_id: Option<&str>, value: &str) -> String {
        match self {
            Self::Parent | Self::Root => super::super::qualify_session_id(agent_id, value),
            Self::SpawnedBy | Self::SessionId | Self::Id => value.to_owned(),
        }
    }
}

fn openclaw_lineage_kind(key: &str) -> Option<OpenClawRawLineageKind> {
    Some(match key {
        "spawnedBy" => OpenClawRawLineageKind::SpawnedBy,
        "parentSessionId" | "parent_session_id" => OpenClawRawLineageKind::Parent,
        "rootSessionId" | "root_session_id" => OpenClawRawLineageKind::Root,
        "sessionId" => OpenClawRawLineageKind::SessionId,
        "id" => OpenClawRawLineageKind::Id,
        _ => return None,
    })
}

fn openclaw_index_entry_agent_id(key: &str) -> Option<&str> {
    key.strip_prefix("agent:")
        .and_then(|route| route.split(':').next())
        .filter(|agent_id| !agent_id.is_empty())
}

pub(super) fn admit_compound(
    authority: &ProviderSourceRoot,
    path: &Path,
    index_relative_path: &Path,
    transcript: Arc<OpenedProviderSourceFile>,
) -> Result<CompoundAdmission> {
    let index_file = match authority.open_file(index_relative_path) {
        Ok(index) => Some(index),
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let index_bytes = index_file
        .as_ref()
        .map(|index| index.read_all_bounded(MAX_OPENCLAW_SESSION_INDEX_BYTES))
        .transpose()?;
    if let Some(index) = &index_file {
        index.revalidate()?;
    }
    let native_session_family = match index_bytes.as_deref() {
        None => OpenClawNativeSessionFamily::Absent,
        Some(bytes) => match (
            serde_json::from_slice::<Value>(bytes),
            openclaw_raw_lineage_is_ambiguous(path, bytes),
        ) {
            (Ok(index), Ok(false)) => native_session_family(path, &index),
            (Ok(_), Ok(true)) | (Err(_), _) | (_, Err(_)) => OpenClawNativeSessionFamily::Invalid,
        },
    };
    let observation = super::super::super::OpenClawSessionObservation::from_admitted(
        path.to_path_buf(),
        transcript.metadata(),
        index_file
            .as_ref()
            .zip(index_bytes.as_deref())
            .map(|(index, bytes)| (index.metadata(), bytes)),
    )?;
    Ok(CompoundAdmission {
        index: observation.index,
        index_file,
        native_session_family,
    })
}

enum OpenClawSessionLineage {
    Root,
    Child {
        relationship: Option<ProviderNativeSessionRelationship>,
        parent_native_session_id: String,
        root_native_session_id: Option<String>,
    },
    Unknown,
}

fn resolve_session_lineage(
    agent_id: Option<&str>,
    native_session_id: &str,
    native_session_family: &OpenClawNativeSessionFamily,
    selected_index: &Value,
) -> OpenClawSessionLineage {
    let generic_parent = related_session_claim(
        selected_index,
        agent_id,
        &["parentSessionId", "parent_session_id"],
    );
    let generic_root = related_session_claim(
        selected_index,
        agent_id,
        &["rootSessionId", "root_session_id"],
    );
    match native_session_family {
        OpenClawNativeSessionFamily::Resolved {
            parent_native_session_id,
        } => {
            let contradictory = generic_parent.invalid
                || generic_root.invalid
                || parent_native_session_id == native_session_id
                || generic_root
                    .value
                    .as_ref()
                    .is_some_and(|root| root == native_session_id)
                || generic_parent
                    .value
                    .as_ref()
                    .is_some_and(|generic| generic != parent_native_session_id);
            if contradictory {
                OpenClawSessionLineage::Unknown
            } else {
                OpenClawSessionLineage::Child {
                    relationship: Some(ProviderNativeSessionRelationship::Delegated),
                    parent_native_session_id: parent_native_session_id.clone(),
                    root_native_session_id: generic_root.value,
                }
            }
        }
        OpenClawNativeSessionFamily::Invalid => OpenClawSessionLineage::Unknown,
        OpenClawNativeSessionFamily::Absent => {
            if generic_parent.invalid || generic_root.invalid {
                return OpenClawSessionLineage::Unknown;
            }
            let Some(parent_native_session_id) = generic_parent.value else {
                return if generic_root.value.is_none() {
                    OpenClawSessionLineage::Root
                } else {
                    OpenClawSessionLineage::Unknown
                };
            };
            let root_native_session_id = generic_root.value;
            if parent_native_session_id == native_session_id
                || root_native_session_id
                    .as_ref()
                    .is_some_and(|root| root == native_session_id)
            {
                return OpenClawSessionLineage::Unknown;
            }
            OpenClawSessionLineage::Child {
                relationship: None,
                parent_native_session_id,
                root_native_session_id,
            }
        }
    }
}

pub(super) struct SessionState {
    pub(super) provider_session_id: String,
    pub(super) agent_id: Option<String>,
    pub(super) parent_session_id: Option<StableEntityId>,
    pub(super) root_session_id: Option<StableEntityId>,
    pub(super) started_at: DateTime<Utc>,
    pub(super) cwd: Option<String>,
    pub(super) branch: Option<String>,
    pub(super) agent_scope: Option<AgentScope>,
    pub(super) relationship: Option<ProviderNativeSessionRelationship>,
}

impl SessionState {
    pub(super) fn new(
        path: &Path,
        native_session_id: &str,
        index: &Value,
        native_session_family: &OpenClawNativeSessionFamily,
        imported_at: DateTime<Utc>,
        direct_session_id: StableEntityId,
        source_anchor_scope: SourceAnchorScope,
    ) -> Result<Self> {
        let agent_id = super::super::super::openclaw_agent_id(path)
            .map(|value| super::super::capped_text(&value));
        let provider_session_id = native_session_id.to_owned();
        let lineage = resolve_session_lineage(
            agent_id.as_deref(),
            native_session_id,
            native_session_family,
            index,
        );
        let (agent_scope, relationship, parent_provider_session_id, root_provider_session_id) =
            match lineage {
                OpenClawSessionLineage::Root => (Some(AgentScope::Primary), None, None, None),
                OpenClawSessionLineage::Child {
                    relationship,
                    parent_native_session_id,
                    root_native_session_id,
                } => (
                    Some(AgentScope::Subagent),
                    relationship,
                    Some(parent_native_session_id),
                    root_native_session_id,
                ),
                OpenClawSessionLineage::Unknown => (None, None, None, None),
            };
        let parent_session_id = parent_provider_session_id
            .as_deref()
            .map(|related| {
                related_session_identity(
                    related,
                    native_session_id,
                    direct_session_id,
                    source_anchor_scope,
                )
            })
            .transpose()?;
        let root_session_id = root_provider_session_id
            .as_deref()
            .map(|related| {
                related_session_identity(
                    related,
                    native_session_id,
                    direct_session_id,
                    source_anchor_scope,
                )
            })
            .transpose()?;
        Ok(Self {
            provider_session_id,
            agent_id,
            parent_session_id,
            root_session_id,
            started_at: imported_at,
            cwd: None,
            branch: explicit_branch(index),
            agent_scope,
            relationship,
        })
    }

    pub(super) fn observe_header(&mut self, value: &Value) {
        if let Some(id) = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        {
            self.provider_session_id =
                super::super::qualify_session_id(self.agent_id.as_deref(), id);
        }
        self.started_at = provider_timestamp_value(value.get("timestamp"), self.started_at);
        self.cwd = value
            .get("cwd")
            .and_then(Value::as_str)
            .map(super::super::capped_text);
        self.branch = self.branch.clone().or_else(|| explicit_branch(value));
    }

    pub(super) fn restore(&mut self, checkpoint: SessionCheckpoint) {
        self.provider_session_id = checkpoint.provider_session_id;
        self.started_at = checkpoint.started_at;
        self.cwd = checkpoint.cwd;
        self.branch = checkpoint.branch;
    }

    pub(super) fn checkpoint(&self) -> SessionCheckpoint {
        SessionCheckpoint {
            provider_session_id: self.provider_session_id.clone(),
            started_at: self.started_at,
            cwd: self.cwd.clone(),
            branch: self.branch.clone(),
        }
    }
}
