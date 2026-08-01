//! Thin CodeBuddy adapter for the shared replacement-document lifecycle.

use ctx_history_core::{
    derive_event_id, derive_session_id, CoreRecord, EventIdentityInput, NativeItemKey,
    NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey,
    SourceObservation, TypedKey,
};

#[cfg(test)]
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Barrier, Mutex,
};

use crate::provider::source_backed::{
    document_leaf_execution_policy,
    family::document::{
        register_replacement_document_tree_route, ChangedDocumentSink, DocumentLeafExecutionPolicy,
        DocumentSourceTerminal, ReplacementDocumentTree,
    },
    SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
};

use super::*;

const IDENTITY_VERSION: u32 = 1;
const PARSER_REVISION: &str = "codebuddy-source-backed-v1";
const SOURCE_ANCHOR_NAMESPACE: &str = "codebuddy-native-source-v1";
const SESSION_KEY_NAMESPACE: &str = "codebuddy-native-session-v1";
const EVENT_KEY_NAMESPACE: &str = "codebuddy-native-event-v1";
const CODEBUDDY_CLI_SCHEMA_VARIANT: &str = "cli-jsonl-v1";
const CODEBUDDY_EXTENSION_SCHEMA_VARIANT: &str = "ide-structured-message-v1";
const CODEBUDDY_CLI_NATIVE_COORDINATE_TAG: &str = "codebuddy-jsonl-range-v1";
const CODEBUDDY_EXTENSION_NATIVE_COORDINATE_TAG: &str = "codebuddy-structured-message-v1";
const EXTENSION_CANONICAL_DOMAIN: &[u8] = b"ctx-codebuddy-structured-source-v1\0";

#[derive(Debug)]
struct CodeBuddyDocumentAdapter {
    root: PathBuf,
    context: ProviderAdapterContext,
    #[cfg(test)]
    parse_count: Option<Arc<AtomicUsize>>,
    #[cfg(test)]
    leaf_workers: Option<usize>,
    #[cfg(test)]
    scan_activity: Option<Arc<CodeBuddyScanActivity>>,
}

#[cfg(test)]
#[derive(Debug)]
struct CodeBuddyScanActivity {
    barrier: Mutex<Option<Arc<Barrier>>>,
    active: AtomicUsize,
    peak: AtomicUsize,
}

#[cfg(test)]
impl CodeBuddyScanActivity {
    fn new(participants: usize) -> Arc<Self> {
        Arc::new(Self {
            barrier: Mutex::new(Some(Arc::new(Barrier::new(participants)))),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }

    fn begin(self: &Arc<Self>) -> CodeBuddyScanActivityGuard {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let barrier = self
            .barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        CodeBuddyScanActivityGuard {
            activity: Arc::clone(self),
        }
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    fn disable_barrier(&self) {
        *self
            .barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
struct CodeBuddyScanActivityGuard {
    activity: Arc<CodeBuddyScanActivity>,
}

#[cfg(test)]
impl Drop for CodeBuddyScanActivityGuard {
    fn drop(&mut self) {
        let previous = self.activity.active.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(previous > 0);
    }
}

impl ReplacementDocumentTree for CodeBuddyDocumentAdapter {
    type Leaf = CodeBuddyDocumentLeaf;
    type TreeAuthority = CodeBuddyTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        owns_codebuddy_source(source)
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        #[cfg(test)]
        if let Some(leaf_workers) = self.leaf_workers {
            return DocumentLeafExecutionPolicy::IndependentCapped(leaf_workers);
        }
        document_leaf_execution_policy(CaptureProvider::CodeBuddy)
    }

    fn independent_leaf_source(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<SourceKey> {
        Ok(leaf.source.clone())
    }

    fn discover_complete(&self) -> SourceBackedRouteResult<CodeBuddyDocumentTree> {
        let inventory = discover_codebuddy_tree(&self.root).map_err(codebuddy_route_error)?;
        if inventory.status == CodeBuddyInventoryStatus::Unavailable {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                "CodeBuddy selected route is temporarily unavailable",
            ));
        }
        inventory.into_complete_tree().ok_or_else(|| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "complete CodeBuddy inventory lost its tree",
            )
        })
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        #[cfg(test)]
        let _scan_activity = self
            .scan_activity
            .as_ref()
            .map(CodeBuddyScanActivity::begin);
        #[cfg(test)]
        if let Some(parse_count) = self.parse_count.as_ref() {
            parse_count.fetch_add(1, Ordering::Relaxed);
        }
        scan_changed_codebuddy_source(authority, leaf, &self.context, sink)
            .map_err(codebuddy_route_error)
    }

    fn revalidate_complete(
        &self,
        tree: &CodeBuddyDocumentTree,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        revalidate_codebuddy_tree(tree).map_err(codebuddy_route_error)
    }
}

fn scan_changed_codebuddy_source(
    authority: &CodeBuddyTreeAuthority,
    leaf: &CodeBuddyDocumentLeaf,
    context: &ProviderAdapterContext,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> Result<DocumentSourceTerminal> {
    let source = open_codebuddy_source(authority, leaf)?;
    let mut state = initial_state(&source, context)?;
    let source_key = codebuddy_source_key(&source, &state.session)?;
    if !source_key.exact_descriptor_eq(&leaf.source) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let observation = source_observation(&source, source_key.clone())?;
    let mut counts = ScannedSourceCounts::default();
    let mut structured_digest = Sha256::new();
    let mut structured_bytes = 0_u64;
    if source.shape == CodeBuddySourceShape::Extension {
        structured_digest.update(EXTENSION_CANONICAL_DOMAIN);
        structured_digest.update(source.source_revision.as_bytes());
        structured_bytes = source.source_revision.len() as u64;
    } else {
        note_body_read();
    }

    sink.begin_source(source_key.clone())
        .map_err(codebuddy_capture_error)?;
    while let Some(page) = next_source_page(&source, &state, context)? {
        for record in &page.records {
            counts.complete_records = checked_add(counts.complete_records, 1, "complete records")?;
            match &record.classification {
                CodeBuddyRecordClassification::AcceptedMessage(core) => {
                    counts.retained_records =
                        checked_add(counts.retained_records, 1, "retained records")?;
                    sink.emit_core_record(codebuddy_core_record(
                        &source,
                        &source_key,
                        record,
                        core,
                    )?)
                    .map_err(codebuddy_capture_error)?;
                    counts.indexed_documents =
                        checked_add(counts.indexed_documents, 1, "indexed documents")?;
                }
                CodeBuddyRecordClassification::RejectedRecord => {
                    counts.rejected_records =
                        checked_add(counts.rejected_records, 1, "rejected records")?;
                }
                CodeBuddyRecordClassification::SkippedMetadata => {
                    counts.ignored_records =
                        checked_add(counts.ignored_records, 1, "ignored records")?;
                }
            }
            if source.shape == CodeBuddySourceShape::Extension {
                structured_digest.update(record.native_ordinal.to_be_bytes());
                structured_digest.update((record.native_bytes.len() as u64).to_be_bytes());
                structured_digest.update(&record.native_bytes);
                structured_bytes = structured_bytes
                    .checked_add(16)
                    .and_then(|value| value.checked_add(record.native_bytes.len() as u64))
                    .ok_or(CaptureError::SystemInvariant(
                        "CodeBuddy structured byte count overflowed",
                    ))?;
            }
        }
        state = page.next_state;
    }
    if !state.terminal {
        return Err(CaptureError::SystemInvariant(
            "CodeBuddy parser stopped before its terminal frontier",
        ));
    }
    if let Some(primary) = source
        .capability
        .as_ref()
        .and_then(|capability| capability.primary.as_ref())
    {
        primary.revalidate()?;
    }
    let (content_digest, certified_bytes) = match source.shape {
        CodeBuddySourceShape::Cli => (
            decode_sha256(&state.certified_prefix_sha256)?,
            state.next_native_offset,
        ),
        CodeBuddySourceShape::Extension => (structured_digest.finalize().into(), structured_bytes),
    };
    counts.certified_bytes = certified_bytes;
    Ok(DocumentSourceTerminal {
        source: source_key,
        opening: observation.clone(),
        closing: observation,
        parser_revision: PARSER_REVISION,
        content_digest,
        counts,
    })
}

pub(super) fn codebuddy_source_key_for_path(
    shape: CodeBuddySourceShape,
    path: &Path,
) -> Result<SourceKey> {
    let native_session_id = match shape {
        CodeBuddySourceShape::Cli => path.file_stem(),
        CodeBuddySourceShape::Extension => path.file_name(),
    }
    .and_then(|name| name.to_str())
    .filter(|name| !name.trim().is_empty())
    .unwrap_or("unknown-session");
    let project_hash = match shape {
        CodeBuddySourceShape::Cli => cli_project_hash(path),
        CodeBuddySourceShape::Extension => path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("unknown-project")
            .to_owned(),
    };
    codebuddy_source_key_for_identity(shape, &project_hash, native_session_id)
}

fn codebuddy_source_key(
    source: &CodeBuddySource,
    session: &CodeBuddySessionState,
) -> Result<SourceKey> {
    codebuddy_source_key_for_identity(
        source.shape,
        &session.project_hash,
        &session.native_session_id,
    )
}

fn codebuddy_source_key_for_identity(
    shape: CodeBuddySourceShape,
    project_hash: &str,
    native_session_id: &str,
) -> Result<SourceKey> {
    let (shape_key, schema_variant) = match shape {
        CodeBuddySourceShape::Cli => ("cli", CODEBUDDY_CLI_SCHEMA_VARIANT),
        CodeBuddySourceShape::Extension => ("ide", CODEBUDDY_EXTENSION_SCHEMA_VARIANT),
    };
    let anchor = contract(
        SourceAnchor::provider_native(
            SOURCE_ANCHOR_NAMESPACE,
            contract(
                TypedKey::composite(vec![
                    contract(TypedKey::utf8(shape_key), "source shape")?,
                    contract(TypedKey::utf8(project_hash), "project source key")?,
                    contract(TypedKey::utf8(native_session_id), "session source key")?,
                ]),
                "source anchor key",
            )?,
        ),
        "source anchor",
    )?;
    contract(
        SourceKey::derive(
            CaptureProvider::CodeBuddy.as_str(),
            CODEBUDDY_SOURCE_FORMAT,
            schema_variant,
            IDENTITY_VERSION,
            anchor,
        ),
        "source key",
    )
}

fn owns_codebuddy_source(source: &SourceKey) -> bool {
    source.provider() == CaptureProvider::CodeBuddy.as_str()
        && source.source_format() == CODEBUDDY_SOURCE_FORMAT
        && matches!(
            source.schema_variant(),
            CODEBUDDY_CLI_SCHEMA_VARIANT | CODEBUDDY_EXTENSION_SCHEMA_VARIANT
        )
        && source.provider_identity_version() == IDENTITY_VERSION
}

fn source_observation(source: &CodeBuddySource, key: SourceKey) -> Result<SourceObservation> {
    contract(
        SourceObservation::new(
            key,
            format!("codebuddy-{}-observation-v1", source.shape.shape_tag()),
            source.source_revision.as_bytes().to_vec(),
        ),
        "source observation",
    )
}

fn codebuddy_core_record(
    source: &CodeBuddySource,
    source_key: &SourceKey,
    record: &CodeBuddyRecord,
    core: &CodeBuddyCoreRow,
) -> Result<CoreRecord> {
    let provider_session_id = core.session.provider_session_id.clone();
    let session_key = contract(
        NativeSessionKey::native_id(
            SESSION_KEY_NAMESPACE,
            contract(TypedKey::utf8(&provider_session_id), "native session key")?,
        ),
        "native session key",
    )?;
    let session_id = contract(
        derive_session_id(SessionIdentityInput {
            source: source_key,
            logical_session_kind: "codebuddy-session",
            native_session_key: &session_key,
        }),
        "session identity",
    )?;
    let native_message_id = core.event.native_message_id.as_str();
    if native_message_id.is_empty() {
        return Err(CaptureError::SystemInvariant(
            "CodeBuddy normalized event lost its native identity",
        ));
    }
    let item_key = contract(
        NativeItemKey::native_id(
            EVENT_KEY_NAMESPACE,
            contract(
                TypedKey::composite(vec![
                    contract(TypedKey::utf8(source.shape.shape_tag()), "event shape")?,
                    contract(TypedKey::utf8(native_message_id), "native message key")?,
                ]),
                "native event key",
            )?,
        ),
        "native event key",
    )?;
    let event_id = contract(
        derive_event_id(EventIdentityInput {
            source: source_key,
            session_id,
            logical_item_kind: "codebuddy-event",
            native_item_key: &item_key,
            subrecord_selector: None,
        }),
        "event identity",
    )?;
    let native_event_id = match source.shape {
        CodeBuddySourceShape::Cli => contract(
            TypedKey::composite(vec![
                contract(
                    TypedKey::utf8(CODEBUDDY_CLI_NATIVE_COORDINATE_TAG),
                    "CLI coordinate tag",
                )?,
                contract(
                    TypedKey::utf8(&provider_session_id),
                    "CLI native session key",
                )?,
                contract(TypedKey::utf8(native_message_id), "CLI native key")?,
                TypedKey::U64(record.native_ordinal),
            ]),
            "CLI native event coordinate",
        )?,
        CodeBuddySourceShape::Extension => contract(
            TypedKey::composite(vec![
                contract(
                    TypedKey::utf8(CODEBUDDY_EXTENSION_NATIVE_COORDINATE_TAG),
                    "structured coordinate tag",
                )?,
                contract(
                    TypedKey::utf8(native_message_id),
                    "structured native file key",
                )?,
                TypedKey::U64(record.native_ordinal),
                contract(
                    TypedKey::utf8(&core.event.legacy_provider_event_hash),
                    "structured native record key",
                )?,
            ]),
            "structured native event coordinate",
        )?,
    };
    let body = if core.event.text.trim().is_empty() {
        core.event.event_type.as_str().to_owned()
    } else {
        core.event.text.clone()
    };
    let mut projected = contract(
        CoreRecord::new_selected(
            event_id,
            session_id,
            session_id,
            source_key.clone(),
            record.native_ordinal,
            core.event.event_type.as_str(),
            AgentType::Primary.as_str(),
            true,
            PARSER_REVISION,
            body,
        ),
        "Core record",
    )?;
    projected.provider_session_id = Some(provider_session_id);
    projected.native_event_id = Some(native_event_id);
    projected.occurred_at_unix_ms = Some(core.event.occurred_at.timestamp_millis());
    projected.role = Some(core.event.role.as_str().to_owned());
    projected.cwd = core.session.cwd.clone();
    contract(projected.validate_contract(), "completed Core record")?;
    Ok(projected)
}

fn decode_sha256(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64 {
        return Err(invalid_source_backed(
            "parser frontier has an invalid SHA-256 digest",
        ));
    }
    let mut digest = [0_u8; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        let start = index.saturating_mul(2);
        *slot = u8::from_str_radix(&value[start..start + 2], 16)
            .map_err(|_| invalid_source_backed("parser frontier digest is not hexadecimal"))?;
    }
    Ok(digest)
}

fn checked_add(value: u64, amount: u64, field: &'static str) -> Result<u64> {
    value
        .checked_add(amount)
        .ok_or(CaptureError::SystemInvariant(field))
}

fn contract<T, E: std::fmt::Display>(
    result: std::result::Result<T, E>,
    boundary: &'static str,
) -> Result<T> {
    result.map_err(|error| {
        invalid_source_backed(format!("{boundary} violates the shared contract: {error}"))
    })
}

fn invalid_source_backed(detail: impl Into<String>) -> CaptureError {
    CaptureError::InvalidPayload(format!(
        "CodeBuddy source-backed adapter: {}",
        detail.into()
    ))
}

fn codebuddy_route_error(error: CaptureError) -> SourceBackedRouteError {
    let kind = if matches!(error, CaptureError::SourceChangedDuringCapture) {
        SourceBackedRouteErrorKind::SourceChanged
    } else {
        SourceBackedRouteErrorKind::InvalidSource
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

fn codebuddy_capture_error(error: SourceBackedRouteError) -> CaptureError {
    invalid_source_backed(error.to_string())
}

#[cfg(test)]
mod tests;

pub(crate) mod registration {
    use chrono::{DateTime, Utc};

    use super::{register_replacement_document_tree_route, CodeBuddyDocumentAdapter};
    use crate::provider::source_backed::{
        SourceBackedCoordinatorResult, SourceBackedProviderRegistry, SourceBackedRouteSelection,
    };
    use crate::{ProviderAdapterContext, ProviderSource};

    pub(crate) fn register(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
    ) -> SourceBackedCoordinatorResult<()> {
        let context = ProviderAdapterContext {
            machine_id: "source-backed-codebuddy".to_owned(),
            source_path: Some(source.path.clone()),
            source_root: Some(source.path.clone()),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        };
        let adapter = CodeBuddyDocumentAdapter {
            root: source.path.clone(),
            context,
            #[cfg(test)]
            parse_count: None,
            #[cfg(test)]
            leaf_workers: None,
            #[cfg(test)]
            scan_activity: None,
        };
        register_replacement_document_tree_route(registry, source, selection, adapter)
    }
}
