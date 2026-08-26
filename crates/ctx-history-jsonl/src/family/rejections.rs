use ctx_history_capture_runtime::{
    SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDraft,
    SourceBackedRecordRejectionDrafts,
};
use ctx_history_core::{CaptureProvider, SourceKey};

use super::JsonlRecordRef;

/// Bounded diagnostics and exact aggregate count for rejected JSONL records.
#[derive(Debug)]
pub struct JsonlRecordRejections {
    source: SourceKey,
    provider: CaptureProvider,
    source_selector: String,
    count: u64,
    drafts: SourceBackedRecordRejectionDrafts,
}

impl JsonlRecordRejections {
    pub fn new(
        source: SourceKey,
        provider: CaptureProvider,
        source_selector: impl Into<String>,
    ) -> Self {
        Self {
            source,
            provider,
            source_selector: source_selector.into(),
            count: 0,
            drafts: SourceBackedRecordRejectionDrafts::default(),
        }
    }

    pub fn record(
        &mut self,
        record: JsonlRecordRef<'_>,
        class: SourceBackedRecordRejectionClass,
        detail: impl Into<String>,
    ) {
        self.count = self.count.saturating_add(1);
        self.drafts.record(SourceBackedRecordRejectionDraft {
            source: self.source.clone(),
            provider: self.provider,
            source_selector: self.source_selector.clone(),
            line_number: record.evidence().physical_ordinal().saturating_add(1),
            payload_type: None,
            class,
            detail: detail.into(),
        });
    }

    pub fn malformed(&mut self, record: JsonlRecordRef<'_>, detail: impl Into<String>) {
        self.record(
            record,
            SourceBackedRecordRejectionClass::MalformedRecord,
            detail,
        );
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn take_drafts(&mut self) -> SourceBackedRecordRejectionDrafts {
        std::mem::take(&mut self.drafts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_exact_count_while_diagnostics_are_drained() {
        let source = SourceKey::derive(
            "pi",
            "test-jsonl",
            "test-v1",
            1,
            ctx_history_core::SourceAnchor::provider_native(
                "test.source",
                ctx_history_core::TypedKey::utf8("source").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let mut rejections = JsonlRecordRejections::new(
            source.clone(),
            CaptureProvider::Pi,
            "/provider/source.jsonl",
        );

        rejections.malformed(JsonlRecordRef::for_test(b"{", 4), "malformed test row");

        assert_eq!(rejections.count(), 1);
        let (drafts, omitted) = rejections.take_drafts().into_parts();
        assert_eq!(drafts.len(), 1);
        assert_eq!(omitted, 0);
        assert_eq!(drafts[0].source, source);
        assert_eq!(drafts[0].line_number, 5);
        assert_eq!(
            drafts[0].class,
            SourceBackedRecordRejectionClass::MalformedRecord
        );
        let (drained, omitted) = rejections.take_drafts().into_parts();
        assert!(drained.is_empty());
        assert_eq!(omitted, 0);
        assert_eq!(rejections.count(), 1);
    }
}
