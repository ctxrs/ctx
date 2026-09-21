use std::path::Path;

use crate::protocol::{BlameResult, NumberedEvidence, ResolvedBlameTarget, StableEntityId};
use anyhow::Result;
use ctx_history_core::CoreRecord;
use ctx_history_snapshot_reader::{CoreSnapshot, SnapshotContract, SnapshotPageBudget};

use crate::evidence_preview::{
    EvidencePreviewModel, MAX_EVIDENCE_PREVIEW_CITATIONS, VerifiedEvidenceRecord,
    project_evidence_previews,
};

const MAX_EVIDENCE_CORE_BYTES_PER_CITATION: usize = 64 * 1_024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EvidenceHydrationBudget {
    aggregate: SnapshotPageBudget,
    per_record: SnapshotPageBudget,
}

struct HydratedEvidenceBatch {
    generation_id: String,
    records: Vec<CoreRecord>,
}

pub fn hydrate_evidence_previews(data_root: &Path, result: &BlameResult) -> EvidencePreviewModel {
    hydrate_evidence_previews_with(result, |generation_id, event_ids, budget| {
        let contract = SnapshotContract::current()?;
        let snapshot = CoreSnapshot::open(data_root, generation_id, &contract)?;
        let hydrated_generation_id = snapshot.generation_id().to_owned();
        Ok(snapshot
            .core_records_by_ids_if_bounded(event_ids, budget.aggregate, budget.per_record)?
            .map(|batch| HydratedEvidenceBatch {
                generation_id: hydrated_generation_id,
                records: batch.records,
            }))
    })
}

fn hydrate_evidence_previews_with(
    result: &BlameResult,
    load: impl FnOnce(
        &str,
        &[StableEntityId],
        EvidenceHydrationBudget,
    ) -> Result<Option<HydratedEvidenceBatch>>,
) -> EvidencePreviewModel {
    let unavailable = || EvidencePreviewModel {
        previews: Vec::new(),
    };
    if !matches!(&result.target, ResolvedBlameTarget::File { .. }) {
        return unavailable();
    }

    let mut selected = result.evidence.iter().collect::<Vec<_>>();
    selected.sort_by_key(|evidence| evidence.number);
    selected.truncate(MAX_EVIDENCE_PREVIEW_CITATIONS);
    let Some(first) = selected.first() else {
        return unavailable();
    };
    let generation_id = first.citation.core_generation_id.as_str();
    if !is_lower_sha256(generation_id)
        || selected.iter().any(|evidence| {
            evidence.citation.core_generation_id != generation_id
                || evidence.citation.byte_range.is_some()
                || evidence
                    .citation
                    .evidence_sha256
                    .as_deref()
                    .is_none_or(|digest| !is_lower_sha256(digest))
        })
    {
        return unavailable();
    }

    let mut event_ids = Vec::with_capacity(selected.len());
    for evidence in &selected {
        let event_id = evidence.citation.event_id;
        if !event_ids.contains(&event_id) {
            event_ids.push(event_id);
        }
    }
    let Ok(Some(batch)) = load(
        generation_id,
        &event_ids,
        evidence_hydration_budget(event_ids.len()),
    ) else {
        return unavailable();
    };
    if batch.generation_id != generation_id
        || batch.records.len() != event_ids.len()
        || batch
            .records
            .iter()
            .zip(&event_ids)
            .any(|(record, event_id)| record.event_id != *event_id)
    {
        return unavailable();
    }

    let verified = selected
        .iter()
        .filter_map(|numbered| verified_record(numbered, generation_id, &event_ids, &batch.records))
        .collect::<Vec<_>>();
    project_evidence_previews(result, &verified)
}

fn evidence_hydration_budget(unique_records: usize) -> EvidenceHydrationBudget {
    let bounded_records = unique_records.min(MAX_EVIDENCE_PREVIEW_CITATIONS);
    let aggregate_bytes = MAX_EVIDENCE_CORE_BYTES_PER_CITATION * bounded_records;
    EvidenceHydrationBudget {
        aggregate: SnapshotPageBudget::new(aggregate_bytes, aggregate_bytes),
        per_record: SnapshotPageBudget::new(
            MAX_EVIDENCE_CORE_BYTES_PER_CITATION,
            MAX_EVIDENCE_CORE_BYTES_PER_CITATION,
        ),
    }
}

fn verified_record<'a>(
    numbered: &'a NumberedEvidence,
    generation_id: &str,
    event_ids: &[StableEntityId],
    records: &'a [CoreRecord],
) -> Option<VerifiedEvidenceRecord<'a>> {
    let position = event_ids
        .iter()
        .position(|event_id| *event_id == numbered.citation.event_id)?;
    let record = records.get(position)?;
    VerifiedEvidenceRecord::new(numbered, generation_id, record)
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
pub(crate) mod tests;
