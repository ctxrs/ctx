use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KiroRejection {
    pub(super) line: u64,
    pub(super) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KiroFrontier {
    pub(super) version: u32,
    pub(super) phase: KiroPhase,
    pub(super) after_rowid: Option<i64>,
    pub(super) active_rowid: Option<i64>,
    pub(super) next_history_index: u64,
    pub(super) next_event_ordinal: u32,
    pub(super) next_row_ordinal: u64,
    pub(super) prefix_sha256: [u8; 32],
}

impl KiroFrontier {
    pub(super) fn initial(tables: KiroTables) -> Self {
        Self {
            version: KIRO_NATIVE_CURSOR_VERSION,
            phase: tables.initial_phase(),
            after_rowid: None,
            active_rowid: None,
            next_history_index: 0,
            next_event_ordinal: 0,
            next_row_ordinal: 0,
            prefix_sha256: Sha256::digest(KIRO_PREFIX_DOMAIN).into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KiroStoreCursor {
    pub(super) version: u32,
    pub(super) provider: String,
    pub(super) locator_identity: String,
    pub(super) canonical_source_identity: String,
    pub(super) source_revision: String,
    pub(super) frontier: KiroFrontier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) retirement: Option<KiroRetirementRequest>,
    pub(super) terminal: bool,
    pub(super) generation: u64,
    pub(super) rejected_records: u64,
    #[serde(default)]
    pub(super) accepted_content_records: u64,
    #[serde(default)]
    pub(super) rejections: Vec<KiroRejection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KiroRetirementRequest {
    pub(super) after: Option<KiroRetirementFrontier>,
    pub(super) committed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KiroRetirementFrontier {
    pub(super) kind: String,
    pub(super) id: Uuid,
}

impl KiroRetirementFrontier {
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
                    "Kiro retirement frontier has an unsupported entity kind".to_owned(),
                ));
            }
        };
        Ok(NativePathSourceEntityFrontier { kind, id: self.id })
    }
}

impl KiroStoreCursor {
    pub(super) fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }

    pub(super) fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded)?;
        if cursor.version != KIRO_NATIVE_CURSOR_VERSION
            || cursor.provider != CaptureProvider::KiroCli.as_str()
            || cursor.frontier.version != KIRO_NATIVE_CURSOR_VERSION
            || cursor.locator_identity.is_empty()
            || cursor.canonical_source_identity.is_empty()
            || (cursor.terminal && cursor.retirement.is_some())
            || cursor.rejections.len() > KIRO_MAX_REJECTION_DETAILS
            || u64::try_from(cursor.rejections.len()).unwrap_or(u64::MAX) > cursor.rejected_records
        {
            return Err(CaptureError::InvalidPayload(
                "Kiro NativePath cursor has an unsupported version or provider".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

pub(super) struct KiroCoreStart {
    pub(super) frontier: KiroFrontier,
    pub(super) retirement: Option<KiroRetirementRequest>,
    pub(super) already_terminal: bool,
    pub(super) rejected_records: u64,
    pub(super) accepted_content_records: u64,
    pub(super) rejections: Vec<KiroRejection>,
}

impl KiroCoreStart {
    pub(super) fn summary(&self) -> ProviderImportSummary {
        let mut summary = ProviderImportSummary {
            failed: usize::try_from(self.rejected_records).unwrap_or(usize::MAX),
            accepted_content_records: usize::try_from(self.accepted_content_records)
                .unwrap_or(usize::MAX),
            failures: self
                .rejections
                .iter()
                .map(|rejection| ProviderImportFailure {
                    line: usize::try_from(rejection.line)
                        .unwrap_or(usize::MAX)
                        .saturating_add(1),
                    error: rejection.reason.clone(),
                })
                .collect(),
            ..ProviderImportSummary::default()
        };
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        summary
    }
}

pub(super) fn core_start(
    stored: Option<&SyncCursor>,
    source: &KiroSource,
) -> Result<KiroCoreStart> {
    let initial = KiroFrontier::initial(source.tables);
    let Some(stored) = stored else {
        return Ok(KiroCoreStart {
            frontier: initial,
            retirement: None,
            already_terminal: false,
            rejected_records: 0,
            accepted_content_records: 0,
            rejections: Vec::new(),
        });
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let cursor = KiroStoreCursor::decode(committed.provider_cursor())?;
        if cursor.locator_identity != source.locator_identity {
            return Err(CaptureError::InvalidPayload(
                "Kiro NativePath cursor belongs to another physical source".to_owned(),
            ));
        }
        if cursor.source_revision == source.source_revision {
            return Ok(KiroCoreStart {
                frontier: cursor.frontier,
                retirement: cursor.retirement,
                already_terminal: cursor.terminal,
                rejected_records: cursor.rejected_records,
                accepted_content_records: cursor.accepted_content_records,
                rejections: cursor.rejections,
            });
        }
        return Ok(KiroCoreStart {
            frontier: initial,
            retirement: None,
            already_terminal: false,
            rejected_records: 0,
            accepted_content_records: 0,
            rejections: Vec::new(),
        });
    }
    decode_released_kiro_cursor(&stored.cursor)?;
    Ok(KiroCoreStart {
        frontier: initial,
        retirement: None,
        already_terminal: false,
        rejected_records: 0,
        accepted_content_records: 0,
        rejections: Vec::new(),
    })
}

/// Migration-only decoder for cursors emitted by the released pre-NativePath
/// Kiro importer. The decoded position is validated but never resumed or
/// emitted; the first NativePath group atomically replaces the exact old
/// cursor after rescanning from provider-owned source authority.
pub(super) fn decode_released_kiro_cursor(encoded: &str) -> Result<()> {
    let Some(cursor) = CertifiedProviderCursor::decode_if_certified(encoded)? else {
        return Ok(());
    };
    if cursor.parser_revision() != 2
        || cursor.policy_revision() != 4
        || cursor.native_position().kind() != KIRO_LEGACY_POSITION_KIND
    {
        return Err(CaptureError::InvalidPayload(
            "stored Kiro cursor is not a released Kiro ingestion cursor".to_owned(),
        ));
    }
    let bytes = cursor.native_position().value();
    if bytes != [0] && (bytes.len() != 17 || !matches!(bytes[0], 1 | 2)) {
        return Err(CaptureError::InvalidPayload(
            "released Kiro cursor has an invalid native position".to_owned(),
        ));
    }
    Ok(())
}
