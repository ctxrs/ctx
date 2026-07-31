use super::*;

/// Exact cardinalities of the generation that was verified after publication.
///
/// These are current-state facts, not deltas attributed to one refresh.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshCurrent {
    pub(crate) source_count: usize,
    pub(crate) indexed_documents: u64,
    pub(crate) complete_records: u64,
    pub(crate) retained_records: u64,
    pub(crate) rejected_records: u64,
    pub(crate) ignored_records: u64,
    pub(crate) certified_source_bytes: u64,
    pub(crate) sources_with_rejections: usize,
    pub(crate) removed_source_count: usize,
}

impl SourceBackedRefreshCurrent {
    pub(super) fn from_sources(
        sources: &[CertifiedSource],
        removed_source_count: usize,
    ) -> Result<Self> {
        let mut current = Self {
            source_count: sources.len(),
            removed_source_count,
            ..Self::default()
        };
        for source in sources {
            let counts = source.counts();
            current.add_counts(counts)?;
            current.sources_with_rejections = current
                .sources_with_rejections
                .checked_add(usize::from(counts.rejected_records > 0))
                .ok_or_else(|| anyhow!("source-backed current rejection-source count overflow"))?;
        }
        Ok(current)
    }

    fn add_counts(&mut self, counts: ScannedSourceCounts) -> Result<()> {
        self.indexed_documents =
            checked_current_count(self.indexed_documents, counts.indexed_documents)?;
        self.complete_records =
            checked_current_count(self.complete_records, counts.complete_records)?;
        self.retained_records =
            checked_current_count(self.retained_records, counts.retained_records)?;
        self.rejected_records =
            checked_current_count(self.rejected_records, counts.rejected_records)?;
        self.ignored_records = checked_current_count(self.ignored_records, counts.ignored_records)?;
        self.certified_source_bytes =
            checked_current_count(self.certified_source_bytes, counts.certified_bytes)?;
        Ok(())
    }

    pub(super) fn to_json(self) -> Value {
        json!({
            "current_source_count": self.source_count,
            "current_indexed_documents": self.indexed_documents,
            "current_complete_records": self.complete_records,
            "current_retained_records": self.retained_records,
            "current_rejected_records": self.rejected_records,
            "current_ignored_records": self.ignored_records,
            "current_certified_source_bytes": self.certified_source_bytes,
            "current_sources_with_rejections": self.sources_with_rejections,
            "removed_source_count": self.removed_source_count,
        })
    }
}

fn checked_current_count(current: u64, next: u64) -> Result<u64> {
    current
        .checked_add(next)
        .ok_or_else(|| anyhow!("source-backed current generation count overflow"))
}
