use std::collections::BTreeSet;

use ctx_history_core::EventType;
use ctx_history_index_query::{IndexError, VerifiedIndex};

/// User-facing coverage facts for one verified Core generation.
///
/// This DTO is deliberately separate from command JSON. It gives human
/// presentation stable product concepts without exposing manifest SourceKey
/// cardinality or changing a machine-readable schema. Inventory-owned fields
/// remain optional until a caller joins a current provider-location snapshot.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct HistoryHealthReport {
    pub contributing_agent_histories: Vec<String>,
    pub provider_roots: Option<HistoryRootCoverage>,
    pub sessions: u64,
    pub messages: u64,
    pub tool_calls: u64,
    pub data: HistoryDataCoverage,
    pub source_failures: u64,
    pub rejected_records: u64,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct HistoryRootCoverage {
    pub included: u64,
    pub partial: u64,
    pub excluded: u64,
    pub unknown: u64,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct HistoryDataCoverage {
    /// Bytes admitted into the verified generation after source processing.
    pub processed: u64,
    /// Exact excluded bytes, when a joined inventory/diagnostic authority can
    /// reconcile them. `None` must never be presented as zero.
    pub excluded: Option<u64>,
}

impl HistoryHealthReport {
    pub fn is_partial(&self) -> bool {
        self.source_failures > 0
            || self.rejected_records > 0
            || self
                .provider_roots
                .is_some_and(|roots| roots.partial > 0 || roots.excluded > 0 || roots.unknown > 0)
    }

    pub fn record_refresh_diagnostics(&mut self, source_failures: u64, rejected_records: u64) {
        self.source_failures = source_failures;
        self.rejected_records = rejected_records;
        if source_failures > 0 || rejected_records > 0 {
            self.data.excluded = None;
        }
    }

    pub fn record_inventory(
        &mut self,
        provider_roots: HistoryRootCoverage,
        excluded_bytes: Option<u64>,
    ) {
        self.provider_roots = Some(provider_roots);
        self.data.excluded = excluded_bytes;
    }
}

/// Adapts exact generation-owned facts into the human health DTO.
///
/// `certified_source_bytes` is deliberately called processed data here. It is
/// not total discovery-inventory size and cannot establish excluded bytes.
pub fn history_health_report(index: &VerifiedIndex) -> Result<HistoryHealthReport, IndexError> {
    let manifest = index.load_manifest()?;
    let contributing_agent_histories = manifest
        .sources
        .iter()
        .filter(|source| source.counts().indexed_documents > 0)
        .map(|source| source.observation().source().provider().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    Ok(HistoryHealthReport {
        contributing_agent_histories,
        provider_roots: None,
        sessions: index.session_count()?,
        messages: index.event_type_count(EventType::Message.as_str())?,
        tool_calls: index.event_type_count(EventType::ToolCall.as_str())?,
        data: HistoryDataCoverage {
            processed: manifest.certified_source_bytes,
            excluded: None,
        },
        source_failures: 0,
        rejected_records: 0,
    })
}
