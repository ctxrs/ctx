use chrono::Utc;
use ctx_history_core::{
    AgentType, Artifact, ArtifactKind, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, Confidence, ContextCitationType, EntityTimestamps, Event, EventRole,
    EventType, Fidelity, FileChangeKind, FileTouched, HistoryRecord, HistoryRecordLink,
    HistoryRecordLinkTargetType, HistoryRecordLinkType, RedactionState, Run, RunStatus, RunType,
    Session, SessionHistoryArchive, SessionStatus, Summary, SummaryKind, SyncMetadata, SyncState,
    VcsChange, VcsChangeKind, VcsHost, VcsKind, VcsWorkspace, Visibility,
};
use serde::Serialize;
use std::{collections::BTreeSet, path::Path};
use uuid::Uuid;

use crate::filters::{
    context_has_excluded_provider_session, event_hit_matches_excluded_provider_session,
    hit_matches_excluded_provider_session,
};
use crate::model::{HitMetadata, RecordContext};
use crate::query::{
    FILTERED_SEARCH_MAX_PAGES, FILTERED_SEARCH_PAGE_SIZE, LARGE_EVENT_CORPUS_THRESHOLD,
};
use crate::source::empty_hit;
use crate::{
    display_snippet, event_preview_text, search_packet, search_packet_terms, PacketOptions,
    ProviderSessionFilter, SearchFilters, SearchResultMode, SearchResultScope, MAX_RESULT_LIMIT,
};
use ctx_history_store::EventSearchHit;

mod support;
use support::{
    excluded_filter, fixed_time, maybe_write_synthetic_search_smoke_artifact, new_link_id,
    sync_metadata, test_store, timestamps,
};

mod exclusion;
mod fast_path;
mod filter_scans;
mod perf;
mod ranking;
mod rich;
mod snippets;
mod source_metadata;
