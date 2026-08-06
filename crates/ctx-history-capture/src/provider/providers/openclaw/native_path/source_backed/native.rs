use super::*;

const TERMINAL_AUTHORITY_POLICY_REVISION: &str = "openclaw-terminal-result-authority-v1";
const TERMINAL_CALL_ID_DOMAIN: &[u8] = b"ctx/openclaw/terminal-call-id/v1\0";
const TERMINAL_AMBIGUITY_FINGERPRINT_DOMAIN: &[u8] =
    b"ctx/openclaw/terminal-ambiguity-fingerprint/v1\0";

#[derive(Debug)]
pub(super) struct OpenClawTerminalAuthority {
    call_ids: HashMap<[u8; 32], u8>,
    exhausted: bool,
    complete: bool,
}

impl OpenClawTerminalAuthority {
    pub(super) fn for_scan() -> Self {
        Self {
            call_ids: HashMap::new(),
            exhausted: false,
            complete: true,
        }
    }

    #[cfg(test)]
    pub(super) fn unscanned_for_test() -> Self {
        Self {
            call_ids: HashMap::new(),
            exhausted: false,
            complete: false,
        }
    }

    fn observe(&mut self, call_id: &str) {
        if self.exhausted || call_id.is_empty() {
            return;
        }
        let digest = terminal_call_id_digest(call_id);
        if !self.call_ids.contains_key(&digest) && self.call_ids.len() >= MAX_TERMINAL_CALL_IDS {
            self.call_ids.clear();
            self.exhausted = true;
            return;
        }
        let count = self.call_ids.entry(digest).or_default();
        *count = count.saturating_add(1).min(2);
    }

    fn observe_ambiguous_terminal(&mut self) {
        self.call_ids.clear();
        self.exhausted = true;
    }

    fn observe_record(&mut self, record: &[u8]) {
        let exact_json_authority = crate::common::json::raw_object_keys_are_unique(record);
        let Ok(value) = serde_json::from_slice::<Value>(record) else {
            self.observe_ambiguous_terminal();
            return;
        };
        let result = native_tool_result(&value);
        if !exact_json_authority && result.is_some() {
            self.observe_ambiguous_terminal();
        }
        if let Some(call_id) = result.and_then(|result| result.call_id) {
            self.observe(call_id);
        }
    }

    pub(super) fn is_unique(&self, call_id: &str) -> bool {
        if !self.complete {
            return true;
        }
        !self.exhausted
            && self
                .call_ids
                .get(&terminal_call_id_digest(call_id))
                .is_some_and(|count| *count == 1)
    }

    pub(super) fn ambiguity_fingerprint(&self) -> [u8; 32] {
        let mut ambiguous = self
            .call_ids
            .iter()
            .filter_map(|(digest, count)| (*count > 1).then_some(*digest))
            .collect::<Vec<_>>();
        ambiguous.sort_unstable();
        let mut hasher = Sha256::new();
        hasher.update(TERMINAL_AMBIGUITY_FINGERPRINT_DOMAIN);
        hasher.update([u8::from(self.exhausted)]);
        for digest in ambiguous {
            hasher.update(digest);
        }
        hasher.finalize().into()
    }
}

fn terminal_call_id_digest(call_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TERMINAL_CALL_ID_DOMAIN);
    hasher.update(call_id.as_bytes());
    hasher.finalize().into()
}

pub(super) fn terminal_authority_for_source(
    source: &SourceKey,
    source_path: &Path,
    source_file: Arc<OpenedProviderSourceFile>,
) -> Result<OpenClawTerminalAuthority> {
    let identity = JsonlSourceIdentity::new(
        CaptureProvider::OpenClaw.as_str(),
        PARSER_REVISION,
        TERMINAL_AUTHORITY_POLICY_REVISION,
        source.exact_descriptor_digest(),
        source_path,
    );
    let mut reader = JsonlReader::open(identity, source_file, None, None)?;
    let mut authority = OpenClawTerminalAuthority::for_scan();
    while reader
        .visit_page(&mut |record| -> Result<()> {
            authority.observe_record(record.bytes());
            Ok(())
        })?
        .is_some()
    {}
    if reader.outcome().is_none() {
        return Err(CaptureError::SystemInvariant(
            "OpenClaw terminal authority scan has no terminal evidence",
        ));
    }
    Ok(authority)
}

#[cfg(test)]
pub(super) fn terminal_authority_for_values<'a>(
    values: impl IntoIterator<Item = &'a Value>,
) -> OpenClawTerminalAuthority {
    let mut authority = OpenClawTerminalAuthority::for_scan();
    for value in values {
        if let Some(call_id) = native_tool_result(value).and_then(|result| result.call_id) {
            authority.observe(call_id);
        }
    }
    authority
}

#[cfg(test)]
pub(super) fn terminal_authority_for_records<'a>(
    records: impl IntoIterator<Item = &'a [u8]>,
) -> OpenClawTerminalAuthority {
    let mut authority = OpenClawTerminalAuthority::for_scan();
    for record in records {
        authority.observe_record(record);
    }
    authority
}

pub(super) struct NativeToolCall<'a> {
    pub(super) block: &'a Value,
    pub(super) block_index: usize,
    pub(super) call_id: Option<&'a str>,
    pub(super) tool_name: Option<&'a str>,
    pub(super) command: Option<String>,
    pub(super) declared_workdir: Option<String>,
    pub(super) file_observations: Vec<UnscopedFileObservation>,
    pub(super) process_session_id: Option<&'a str>,
}

pub(super) struct NativeToolResult<'a> {
    pub(super) message: &'a Value,
    pub(super) call_id: Option<&'a str>,
    pub(super) output: &'a Value,
    pub(super) structured_commit_oid: Option<&'a str>,
    pub(super) output_workdir: Option<&'a str>,
    pub(super) running_process_session_id: Option<&'a str>,
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
    let kind = match tool_name.unwrap_or_default().to_ascii_lowercase().as_str() {
        "read" | "read_file" | "grep" | "glob" | "search" => RepositoryFileObservationKind::Read,
        "edit" | "edit_file" | "apply_patch" => RepositoryFileObservationKind::Modified,
        "write" | "write_file" => RepositoryFileObservationKind::Unknown,
        _ => RepositoryFileObservationKind::Unknown,
    };
    let file_observations = ["path", "file_path", "filePath"]
        .into_iter()
        .filter_map(|key| arguments?.get(key).and_then(Value::as_str))
        .filter(|path| !path.trim().is_empty() && path.len() <= 16 * 1024)
        .take(64)
        .map(|path| UnscopedFileObservation {
            path: path.to_owned(),
            prior_path: None,
            kind,
        })
        .collect();
    NativeToolCall {
        block,
        block_index,
        call_id: block.get("id").and_then(Value::as_str),
        tool_name,
        command,
        declared_workdir,
        file_observations,
        process_session_id: arguments
            .and_then(|arguments| arguments.get("sessionId"))
            .and_then(Value::as_str),
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
    let structured_commit_oid = details.and_then(|details| {
        details
            .get("commit_oid")
            .or_else(|| details.get("commitOid"))
            .and_then(Value::as_str)
    });
    Some(NativeToolResult {
        message,
        call_id: message
            .get("toolCallId")
            .or_else(|| message.get("tool_call_id"))
            .and_then(Value::as_str),
        output,
        structured_commit_oid,
        output_workdir: details
            .and_then(|details| details.get("cwd"))
            .and_then(Value::as_str),
        running_process_session_id: details
            .filter(|details| details.get("status").and_then(Value::as_str) == Some("running"))
            .and_then(|details| details.get("sessionId"))
            .and_then(Value::as_str),
    })
}

pub(super) struct CompoundAdmission {
    pub(super) index: Value,
    pub(super) index_file: Option<OpenedProviderSourceFile>,
    pub(super) native_session_family: OpenClawNativeSessionFamily,
    pub(super) terminal_authority: OpenClawTerminalAuthority,
}

pub(super) fn admit_compound(
    authority: &ProviderSourceRoot,
    path: &Path,
    index_relative_path: &Path,
    transcript: Arc<OpenedProviderSourceFile>,
    source: &SourceKey,
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
    let native_session_family = index_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .map(|index| native_session_family(path, &index))
        .unwrap_or(OpenClawNativeSessionFamily::Absent);
    let terminal_authority = terminal_authority_for_source(source, path, Arc::clone(&transcript))?;
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
        terminal_authority,
    })
}

struct OpenClawSessionLineage {
    relationship: SessionRelationshipKind,
    parent_native_session_id: Option<String>,
    root_native_session_id: Option<String>,
}

fn resolve_session_lineage(
    agent_id: Option<&str>,
    native_session_family: &OpenClawNativeSessionFamily,
    selected_index: &Value,
) -> Result<OpenClawSessionLineage> {
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
            root_native_session_id,
        } => {
            let contradictory = generic_parent.invalid
                || generic_root.invalid
                || generic_parent
                    .value
                    .as_ref()
                    .is_some_and(|generic| generic != parent_native_session_id)
                || generic_root
                    .value
                    .as_ref()
                    .is_some_and(|generic| generic != root_native_session_id);
            Ok(OpenClawSessionLineage {
                relationship: if contradictory {
                    SessionRelationshipKind::RelatedUnknown
                } else {
                    SessionRelationshipKind::Delegated
                },
                parent_native_session_id: Some(parent_native_session_id.clone()),
                root_native_session_id: Some(root_native_session_id.clone()),
            })
        }
        OpenClawNativeSessionFamily::Absent | OpenClawNativeSessionFamily::Invalid => {
            let Some(parent_native_session_id) = generic_parent.value else {
                if matches!(native_session_family, OpenClawNativeSessionFamily::Invalid)
                    || generic_parent.invalid
                    || generic_root.invalid
                    || generic_root.value.is_some()
                {
                    return Err(CaptureError::InvalidPayload(
                        "OpenClaw session has invalid lineage without a resolvable parent"
                            .to_owned(),
                    ));
                }
                return Ok(OpenClawSessionLineage {
                    relationship: SessionRelationshipKind::Root,
                    parent_native_session_id: None,
                    root_native_session_id: None,
                });
            };
            let root_native_session_id = generic_root
                .value
                .unwrap_or_else(|| parent_native_session_id.clone());
            Ok(OpenClawSessionLineage {
                relationship: SessionRelationshipKind::RelatedUnknown,
                parent_native_session_id: Some(parent_native_session_id),
                root_native_session_id: Some(root_native_session_id),
            })
        }
    }
}

pub(super) struct SessionState {
    pub(super) provider_session_id: String,
    pub(super) agent_id: Option<String>,
    pub(super) parent_session_id: Option<StableEntityId>,
    pub(super) root_session_id: StableEntityId,
    pub(super) started_at: DateTime<Utc>,
    pub(super) cwd: Option<String>,
    pub(super) branch: Option<String>,
    pub(super) agent_type: AgentType,
    pub(super) relationship: SessionRelationshipKind,
}

impl SessionState {
    pub(super) fn new(
        path: &Path,
        native_session_id: &str,
        index: &Value,
        native_session_family: &OpenClawNativeSessionFamily,
        imported_at: DateTime<Utc>,
        direct_session_id: StableEntityId,
    ) -> Result<Self> {
        let agent_id = super::super::super::openclaw_agent_id(path)
            .map(|value| super::super::capped_text(&value));
        let provider_session_id = native_session_id.to_owned();
        let lineage = resolve_session_lineage(agent_id.as_deref(), native_session_family, index)?;
        let parent_provider_session_id = lineage.parent_native_session_id;
        let relationship = lineage.relationship;
        let root_provider_session_id = lineage
            .root_native_session_id
            .or_else(|| parent_provider_session_id.clone());
        let parent_session_id = parent_provider_session_id
            .as_deref()
            .map(|related| related_session_identity(related, native_session_id, direct_session_id))
            .transpose()?;
        let root_session_id = root_provider_session_id
            .as_deref()
            .map(|related| related_session_identity(related, native_session_id, direct_session_id))
            .transpose()?
            .or(parent_session_id)
            .unwrap_or(direct_session_id);
        Ok(Self {
            provider_session_id,
            agent_id,
            parent_session_id,
            root_session_id,
            started_at: imported_at,
            cwd: None,
            branch: explicit_branch(index),
            agent_type: if relationship == SessionRelationshipKind::Delegated {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
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
