use super::*;

pub(super) const MUX_CURSOR_VERSION: u32 = 1;
pub(super) const MUX_FRONTIER_VERSION: u32 = 1;
pub(super) const MUX_ROOT_MANIFEST_VERSION: u32 = 1;
pub(super) const MUX_PAGE_MAX_RECORDS: usize = 8;
pub(super) const MUX_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MUX_MAX_FILE_TOUCHES_PER_EVENT: usize = 448;
pub(super) const MUX_OUTPUT_PARSER_REVISION: &str = "mux-nativepath-output-v1";
pub(super) const MUX_PUBLICATION_PREFIX: &str = "mux-nativepath-v1:";
pub(super) const MUX_PARTIAL_NATIVE_ORDINAL: u64 = 1_u64 << 63;
pub(super) const MUX_GENERATION_BITS: u32 = 16;
pub(super) const MUX_ORDINAL_BITS: u32 = 47;
pub(super) const MUX_MAX_GENERATION: u64 = (1_u64 << MUX_GENERATION_BITS) - 1;
pub(super) const MUX_MAX_ORDINAL: u64 = (1_u64 << MUX_ORDINAL_BITS) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MuxStreamKind {
    Chat,
    Partial,
}

impl MuxStreamKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Chat => "chat-jsonl",
            Self::Partial => "partial-json",
        }
    }

    pub(super) fn is_partial(self) -> bool {
        self == Self::Partial
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MuxFrontier {
    pub(super) version: u32,
    pub(super) next_offset: u64,
    pub(super) next_ordinal: u64,
    pub(super) prefix_sha256: [u8; 32],
    pub(super) file_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) legacy_valid_rows: Option<u64>,
}

impl MuxFrontier {
    pub(super) fn initial() -> Self {
        Self {
            version: MUX_FRONTIER_VERSION,
            next_offset: 0,
            next_ordinal: 0,
            prefix_sha256: Sha256::digest([]).into(),
            file_identity: None,
            legacy_valid_rows: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MuxCursorWire {
    pub(super) version: u32,
    pub(super) capture_revision: u32,
    pub(super) policy_revision: u32,
    pub(super) kind: MuxStreamKind,
    pub(super) canonical_path: PathBuf,
    pub(super) source_revision: String,
    pub(super) metadata_revision: String,
    pub(super) generation: u64,
    pub(super) frontier: MuxFrontier,
    pub(super) terminal: bool,
    pub(super) retired: bool,
    pub(super) accepted_events: u64,
    pub(super) rejected_records: u64,
    pub(super) first_failure: Option<MuxFailureWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MuxFailureWire {
    pub(super) line: usize,
    pub(super) error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MuxRootManifest {
    pub(super) version: u32,
    pub(super) configured_root: PathBuf,
    pub(super) sources: Vec<MuxManifestSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MuxManifestSource {
    pub(super) path: PathBuf,
    pub(super) kind: MuxStreamKind,
    pub(super) cursor_stream: String,
    pub(super) locator_identity: String,
    pub(super) canonical_source_identity: String,
    pub(super) source_revision: String,
}

#[derive(Debug)]
pub(super) struct MuxPreparedRow {
    pub(super) line_number: usize,
    pub(super) native_ordinal: u64,
    pub(super) source_record_ordinal: u64,
    pub(super) source_locator: CompleteContentSourceLocator,
    pub(super) source_record_digest: CompleteContentBodyDigest,
    pub(super) native_record_id: String,
    pub(super) message_content_ref: Option<ContentRef>,
    pub(super) unaddressable_output: Option<MuxUnaddressableOutput>,
    pub(super) event: Option<MuxCoreEvent>,
    pub(super) event_hash: Option<String>,
    pub(super) file_touches: Vec<MuxFileTouch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MuxUnaddressableOutput {
    Redacted,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MuxLegacyPartialDisposition {
    None,
    Ignored,
    Replace { chat_rank: u64 },
    Insert { merged_index: u64 },
}

#[derive(Debug, Clone)]
pub(super) struct MuxLegacyBridge {
    pub(super) primary_path: PathBuf,
    pub(super) primary_source_id: Uuid,
    pub(super) primary_source_identity: String,
    pub(super) provider_session_id: String,
    pub(super) partial_disposition: MuxLegacyPartialDisposition,
}

#[derive(Debug)]
pub(super) struct MuxFileTouch {
    pub(super) provider_touch_index: u64,
    pub(super) provider_event_index: Option<u64>,
    pub(super) raw_source_path: Option<String>,
    pub(super) source_root: Option<String>,
    pub(super) path: String,
    pub(super) change_kind: Option<FileChangeKind>,
    pub(super) old_path: Option<String>,
    pub(super) line_count_delta: Option<i64>,
    pub(super) confidence: Confidence,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) metadata: Value,
}

#[derive(Debug)]
pub(super) struct MuxPreparedPage {
    pub(super) rows: Vec<MuxPreparedRow>,
    pub(super) expected: MuxFrontier,
    pub(super) next: MuxFrontier,
    pub(super) terminal: bool,
    pub(super) deferred_incomplete: bool,
    pub(super) previous_rejected_records: u64,
    pub(super) rejected_records: u64,
    pub(super) first_failure: Option<MuxFailureWire>,
}

#[derive(Debug)]
pub(super) struct MuxLoadedCursor {
    pub(super) stored: SyncCursor,
    pub(super) wire: Option<MuxCursorWire>,
}

#[derive(Debug)]
pub(super) struct MuxSourcePlan {
    pub(super) source: MuxSessionSource,
    pub(super) path: PathBuf,
    pub(super) kind: MuxStreamKind,
    pub(super) observation: MuxFileObservation,
    pub(super) path_identity: String,
    pub(super) cursor_stream: String,
    pub(super) canonical_source_identity: String,
    pub(super) source_revision: String,
    pub(super) metadata_revision: String,
    pub(super) prior: Option<MuxLoadedCursor>,
    pub(super) generation: u64,
    pub(super) initial_frontier: MuxFrontier,
    pub(super) accepted_events: u64,
    pub(super) rejected_records: u64,
    pub(super) first_failure: Option<MuxFailureWire>,
    pub(super) legacy_bridge: Option<MuxLegacyBridge>,
}

impl MuxSourcePlan {
    pub(super) fn manifest_source(&self) -> MuxManifestSource {
        MuxManifestSource {
            path: self.observation.canonical_path.clone(),
            kind: self.kind,
            cursor_stream: self.cursor_stream.clone(),
            locator_identity: self.path_identity.clone(),
            canonical_source_identity: self.canonical_source_identity.clone(),
            source_revision: self.source_revision.clone(),
        }
    }

    pub(super) fn is_primary_source(&self) -> bool {
        self.source
            .chat_path
            .as_deref()
            .or(self.source.partial_path.as_deref())
            == Some(self.path.as_path())
    }

    pub(super) fn is_legacy_primary_source(&self) -> bool {
        self.legacy_bridge
            .as_ref()
            .is_some_and(|bridge| bridge.primary_path == self.path)
    }

    pub(super) fn event_identity_source_id(&self, capture_source_id: Uuid) -> Uuid {
        self.legacy_bridge
            .as_ref()
            .map_or(capture_source_id, |bridge| bridge.primary_source_id)
    }

    pub(super) fn session_source_id(&self, capture_source_id: Uuid) -> Uuid {
        self.event_identity_source_id(capture_source_id)
    }

    pub(super) fn counts_session_projection(&self) -> bool {
        self.legacy_bridge.is_none() || self.is_primary_source()
    }

    pub(super) fn legacy_event_index(&self, chat_rank: Option<u64>) -> Result<Option<u64>> {
        let Some(bridge) = self.legacy_bridge.as_ref() else {
            return Ok(None);
        };
        match self.kind {
            MuxStreamKind::Chat => {
                let rank = chat_rank.ok_or(CaptureError::SystemInvariant(
                    "Mux legacy chat row lost its merged rank",
                ))?;
                match bridge.partial_disposition {
                    MuxLegacyPartialDisposition::Replace { chat_rank } if chat_rank == rank => {
                        Ok(None)
                    }
                    MuxLegacyPartialDisposition::Insert { merged_index }
                        if rank >= merged_index =>
                    {
                        rank.checked_add(1)
                            .map(Some)
                            .ok_or(CaptureError::SystemInvariant(
                                "Mux legacy merged event index overflowed",
                            ))
                    }
                    _ => Ok(Some(rank)),
                }
            }
            MuxStreamKind::Partial => match bridge.partial_disposition {
                MuxLegacyPartialDisposition::Replace { chat_rank } => Ok(Some(chat_rank)),
                MuxLegacyPartialDisposition::Insert { merged_index } => Ok(Some(merged_index)),
                MuxLegacyPartialDisposition::None | MuxLegacyPartialDisposition::Ignored => {
                    Ok(None)
                }
            },
        }
    }
}

pub(super) fn stream_kind_rank(kind: MuxStreamKind) -> u8 {
    match kind {
        MuxStreamKind::Chat => 0,
        MuxStreamKind::Partial => 1,
    }
}
