use anyhow::{anyhow, Result};
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use ctx_history_index::{CoreEventPageBudget, CoreEventRecord, VerifiedIndex};

use crate::{semantic_core_content_is_control, SemanticDocumentBuilder, SemanticEventDocument};

const MAX_LITE_TURN_PAIRING_PAGE_RECORDS: usize = 64;
const LITE_TURN_PAIRING_BUDGET: CoreEventPageBudget =
    CoreEventPageBudget::new(64 * 1024 * 1024, 16 * 1024 * 1024);

/// Builds the canonical semantic projection from one pinned Core generation.
pub struct SourceBackedSemanticDocumentBuilder<'a> {
    index: &'a VerifiedIndex,
    pairing_page_records: usize,
    pairing_budget: CoreEventPageBudget,
}

impl<'a> SourceBackedSemanticDocumentBuilder<'a> {
    pub fn new(index: &'a VerifiedIndex) -> Self {
        Self {
            index,
            pairing_page_records: MAX_LITE_TURN_PAIRING_PAGE_RECORDS,
            pairing_budget: LITE_TURN_PAIRING_BUDGET,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_pairing_limits_for_test(
        index: &'a VerifiedIndex,
        pairing_page_records: usize,
        pairing_budget: CoreEventPageBudget,
    ) -> Self {
        Self {
            index,
            pairing_page_records,
            pairing_budget,
        }
    }

    fn paired_assistant(&self, anchor: &CoreEventRecord) -> Result<Option<(String, i64)>> {
        Ok(self.index.semantic_lite_turn_assistant(
            anchor,
            self.pairing_page_records,
            self.pairing_budget,
        )?)
    }
}

impl SemanticDocumentBuilder for SourceBackedSemanticDocumentBuilder<'_> {
    fn build_document(
        &mut self,
        record: &CoreEventRecord,
    ) -> Result<Option<SemanticEventDocument>> {
        let user_text = record.core_record.content.meaningful_text();
        if user_text.trim().is_empty() {
            return Ok(None);
        }
        let mut sections = vec![format!("user:\n{}", user_text.trim())];
        let mut occurred_at_ms = record.occurred_at_unix_ms.unwrap_or_default();
        if !semantic_core_content_is_control(&sections[0]) {
            if let Some((assistant_text, assistant_at_ms)) = self.paired_assistant(record)? {
                sections.push(format!("assistant:\n{}", assistant_text.trim()));
                occurred_at_ms = occurred_at_ms.max(assistant_at_ms);
            }
        }
        let literal_facts = record
            .core_record
            .content
            .activity
            .as_ref()
            .map_or_else(Vec::new, |activity| activity.facts.clone());
        Ok(Some(SemanticEventDocument::new(
            record.event_id.as_uuid(),
            Some(record.session_id.as_uuid()),
            record.event_sequence,
            occurred_at_ms,
            parse_core_event_type(&record.event_type)?,
            record
                .role
                .as_deref()
                .map(parse_core_event_role)
                .transpose()?,
            "lite_turn".to_owned(),
            Some(parse_core_provider(&record.provider)?),
            Some(record.source_format.clone()),
            record.core_record.agent_scope,
            literal_facts,
            sections.join("\n\n"),
        )))
    }
}

fn parse_core_event_type(value: &str) -> Result<EventType> {
    value
        .parse()
        .map_err(|error| anyhow!("invalid Core event type {value:?}: {error}"))
}

fn parse_core_event_role(value: &str) -> Result<EventRole> {
    value
        .parse()
        .map_err(|error| anyhow!("invalid Core event role {value:?}: {error}"))
}

fn parse_core_provider(value: &str) -> Result<CaptureProvider> {
    value
        .parse()
        .map_err(|error| anyhow!("invalid Core provider {value:?}: {error}"))
}
