#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceProgress {
    pub source: SourceKey,
    pub source_epoch: u64,
    pub certified_revision_sha256: String,
    pub frontier: Option<SourceFrontier>,
    pub materializer_revision: String,
    pub terminal: bool,
}

impl SourceProgress {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("source progress identity", error))?;
        if self.source_epoch == 0 {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source progress epoch must be positive",
            ));
        }
        validate_sha256(&self.certified_revision_sha256, "certified source revision")?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        if let Some(frontier) = &self.frontier {
            frontier
                .validate_contract()
                .map_err(|error| invalid_contract("source progress frontier", error))?;
        }
        Ok(())
    }

    #[must_use]
    pub fn exact_eq(&self, other: &Self) -> bool {
        self.source.exact_descriptor_eq(&other.source)
            && self.source_epoch == other.source_epoch
            && self.certified_revision_sha256 == other.certified_revision_sha256
            && self.frontier == other.frontier
            && self.materializer_revision == other.materializer_revision
            && self.terminal == other.terminal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceDisposition {
    NewSource,
    Resume,
    Rewrite,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransientSourceContent(String);

impl TransientSourceContent {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        (bytes.len() <= MAX_SOURCE_CONTENT_BYTES).then(|| Self(STANDARD.encode(bytes)))
    }

    pub fn decode(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.0.len() > MAX_SOURCE_ENCODED_CONTENT_BYTES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "transient source content exceeds its encoded byte bound",
            ));
        }
        let decoded = STANDARD.decode(&self.0).map_err(|_| {
            ProtocolError::new(
                ErrorClass::InvalidRequest,
                "transient source content is not canonical base64",
            )
        })?;
        if decoded.len() > MAX_SOURCE_CONTENT_BYTES || STANDARD.encode(&decoded) != self.0 {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "transient source content exceeds its decoded bound or is not canonical base64",
            ));
        }
        Ok(decoded)
    }

    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for TransientSourceContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransientSourceContent")
            .field("encoded_bytes", &self.0.len())
            .field("content", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMessageFact {
    pub content: TransientSourceContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCommandFact {
    pub call_id: Option<String>,
    pub tool_name: Option<String>,
    pub command: TransientSourceContent,
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceOutcome {
    Success,
    Failure,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceResultFact {
    pub call_id: Option<String>,
    pub outcome: SourceOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub content: TransientSourceContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TransientSourceFact {
    Message(SourceMessageFact),
    Command(SourceCommandFact),
    Result(SourceResultFact),
}

impl TransientSourceFact {
    fn validate_and_count_bytes(&self) -> Result<usize, ProtocolError> {
        match self {
            Self::Message(fact) => fact.content.decode().map(|content| content.len()),
            Self::Command(fact) => {
                validate_optional_identity(fact.call_id.as_deref(), "source command call ID")?;
                validate_optional_identity(fact.tool_name.as_deref(), "source command tool name")?;
                validate_optional_path(
                    fact.working_directory.as_deref(),
                    "source command working directory",
                )?;
                fact.command.decode().map(|content| content.len())
            }
            Self::Result(fact) => {
                validate_optional_identity(fact.call_id.as_deref(), "source result call ID")?;
                fact.content.decode().map(|content| content.len())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSessionRelationships {
    pub direct_session_id: StableEntityId,
    pub root_session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub provider_session_id: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceWorktreeRootLocator {
    pub absolute_path: String,
}

impl SourceWorktreeRootLocator {
    pub fn new(absolute_path: String) -> Result<Self, ProtocolError> {
        let locator = Self { absolute_path };
        locator.validate()?;
        Ok(locator)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_path(
            &self.absolute_path,
            "source repository worktree-root locator",
        )?;
        let bytes = self.absolute_path.as_bytes();
        let unix_absolute = bytes.first() == Some(&b'/');
        let windows_drive_absolute = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\');
        let windows_unc_absolute =
            bytes.starts_with(br"\\") || bytes.starts_with(b"//");
        if !(unix_absolute || windows_drive_absolute || windows_unc_absolute)
            || self.absolute_path.chars().any(char::is_control)
            || self
                .absolute_path
                .split(['/', '\\'])
                .any(|component| matches!(component, "." | ".."))
        {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source repository worktree-root locator must be an absolute normalized path",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRepositoryContext {
    pub repository_id: String,
    pub checkout_id: Option<String>,
    pub worktree_id: Option<String>,
    pub object_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_root: Option<SourceWorktreeRootLocator>,
}

impl SourceRepositoryContext {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_identity(&self.repository_id, "source repository ID")?;
        validate_optional_identity(self.checkout_id.as_deref(), "source checkout ID")?;
        validate_optional_identity(self.worktree_id.as_deref(), "source worktree ID")?;
        validate_optional_identity(self.object_format.as_deref(), "source object format")?;
        if let Some(locator) = &self.worktree_root {
            locator.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecordMetadata {
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub touched_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub locator: SourceRecordLocator,
    pub relationships: SourceSessionRelationships,
    pub repository: Option<SourceRepositoryContext>,
    pub metadata: SourceRecordMetadata,
    pub facts: Vec<TransientSourceFact>,
}

impl SourceRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_id: StableEntityId,
        session_id: StableEntityId,
        locator: SourceRecordLocator,
        relationships: SourceSessionRelationships,
        repository: Option<SourceRepositoryContext>,
        metadata: SourceRecordMetadata,
        facts: Vec<TransientSourceFact>,
    ) -> Result<Self, ProtocolError> {
        let record = Self {
            event_id,
            session_id,
            locator,
            relationships,
            repository,
            metadata,
            facts,
        };
        record.validate_and_count_bytes()?;
        Ok(record)
    }

    fn validate_and_count_bytes(&self) -> Result<usize, ProtocolError> {
        let event = EventHydrationRequest::new(self.event_id, self.locator.clone())
            .map_err(|error| invalid_contract("source record event locator", error))?;
        SessionHydrationRequest::new(self.session_id, vec![event])
            .map_err(|error| invalid_contract("source record session locator", error))?;
        validate_session_id_for_locator(
            self.relationships.direct_session_id,
            &self.locator,
            "direct session",
        )?;
        if self.relationships.direct_session_id != self.session_id {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source record direct session must equal its session ID",
            ));
        }
        validate_session_id(self.relationships.root_session_id, "root session")?;
        if let Some(parent) = self.relationships.parent_session_id {
            validate_session_id(parent, "parent session")?;
        }
        validate_optional_identity(
            self.relationships.provider_session_id.as_deref(),
            "provider session ID",
        )?;
        validate_optional_identity(self.relationships.agent_id.as_deref(), "source agent ID")?;
        if let Some(repository) = &self.repository {
            repository.validate()?;
        }
        validate_identity(&self.metadata.event_type, "source event type")?;
        validate_optional_identity(self.metadata.role.as_deref(), "source event role")?;
        validate_optional_path(self.metadata.workspace.as_deref(), "source workspace")?;
        validate_optional_path(self.metadata.cwd.as_deref(), "source working directory")?;
        if self.metadata.touched_files.len() > MAX_SOURCE_TOUCHED_FILES_PER_RECORD {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source record exceeds its touched-file count bound",
            ));
        }
        for path in &self.metadata.touched_files {
            validate_path(path, "source touched-file path")?;
        }
        if self.facts.len() > MAX_SOURCE_FACTS_PER_RECORD {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source record exceeds its detector-fact count bound",
            ));
        }
        self.facts.iter().try_fold(0_usize, |total, fact| {
            total
                .checked_add(fact.validate_and_count_bytes()?)
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorClass::Bounds,
                        "source record transient-content byte total overflowed",
                    )
                })
        })
    }
}
