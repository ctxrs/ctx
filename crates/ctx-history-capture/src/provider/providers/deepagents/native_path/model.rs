use super::*;

pub(super) const DEEPAGENTS_NATIVE_CURSOR_VERSION: u32 = 1;
pub(super) const DEEPAGENTS_OUTPUT_FRONTIER_VERSION: u32 = 1;
pub(super) const DEEPAGENTS_NATIVE_PARSER_REVISION: &str = "deepagents-nativepath-sqlite-v1";
pub(super) const DEEPAGENTS_NATIVE_POLICY_REVISION: &str = "deepagents-core-private-output-v1";
pub(super) const DEEPAGENTS_OUTPUT_PARSER_REVISION: &str = "deepagents-native-output-v1";
pub(super) const DEEPAGENTS_PAGE_UNITS: usize = 48;
pub(super) const DEEPAGENTS_RETIREMENT_UNITS: usize = 48;
pub(super) const DEEPAGENTS_PAGE_OVERHEAD_BYTES: usize = 256 * 1024;
pub(super) const DEEPAGENTS_PUBLICATION_DOMAIN: &[u8] = b"ctx-deepagents-native-publication-v1\0";

#[derive(Debug)]
pub(super) struct DeepAgentsSourceAuthority {
    pub(super) configured_source_root: PathBuf,
    pub(super) database_path: PathBuf,
    pub(super) canonical_database_path: PathBuf,
    pub(super) route_identity: String,
    pub(super) cursor_stream: String,
    pub(super) proposed_source_identity: String,
    pub(super) canonical_source_identity: String,
    pub(super) source_revision: String,
    pub(super) schema_fingerprint: String,
    pub(super) sqlite_user_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "phase")]
pub(super) enum DeepAgentsCorePhase {
    Threads {
        after_rowid: Option<i64>,
    },
    Writes {
        after_rowid: Option<i64>,
        active_rowid: Option<i64>,
        next_message_offset: u32,
        current_thread_id: Option<String>,
        next_event_index: u64,
    },
    StageSources {
        next_source: usize,
    },
    Retire {
        after: Option<SerializableRetirementFrontier>,
    },
    Complete,
    MissingStage {
        next_source: usize,
    },
    MissingRetire {
        after: Option<SerializableRetirementFrontier>,
    },
    MissingComplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SerializableRetirementFrontier {
    pub(super) kind: String,
    pub(super) id: Uuid,
}

impl SerializableRetirementFrontier {
    pub(super) fn from_store(value: NativePathSourceEntityFrontier) -> Self {
        Self {
            kind: value.kind.as_str().to_owned(),
            id: value.id,
        }
    }

    pub(super) fn to_store(&self) -> Result<NativePathSourceEntityFrontier> {
        let kind = match self.kind.as_str() {
            "session" => NativePathSourceEntityKind::Session,
            "session_edge" => NativePathSourceEntityKind::SessionEdge,
            "run" => NativePathSourceEntityKind::Run,
            "event" => NativePathSourceEntityKind::Event,
            "file_touch" => NativePathSourceEntityKind::FileTouch,
            _ => {
                return Err(CaptureError::InvalidPayload(
                    "Deep Agents retirement cursor has an unsupported entity kind".to_owned(),
                ));
            }
        };
        Ok(NativePathSourceEntityFrontier { kind, id: self.id })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeepAgentsNativeCursor {
    pub(super) version: u32,
    pub(super) parser_revision: String,
    pub(super) policy_revision: String,
    pub(super) route_identity: String,
    pub(super) canonical_source_identity: String,
    pub(super) source_revision: String,
    pub(super) schema_fingerprint: String,
    pub(super) generation: u64,
    pub(super) generation_staged: bool,
    pub(super) accepted_sessions: u64,
    pub(super) accepted_events: u64,
    pub(super) rejected_records: u64,
    #[serde(default)]
    pub(super) rejections: Vec<ProviderImportFailure>,
    pub(super) phase: DeepAgentsCorePhase,
}

impl DeepAgentsNativeCursor {
    pub(super) fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }

    pub(super) fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded)?;
        if cursor.version != DEEPAGENTS_NATIVE_CURSOR_VERSION
            || cursor.parser_revision != DEEPAGENTS_NATIVE_PARSER_REVISION
            || cursor.policy_revision != DEEPAGENTS_NATIVE_POLICY_REVISION
            || cursor.route_identity.is_empty()
            || cursor.canonical_source_identity.is_empty()
            || cursor.source_revision.is_empty()
            || cursor.schema_fingerprint.is_empty()
        {
            return Err(CaptureError::InvalidPayload(
                "Deep Agents NativePath cursor is unsupported or incomplete".to_owned(),
            ));
        }
        if cursor.rejections.len() > crate::summaries::MAX_RETAINED_PROVIDER_FAILURES {
            return Err(CaptureError::InvalidPayload(
                "Deep Agents NativePath cursor retains too many rejection details".to_owned(),
            ));
        }
        Ok(cursor)
    }

    pub(super) fn is_complete(&self) -> bool {
        matches!(
            self.phase,
            DeepAgentsCorePhase::Complete | DeepAgentsCorePhase::MissingComplete
        )
    }
}

#[derive(Debug)]
pub(super) struct DeepAgentsThreadPage {
    pub(super) entries: Vec<DeepAgentsThreadEntry>,
    pub(super) next_after_rowid: Option<i64>,
    pub(super) terminal: bool,
    pub(super) retained_bytes: usize,
}

#[derive(Debug)]
pub(super) struct DeepAgentsThreadEntry {
    pub(super) rowid: i64,
    pub(super) summary: Option<DeepAgentsThreadSummary>,
    pub(super) rejection: Option<String>,
}

#[derive(Debug)]
pub(super) struct DeepAgentsWritePage {
    pub(super) key: Option<DeepAgentsWriteKey>,
    pub(super) rowid: Option<i64>,
    pub(super) messages: Vec<DeepAgentsParsedMessage>,
    pub(super) value_type: Option<String>,
    pub(super) value: Vec<u8>,
    pub(super) occurred_at: Option<DateTime<Utc>>,
    pub(super) rejection: Option<String>,
    pub(super) message_rejection_count: u64,
    pub(super) message_rejections: Vec<DeepAgentsMessageRejection>,
    pub(super) next_phase: DeepAgentsCorePhase,
    pub(super) retained_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeepAgentsOutputFrontier {
    pub(super) version: u32,
    pub(super) after_rowid: Option<i64>,
    pub(super) active_rowid: Option<i64>,
    pub(super) next_message_offset: u32,
    pub(super) terminal: bool,
}

impl DeepAgentsOutputFrontier {
    pub(super) fn initial() -> Self {
        Self {
            version: DEEPAGENTS_OUTPUT_FRONTIER_VERSION,
            after_rowid: None,
            active_rowid: None,
            next_message_offset: 0,
            terminal: false,
        }
    }
}
