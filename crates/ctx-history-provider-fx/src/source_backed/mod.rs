use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, SourceAnchorScope, SourceKey, TypedKey};
use ctx_history_jsonl::JsonlFamilyError;
use ctx_history_provider_runtime::source_io::{
    ProviderSourceRoot, PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
};
use ctx_history_provider_runtime::{
    source_io::OpenedProviderSourceFile, CaptureError, JsonlFamilyProjector,
    ProviderBaseEventLookup, ProviderJsonlAdapter, ProviderJsonlExecutionIo,
    ProviderJsonlInventory, ProviderJsonlLeaf, ProviderJsonlMembershipObservation,
    ProviderJsonlRuntime, ProviderJsonlWorkerContext, ProviderRuntimeBinding,
};

mod discovery;
mod legacy_projector;
mod reducer;
use discovery::{
    has_markerless_v3_evidence, pending_exists, read_limited, reject_path, safe_directory_name,
};
use legacy_projector::FxLegacyProjector;

use crate::{
    decode_authority, decode_first_event_binding, decode_replay_checkpoint, decode_watermark,
    encode_replay_checkpoint, project_canonical_state, project_logical_turns,
    replay_legacy_snapshot, FxAuthority, FxProviderError, FxWatermark, LegacyDefaults,
    ProjectionBinding, ReplayLimits, SuffixDisposition, TempFileScratch,
};
use ctx_history_provider_runtime::{
    JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventoryMode, JsonlFamilyPendingLeaf,
    JsonlFamilyProjectionMode, JsonlFamilyRejectedLeaf, JsonlFamilyRootMissingMode,
    JsonlFamilySemanticExecutor, JsonlFamilySemanticPage, JsonlFamilySemanticPreflight,
    JsonlFamilySemanticSummary,
};
use reducer::{CommittedReplayReducer, SuffixReplayReducer};

pub const FX_SESSIONS_TREE_SOURCE_FORMAT: &str = "fx_sessions_tree";
pub const FX_SESSIONS_TREE_SCHEMA_VARIANT: &str = "fx-native-sessions-v1";

const SOURCE_ANCHOR_NAMESPACE: &str = "fx.session";
const DEFAULT_CATALOG_LINEAGE: [u8; 32] = [
    0x45, 0x82, 0x08, 0x67, 0x5d, 0x34, 0xd3, 0x7a, 0x6f, 0x9a, 0x94, 0x2f, 0xa7, 0x9d, 0x35, 0x12,
    0x65, 0x92, 0x5d, 0x8c, 0x3a, 0x4d, 0x61, 0xa1, 0x44, 0x0a, 0x83, 0x77, 0x6e, 0xe9, 0x71, 0x0c,
];
const SIDECAR_MAX_BYTES: usize = 16 * 1024;
const LEGACY_MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct FxSessionsTreeAdapter<B> {
    source_root_lineage: Option<[u8; 32]>,
    plans: Arc<Mutex<HashMap<SourceKey, SessionPlan>>>,
    _binding: std::marker::PhantomData<fn() -> B>,
}

pub fn fx_sessions_tree_adapter<B: ProviderRuntimeBinding>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<ProviderJsonlAdapter<B>> {
    Arc::new(FxSessionsTreeAdapter::new(source_root_lineage))
}

#[cfg(test)]
pub(crate) fn test_inventory(root: &Path) -> Result<ProviderJsonlInventory, CaptureError> {
    FxSessionsTreeAdapter::<()>::new(Some([0x5a; 32])).discover_inventory(root)
}

impl<B> FxSessionsTreeAdapter<B> {
    fn new(source_root_lineage: Option<[u8; 32]>) -> Self {
        Self {
            source_root_lineage,
            plans: Arc::new(Mutex::new(HashMap::new())),
            _binding: std::marker::PhantomData,
        }
    }
}

#[derive(Clone)]
enum SessionPlan {
    V3 {
        source_path: PathBuf,
        observation: ctx_history_provider_runtime::JsonlFileObservation,
        logical_eof: Option<u64>,
        authority: FxAuthority,
        watermark: FxWatermark,
    },
    Legacy {
        source_path: PathBuf,
        observation: ctx_history_provider_runtime::JsonlFileObservation,
        logical_eof: Option<u64>,
        defaults: LegacyDefaults,
    },
}

struct InventoryBuild {
    leaves: Vec<ProviderJsonlLeaf>,
    rejected: Vec<JsonlFamilyRejectedLeaf>,
    pending: Vec<JsonlFamilyPendingLeaf>,
    plans: HashMap<SourceKey, SessionPlan>,
    metadata_entries_remaining: usize,
}

impl<B> FxSessionsTreeAdapter<B> {
    fn source_key(&self, native_session_id: &str) -> Result<SourceKey, CaptureError> {
        let lineage = self.source_root_lineage.unwrap_or(DEFAULT_CATALOG_LINEAGE);
        SourceKey::derive_provider_native_scoped(
            CaptureProvider::Fx.as_str(),
            FX_SESSIONS_TREE_SOURCE_FORMAT,
            FX_SESSIONS_TREE_SCHEMA_VARIANT,
            1,
            SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(native_session_id).map_err(contract)?,
            SourceAnchorScope::Lineage(lineage),
        )
        .map_err(contract)
    }

    fn install_plans(&self, plans: HashMap<SourceKey, SessionPlan>) -> Result<(), CaptureError> {
        let mut stored = self.plans.lock().map_err(|_| state_error())?;
        *stored = plans;
        Ok(())
    }

    fn session_plan(&self, leaf: &ProviderJsonlLeaf) -> Result<SessionPlan, CaptureError> {
        let plan = self
            .plans
            .lock()
            .map_err(|_| state_error())?
            .get(leaf.source())
            .cloned()
            .ok_or_else(|| {
                CaptureError::InvalidPayload("fx JSONL leaf has no session plan".to_owned())
            })?;
        let (source_path, observation, logical_eof) = match &plan {
            SessionPlan::V3 {
                source_path,
                observation,
                logical_eof,
                ..
            }
            | SessionPlan::Legacy {
                source_path,
                observation,
                logical_eof,
                ..
            } => (source_path, observation, logical_eof),
        };
        if source_path != leaf.source_path()
            || observation != leaf.observation()
            || *logical_eof != leaf.logical_eof()
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(plan)
    }

    fn discover_session(
        &self,
        route_root: &Path,
        session_relative: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        inventory: &mut InventoryBuild,
    ) -> Result<(), CaptureError> {
        let InventoryBuild {
            leaves,
            rejected,
            pending,
            plans,
            metadata_entries_remaining,
        } = inventory;
        let authority_marker = session_relative.join("authority.json");
        let legacy_snapshot = session_relative.join("session.json");
        let events = session_relative.join("events.jsonl");
        let authority_pending = session_relative.join("authority.pending.json");
        let commit_pending = session_relative.join("commit.pending.json");
        let provisional = safe_directory_name(&session_relative)
            .map(|name| self.source_key(name))
            .transpose()?;
        // Pending sidecars are authoritative transition evidence. Admit one before
        // touching any stable sidecar: a writer may have removed/replaced the stable
        // triplet already, and that state must retain a prior durable source rather than
        // look like a deletion or a malformed ordinary session.
        let authority_pending_exists = match pending_exists(&authority, &authority_pending) {
            Ok(value) => value,
            Err(error) => {
                return reject_path(
                    &authority,
                    &authority_pending,
                    rejected,
                    provisional,
                    error.to_string(),
                )
            }
        };
        let commit_pending_exists = match pending_exists(&authority, &commit_pending) {
            Ok(value) => value,
            Err(error) => {
                return reject_path(
                    &authority,
                    &commit_pending,
                    rejected,
                    provisional,
                    error.to_string(),
                )
            }
        };
        let pending_path = if authority_pending_exists {
            Some(authority_pending.clone())
        } else if commit_pending_exists {
            Some(commit_pending.clone())
        } else {
            None
        };
        if let Some(pending_path) = pending_path {
            let opened = match authority.open_file(&pending_path) {
                Ok(opened) => opened,
                Err(error) => {
                    return reject_path(
                        &authority,
                        &pending_path,
                        rejected,
                        provisional,
                        error.to_string(),
                    )
                }
            };
            let source_path = authority.named_path().join(&pending_path);
            pending.push(JsonlFamilyPendingLeaf::bind_observed(
                source_path.clone(),
                pending_path,
                ctx_history_provider_runtime::observe_opened_file(&source_path, &opened)?,
                TypedKey::utf8("fx session authority transition is pending").map_err(contract)?,
                provisional,
            ));
            return Ok(());
        }
        let marker = match read_limited(&authority, &authority_marker, SIDECAR_MAX_BYTES) {
            Ok(value) => Some(value),
            Err(error) if error.is_not_found() => None,
            Err(error) => {
                return reject_path(
                    &authority,
                    &authority_marker,
                    rejected,
                    provisional,
                    error.to_string(),
                )
            }
        };
        if let Some((opened_authority, authority_bytes)) = marker {
            let parsed_authority = match decode_authority(&authority_bytes, ReplayLimits::default())
            {
                Ok(value) => value,
                Err(error) => {
                    return self.reject_events(&authority, &events, rejected, provisional, error);
                }
            };
            let native_session_id = parsed_authority.session_id.clone();
            let source = self.source_key(&native_session_id)?;
            if safe_directory_name(&session_relative) != Some(native_session_id.as_str()) {
                return reject_path(
                    &authority,
                    &events,
                    rejected,
                    provisional.or(Some(source)),
                    "fx authority session ID does not match its directory name".to_owned(),
                );
            }
            let events_opened = match authority.open_file(&events) {
                Ok(value) => value,
                Err(error) => {
                    return reject_path(
                        &authority,
                        &events,
                        rejected,
                        Some(source),
                        error.to_string(),
                    )
                }
            };
            let first_read = match events_opened.read_exact_range(
                0,
                usize::try_from(
                    events_opened
                        .len()
                        .min(crate::EVENT_FRAME_MAX_BYTES as u64 + 1),
                )
                .map_err(|_| {
                    CaptureError::SystemInvariant("fx first frame length exceeds usize")
                })?,
                crate::EVENT_FRAME_MAX_BYTES + 1,
            ) {
                Ok(value) => value,
                Err(error) => {
                    return reject_path(
                        &authority,
                        &events,
                        rejected,
                        Some(source),
                        error.to_string(),
                    )
                }
            };
            let Some(newline) = first_read.iter().position(|byte| *byte == b'\n') else {
                return reject_path(
                    &authority,
                    &events,
                    rejected,
                    Some(source),
                    "fx event log has no bounded complete first frame".to_owned(),
                );
            };
            let first =
                match decode_first_event_binding(&first_read[..newline], ReplayLimits::default()) {
                    Ok(value) => value,
                    Err(error) => {
                        return self.reject_events(
                            &authority,
                            &events,
                            rejected,
                            Some(source),
                            error,
                        )
                    }
                };
            if first.native_session_id != native_session_id {
                return self.reject_events(
                    &authority,
                    &events,
                    rejected,
                    Some(source),
                    FxProviderError::InvalidAuthority(
                        "first event session does not match authority",
                    ),
                );
            }
            let watermark_path =
                session_relative.join(format!("commit.{}.json", first.log_generation));
            let (opened_watermark, watermark_bytes) =
                match read_limited(&authority, &watermark_path, SIDECAR_MAX_BYTES) {
                    Ok(value) => value,
                    Err(error) => {
                        return reject_path(
                            &authority,
                            &events,
                            rejected,
                            Some(source),
                            format!("fx commit sidecar is unavailable: {error}"),
                        )
                    }
                };
            let watermark = match decode_watermark(&watermark_bytes, ReplayLimits::default()) {
                Ok(value) => value,
                Err(error) => {
                    return self.reject_events(&authority, &events, rejected, Some(source), error);
                }
            };
            if watermark.session_id != native_session_id
                || watermark.log_generation != first.log_generation
                || watermark.through_seq == 0
                || watermark.through_event_log_bytes > events_opened.len()
            {
                return self.reject_events(
                    &authority,
                    &events,
                    rejected,
                    Some(source),
                    FxProviderError::InvalidAuthority("commit session does not match authority"),
                );
            }
            let events_path = authority.named_path().join(&events);
            let observation =
                ctx_history_provider_runtime::observe_opened_file(&events_path, &events_opened)?;
            let leaf = ProviderJsonlLeaf::bind_observed(
                source.clone(),
                events_path,
                Arc::clone(&authority),
                events,
                TypedKey::utf8(&native_session_id).map_err(contract)?,
                observation,
            )
            .with_logical_eof(watermark.through_event_log_bytes)?
            .with_exact_present_dependency(authority_marker, &opened_authority)?
            .with_exact_present_dependency(watermark_path, &opened_watermark)?
            .with_exact_absent_dependency(authority_pending)?
            .with_exact_absent_dependency(commit_pending)?;
            plans.insert(
                source,
                SessionPlan::V3 {
                    source_path: leaf.source_path().to_path_buf(),
                    observation: leaf.observation().clone(),
                    logical_eof: leaf.logical_eof(),
                    authority: parsed_authority,
                    watermark,
                },
            );
            leaves.push(leaf);
            return Ok(());
        }

        let markerless_v3 = match has_markerless_v3_evidence(
            &authority,
            &session_relative,
            metadata_entries_remaining,
        ) {
            Ok(value) => value,
            Err(error) => {
                return reject_path(
                    &authority,
                    &events,
                    rejected,
                    provisional,
                    error.to_string(),
                )
            }
        };
        if markerless_v3 {
            return reject_path(
                &authority,
                &events,
                rejected,
                provisional,
                "fx markerless v3 evidence has no authority marker".to_owned(),
            );
        }
        let (_opened_snapshot, snapshot) =
            match read_limited(&authority, &legacy_snapshot, LEGACY_MAX_BYTES) {
                Ok(value) => value,
                Err(error) if error.is_not_found() => return Ok(()),
                Err(error) => {
                    return reject_path(
                        &authority,
                        &legacy_snapshot,
                        rejected,
                        provisional,
                        error.to_string(),
                    )
                }
            };
        let defaults = LegacyDefaults {
            source_root: route_root.display().to_string(),
            preferences: crate::SessionPreferences {
                provider: crate::ProviderId::Gateway,
                model: "fx-legacy".to_owned(),
                effort: "auto".to_owned(),
                fast_mode: false,
            },
        };
        let legacy = match replay_legacy_snapshot(&snapshot, &defaults, ReplayLimits::default()) {
            Ok(value) => value,
            Err(error) => {
                return self.reject_snapshot(
                    &authority,
                    &legacy_snapshot,
                    rejected,
                    provisional,
                    error,
                );
            }
        };
        let source = self.source_key(&legacy.state.id)?;
        if safe_directory_name(&session_relative) != Some(legacy.state.id.as_str()) {
            return self.reject_snapshot(
                &authority,
                &legacy_snapshot,
                rejected,
                provisional.or(Some(source)),
                FxProviderError::InvalidLegacy(
                    "legacy session ID does not match its directory name",
                ),
            );
        }
        let snapshot_path = authority.named_path().join(&legacy_snapshot);
        let leaf = ProviderJsonlLeaf::observe_whole_record(
            source.clone(),
            snapshot_path,
            authority,
            legacy_snapshot,
            TypedKey::utf8(&legacy.state.id).map_err(contract)?,
        )?
        .with_exact_absent_dependency(authority_marker)?
        .with_exact_absent_dependency(authority_pending)?
        .with_exact_absent_dependency(commit_pending)?;
        plans.insert(
            source,
            SessionPlan::Legacy {
                source_path: leaf.source_path().to_path_buf(),
                observation: leaf.observation().clone(),
                logical_eof: leaf.logical_eof(),
                defaults,
            },
        );
        leaves.push(leaf);
        Ok(())
    }

    fn reject_events(
        &self,
        authority: &Arc<ProviderSourceRoot>,
        events: &Path,
        rejected: &mut Vec<JsonlFamilyRejectedLeaf>,
        source: Option<SourceKey>,
        error: FxProviderError,
    ) -> Result<(), CaptureError> {
        reject_path(authority, events, rejected, source, error.to_string())
    }

    fn reject_snapshot(
        &self,
        authority: &Arc<ProviderSourceRoot>,
        snapshot: &Path,
        rejected: &mut Vec<JsonlFamilyRejectedLeaf>,
        source: Option<SourceKey>,
        error: FxProviderError,
    ) -> Result<(), CaptureError> {
        reject_path(authority, snapshot, rejected, source, error.to_string())
    }

    fn discover_inventory(&self, root: &Path) -> Result<ProviderJsonlInventory, CaptureError> {
        let root_authority = match ProviderSourceRoot::open(root) {
            Ok(root) => Arc::new(root),
            Err(error) if error.is_not_found() => {
                self.install_plans(HashMap::new())?;
                return ProviderJsonlInventory::missing(CaptureProvider::Fx, root);
            }
            Err(error) => return Err(error),
        };
        let names = root_authority
            .directory()?
            .entries(PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES)?;
        let metadata_entries_remaining = PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES
            .checked_sub(names.len())
            .ok_or(CaptureError::SystemInvariant(
                "fx root metadata budget accounting underflowed",
            ))?;
        let mut names = names;
        names.sort();
        let authorities = vec![Arc::clone(&root_authority)];
        let mut inventory = InventoryBuild {
            leaves: Vec::new(),
            rejected: Vec::new(),
            pending: Vec::new(),
            plans: HashMap::new(),
            metadata_entries_remaining,
        };
        for name in names {
            let relative = PathBuf::from(&name);
            match root_authority.open_directory(&relative) {
                Ok(_) => {}
                Err(error) if error.is_ignorable_membership_entry() || error.is_not_found() => {
                    continue;
                }
                Err(error) => return Err(error),
            }
            self.discover_session(root, relative, Arc::clone(&root_authority), &mut inventory)?;
        }
        let InventoryBuild {
            leaves,
            rejected,
            pending,
            plans,
            ..
        } = inventory;
        self.install_plans(plans)?;
        ProviderJsonlInventory::present_multi_with_dispositions(
            CaptureProvider::Fx,
            root,
            authorities,
            leaves,
            rejected,
            pending,
        )
    }
}

impl<B: ProviderRuntimeBinding> JsonlFamilyAdapter for FxSessionsTreeAdapter<B> {
    type Runtime = ctx_history_provider_runtime::ProviderJsonlRuntime<B>;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Fx
    }
    fn source_format(&self) -> &'static str {
        FX_SESSIONS_TREE_SOURCE_FORMAT
    }
    fn schema_variant(&self) -> &'static str {
        FX_SESSIONS_TREE_SCHEMA_VARIANT
    }
    fn parser_revision(&self) -> &'static str {
        crate::FX_PARSER_REVISION
    }
    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }
    fn root_missing_mode(&self) -> JsonlFamilyRootMissingMode {
        JsonlFamilyRootMissingMode::Unavailable
    }
    fn inventory_mode(&self) -> JsonlFamilyInventoryMode {
        JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions
    }

    fn discover(&self, root: &Path) -> Result<ProviderJsonlInventory, CaptureError> {
        self.discover_inventory(root)
    }

    fn partial_member_roots(&self, _root: &Path) -> Option<Vec<PathBuf>> {
        None
    }

    fn observe_terminal_membership(
        &self,
        root: &Path,
        opening: &ProviderJsonlInventory,
    ) -> Result<ProviderJsonlMembershipObservation, CaptureError> {
        ctx_history_provider_runtime::JsonlFamilyMembershipObservation::observe(root, opening)
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &ProviderJsonlLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<ProviderBaseEventLookup<B>>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = ProviderJsonlRuntime<B>>>, CaptureError>
    {
        if checkpoint.is_some() || mode == JsonlFamilyProjectionMode::CertifiedAppend {
            return Err(CaptureError::InvalidPayload(
                "fx legacy snapshots do not accept append checkpoints".to_owned(),
            ));
        }
        match self.session_plan(leaf)? {
            SessionPlan::Legacy { defaults, .. } => Ok(Box::new(FxLegacyProjector::<B>::new(
                leaf.source().clone(),
                defaults,
            ))),
            SessionPlan::V3 { .. } => Err(CaptureError::SystemInvariant(
                "fx v3 event log did not select its semantic executor",
            )),
        }
    }

    fn semantic_executor(
        &self,
        leaf: &ProviderJsonlLeaf,
        checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<ProviderBaseEventLookup<B>>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Option<Box<dyn JsonlFamilySemanticExecutor<Runtime = Self::Runtime>>>, CaptureError>
    {
        let plan = self.session_plan(leaf)?;
        match plan {
            SessionPlan::V3 { .. } => Ok(Some(Box::new(FxSemanticExecutor::new(
                leaf.source().clone(),
                plan,
                checkpoint,
                mode,
            )?))),
            SessionPlan::Legacy { .. } => Ok(None),
        }
    }
}

struct FxSemanticExecutor<B: ProviderRuntimeBinding> {
    source: SourceKey,
    plan: SessionPlan,
    mode: JsonlFamilyProjectionMode,
    checkpoint: Option<TypedKey>,
    reducer: Option<ReplayReducer>,
    pages: VecDeque<JsonlFamilySemanticPage>,
    represented: u64,
    done: bool,
    _binding: std::marker::PhantomData<fn() -> B>,
}

enum ReplayReducer {
    Cold(Box<CommittedReplayReducer>),
    Suffix(Box<SuffixReplayReducer>),
}

impl<B: ProviderRuntimeBinding> FxSemanticExecutor<B> {
    fn new(
        source: SourceKey,
        plan: SessionPlan,
        checkpoint: Option<&TypedKey>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Self, CaptureError> {
        Ok(Self {
            source,
            plan,
            mode,
            checkpoint: checkpoint.cloned(),
            reducer: None,
            pages: VecDeque::new(),
            represented: 0,
            done: false,
            _binding: std::marker::PhantomData,
        })
    }

    fn initialize(&mut self) -> Result<JsonlFamilySemanticPreflight, CaptureError> {
        let reducer = match (&self.plan, self.mode, self.checkpoint.as_ref()) {
            (
                SessionPlan::V3 {
                    authority,
                    watermark,
                    ..
                },
                JsonlFamilyProjectionMode::CertifiedAppend,
                Some(TypedKey::Bytes(bytes)),
            ) => {
                match decode_replay_checkpoint(bytes, ReplayLimits::default()).and_then(
                    |checkpoint| {
                        SuffixReplayReducer::new(
                            authority,
                            checkpoint,
                            watermark.clone(),
                            ReplayLimits::default(),
                        )
                    },
                ) {
                    Ok(reducer) => ReplayReducer::Suffix(Box::new(reducer)),
                    Err(_) => return Ok(JsonlFamilySemanticPreflight::RetryReplacement),
                }
            }
            (SessionPlan::V3 { .. }, JsonlFamilyProjectionMode::CertifiedAppend, _) => {
                return Ok(JsonlFamilySemanticPreflight::RetryReplacement)
            }
            (
                SessionPlan::V3 {
                    authority,
                    watermark,
                    ..
                },
                _,
                _,
            ) => ReplayReducer::Cold(Box::new(
                CommittedReplayReducer::new(
                    authority.clone(),
                    watermark.clone(),
                    ReplayLimits::default(),
                )
                .map_err(fx_error)?,
            )),
            (SessionPlan::Legacy { .. }, _, _) => {
                return Err(CaptureError::SystemInvariant(
                    "fx legacy snapshot selected its semantic executor",
                ));
            }
        };
        self.reducer = Some(reducer);
        Ok(JsonlFamilySemanticPreflight::Ready)
    }

    fn preflight_append(
        &mut self,
        input: &mut ProviderJsonlExecutionIo<B>,
    ) -> Result<JsonlFamilySemanticPreflight, CaptureError> {
        if self.initialize()? == JsonlFamilySemanticPreflight::RetryReplacement {
            return Ok(JsonlFamilySemanticPreflight::RetryReplacement);
        }
        loop {
            let Some(record) = input.next_record()? else {
                break;
            };
            if !record.complete() || record.oversized() {
                return Ok(JsonlFamilySemanticPreflight::RetryReplacement);
            }
            let bytes = input.record_bytes(record)?.to_vec();
            let reducer = self
                .reducer
                .as_mut()
                .ok_or(CaptureError::SystemInvariant("fx append reducer is absent"))?;
            match reducer {
                ReplayReducer::Suffix(reducer) => reducer
                    .consume(&bytes, record.byte_start(), record.byte_end_exclusive())
                    .map_err(fx_error)?,
                _ => {
                    return Err(CaptureError::SystemInvariant(
                        "fx append reducer is not suffix",
                    ))
                }
            }
            self.represented =
                self.represented
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "fx represented record count overflowed",
                    ))?;
            input.release_record_buffer()?;
        }
        let reducer = self
            .reducer
            .take()
            .ok_or(CaptureError::SystemInvariant("fx append reducer is absent"))?;
        let ReplayReducer::Suffix(reducer) = reducer else {
            return Err(CaptureError::SystemInvariant(
                "fx append reducer is not suffix",
            ));
        };
        match reducer.finish().map_err(fx_error)? {
            (SuffixDisposition::ReplaceCanonicalState, _)
            | (SuffixDisposition::UnsafePending(_), _) => {
                Ok(JsonlFamilySemanticPreflight::RetryReplacement)
            }
            (SuffixDisposition::AppendNewTurns(_), _) => {
                // Preflight proves that the admitted suffix can extend the certified
                // checkpoint, but the shared family rewinds its physical input before
                // publication. Recreate the reducer so execution consumes that suffix
                // again and leaves the shared reader at a terminal checkpoint.
                self.represented = 0;
                self.initialize()
            }
        }
    }

    fn finish_replay(&mut self) -> Result<(), CaptureError> {
        let reducer = self
            .reducer
            .take()
            .ok_or_else(|| CaptureError::SystemInvariant("fx semantic reducer is absent"))?;
        let (records, checkpoint) = match reducer {
            ReplayReducer::Cold(reducer) => {
                let (replay, _) = reducer.finish().map_err(fx_error)?;
                let records = project_canonical_state(
                    ProjectionBinding {
                        source: &self.source,
                        native_session_id: &replay.state.id,
                    },
                    &replay.state,
                )
                .map_err(fx_error)?;
                (
                    records,
                    Some(
                        TypedKey::bytes(
                            encode_replay_checkpoint(&replay.checkpoint).map_err(fx_error)?,
                        )
                        .map_err(contract)?,
                    ),
                )
            }
            ReplayReducer::Suffix(reducer) => match reducer.finish().map_err(fx_error)? {
                (SuffixDisposition::AppendNewTurns(replay), _) => {
                    let records = project_logical_turns(
                        ProjectionBinding {
                            source: &self.source,
                            native_session_id: &replay.checkpoint.native_session_id,
                        },
                        &replay.new_turns,
                    )
                    .map_err(fx_error)?;
                    (
                        records,
                        Some(
                            TypedKey::bytes(
                                encode_replay_checkpoint(&replay.checkpoint).map_err(fx_error)?,
                            )
                            .map_err(contract)?,
                        ),
                    )
                }
                (SuffixDisposition::ReplaceCanonicalState, _)
                | (SuffixDisposition::UnsafePending(_), _) => {
                    return Err(CaptureError::SystemInvariant(
                        "fx suffix replacement was not retried before projection",
                    ))
                }
            },
        };
        self.pages
            .extend(JsonlFamilySemanticPage::split_bounded::<CaptureError>(
                records,
            )?);
        self.checkpoint = checkpoint;
        self.done = true;
        Ok(())
    }
}

impl<B: ProviderRuntimeBinding> JsonlFamilySemanticExecutor for FxSemanticExecutor<B> {
    type Runtime = ctx_history_provider_runtime::ProviderJsonlRuntime<B>;

    fn preflight(
        &mut self,
        input: &mut ProviderJsonlExecutionIo<B>,
    ) -> Result<JsonlFamilySemanticPreflight, CaptureError> {
        if self.mode == JsonlFamilyProjectionMode::CertifiedAppend {
            return self.preflight_append(input);
        }
        self.initialize()?;
        while let Some(record) = input.next_record()? {
            if !record.complete() || record.oversized() {
                return Err(CaptureError::InvalidPayload(
                    "fx committed frame is incomplete or oversized".to_owned(),
                ));
            }
            input.release_record_buffer()?;
        }
        Ok(JsonlFamilySemanticPreflight::Ready)
    }

    fn next_page(
        &mut self,
        input: &mut ProviderJsonlExecutionIo<B>,
        _worker: &mut ProviderJsonlWorkerContext<B>,
    ) -> Result<Option<JsonlFamilySemanticPage>, CaptureError> {
        if let Some(page) = self.pages.pop_front() {
            return Ok(Some(page));
        }
        if self.done {
            return Ok(None);
        }
        let Some(record) = input.next_record()? else {
            self.finish_replay()?;
            return Ok(self.pages.pop_front());
        };
        if !record.complete() || record.oversized() {
            return Err(CaptureError::InvalidPayload(
                "fx committed frame is incomplete or oversized".to_owned(),
            ));
        }
        let bytes = input.record_bytes(record)?.to_vec();
        match self
            .reducer
            .as_mut()
            .ok_or_else(|| CaptureError::SystemInvariant("fx semantic reducer is absent"))?
        {
            ReplayReducer::Cold(reducer) => reducer
                .consume(
                    &bytes,
                    record.byte_start(),
                    record.byte_end_exclusive(),
                    &TempFileScratch,
                )
                .map_err(fx_error)?,
            ReplayReducer::Suffix(reducer) => reducer
                .consume(&bytes, record.byte_start(), record.byte_end_exclusive())
                .map_err(fx_error)?,
        }
        self.represented = self.represented.checked_add(1).ok_or_else(|| {
            CaptureError::SystemInvariant("fx represented record count overflowed")
        })?;
        Ok(Some(JsonlFamilySemanticPage::new(Vec::new())))
    }

    fn finish(self: Box<Self>) -> Result<JsonlFamilySemanticSummary, CaptureError> {
        if !self.done || !self.pages.is_empty() {
            return Err(CaptureError::SystemInvariant(
                "fx semantic executor finished before replay completed",
            ));
        }
        Ok(JsonlFamilySemanticSummary::new(
            self.represented,
            0,
            self.checkpoint,
        ))
    }
}

fn contract(error: ctx_history_core::ProjectionContractError) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
fn fx_error(error: FxProviderError) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
fn state_error() -> CaptureError {
    CaptureError::InvalidPayload("fx JSONL adapter state lock was poisoned".to_owned())
}
