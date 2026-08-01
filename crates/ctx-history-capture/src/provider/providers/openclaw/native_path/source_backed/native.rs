use super::*;

pub(super) struct NativeToolCall<'a> {
    pub(super) block: &'a Value,
    pub(super) call_id: Option<&'a str>,
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

pub(super) fn native_tool_call(value: &Value) -> Option<NativeToolCall<'_>> {
    let message = value.get("message").unwrap_or(value);
    let block = message
        .get("content")?
        .as_array()?
        .iter()
        .find(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"))?;
    let arguments = block.get("arguments").and_then(Value::as_object);
    let string = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| arguments?.get(*key).and_then(Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    let command = string(&["command"]);
    let declared_workdir = string(&["workdir", "cwd"]);
    let tool_name = block
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = match tool_name.to_ascii_lowercase().as_str() {
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
    Some(NativeToolCall {
        block,
        call_id: block.get("id").and_then(Value::as_str),
        command,
        declared_workdir,
        file_observations,
        process_session_id: arguments
            .and_then(|arguments| arguments.get("sessionId"))
            .and_then(Value::as_str),
    })
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
    pub(super) parent_native_session_id: Option<String>,
    pub(super) root_native_session_id: Option<String>,
}

pub(super) fn admit_compound(
    authority: &ProviderSourceRoot,
    path: &Path,
    index_relative_path: &Path,
    transcript: &OpenedProviderSourceFile,
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
    let observation = super::super::super::OpenClawSessionObservation::from_admitted(
        path.to_path_buf(),
        transcript.metadata(),
        index_file
            .as_ref()
            .zip(index_bytes.as_deref())
            .map(|(index, bytes)| (index.metadata(), bytes)),
    )?;
    let (parent_native_session_id, root_native_session_id) = index_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .map(|index| native_session_family(path, &index))
        .unwrap_or((None, None));
    Ok(CompoundAdmission {
        index: observation.index,
        index_file,
        parent_native_session_id,
        root_native_session_id,
    })
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
    pub(super) is_primary: bool,
}

impl SessionState {
    pub(super) fn new(
        path: &Path,
        native_session_id: &str,
        index: &Value,
        native_parent_session_id: Option<&str>,
        native_root_session_id: Option<&str>,
        imported_at: DateTime<Utc>,
        direct_session_id: StableEntityId,
    ) -> Result<Self> {
        let agent_id = super::super::super::openclaw_agent_id(path)
            .map(|value| super::super::capped_text(&value));
        let provider_session_id = native_session_id.to_owned();
        let parent_provider_session_id =
            native_parent_session_id.map(str::to_owned).or_else(|| {
                related_session_id(
                    index,
                    agent_id.as_deref(),
                    &["parentSessionId", "parent_session_id"],
                )
            });
        let root_provider_session_id = native_root_session_id
            .map(str::to_owned)
            .or_else(|| {
                related_session_id(
                    index,
                    agent_id.as_deref(),
                    &["rootSessionId", "root_session_id"],
                )
            })
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
            agent_type: if parent_session_id.is_some() {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            is_primary: parent_session_id.is_none(),
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
