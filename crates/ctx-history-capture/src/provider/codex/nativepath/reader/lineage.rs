use std::{
    mem::size_of,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use super::*;
use crate::provider::codex::nativepath::record::codex_lineage_call_id_digest;

const MAX_LINEAGE_FACT_BYTES_PER_TASK: usize = 64 * 1024 * 1024;
pub(crate) const CODEX_LINEAGE_EXHAUSTED_SENTINEL: &str = "Codex lineage working set exhausted";
// Keep a defensive logical-count ceiling, but derive it from the same fixed-
// width memory budget instead of imposing an unrelated lower corpus-size cap.
const MAX_LINEAGE_FACTS_PER_TASK: usize =
    MAX_LINEAGE_FACT_BYTES_PER_TASK / size_of::<CodexLineageFactV0>();
const LINEAGE_FACT_GROWTH: usize = 64;
const LINEAGE_CONTAINER_CHARGE: usize = 128;

#[derive(Debug)]
pub(crate) struct CodexLineageFactBudgetV0 {
    charged: AtomicUsize,
    facts: AtomicUsize,
    byte_limit: usize,
    fact_limit: usize,
}

impl Default for CodexLineageFactBudgetV0 {
    fn default() -> Self {
        Self {
            charged: AtomicUsize::new(0),
            facts: AtomicUsize::new(0),
            byte_limit: MAX_LINEAGE_FACT_BYTES_PER_TASK,
            fact_limit: MAX_LINEAGE_FACTS_PER_TASK,
        }
    }
}

impl CodexLineageFactBudgetV0 {
    #[cfg(test)]
    pub(crate) fn with_limits(byte_limit: usize, fact_limit: usize) -> Self {
        Self {
            charged: AtomicUsize::new(0),
            facts: AtomicUsize::new(0),
            byte_limit,
            fact_limit,
        }
    }

    fn charge(&self, bytes: usize) -> Result<()> {
        self.charged
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|next| *next <= self.byte_limit)
            })
            .map(|_| ())
            .map_err(|_| lineage_exhausted())
    }

    fn release(&self, bytes: usize) {
        self.charged.fetch_sub(bytes, Ordering::AcqRel);
    }

    fn charge_facts(&self, facts: usize) -> Result<()> {
        self.facts
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(facts)
                    .filter(|next| *next <= self.fact_limit)
            })
            .map(|_| ())
            .map_err(|_| lineage_exhausted())
    }

    fn release_facts(&self, facts: usize) {
        self.facts.fetch_sub(facts, Ordering::AcqRel);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CodexLineageFactKindV0 {
    Call,
    Result,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CodexLineageFactV0 {
    call_id_sha256: [u8; 32],
    kind: CodexLineageFactKindV0,
}

#[derive(Debug)]
pub(crate) struct CodexLineageFactsV0 {
    facts: Vec<CodexLineageFactV0>,
    has_unattributed_ambiguity: bool,
    sealed: bool,
    charged: usize,
    charged_facts: usize,
    budget: Arc<CodexLineageFactBudgetV0>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CodexLineageFactMarkV0 {
    len: usize,
    has_unattributed_ambiguity: bool,
}

impl CodexLineageFactsV0 {
    pub(crate) fn new(budget: Arc<CodexLineageFactBudgetV0>) -> Result<Self> {
        budget.charge(LINEAGE_CONTAINER_CHARGE)?;
        Ok(Self {
            facts: Vec::new(),
            has_unattributed_ambiguity: false,
            sealed: false,
            charged: LINEAGE_CONTAINER_CHARGE,
            charged_facts: 0,
            budget,
        })
    }

    pub(super) fn record(&mut self, evidence: CodexLineageRecordEvidence<'_>) -> Result<()> {
        match evidence {
            CodexLineageRecordEvidence::None => {}
            CodexLineageRecordEvidence::UnattributedAmbiguity => {
                self.has_unattributed_ambiguity = true;
            }
            CodexLineageRecordEvidence::Call(call_id) => {
                self.push(CodexLineageFactKindV0::Call, call_id)?;
            }
            CodexLineageRecordEvidence::Result(call_id) => {
                self.push(CodexLineageFactKindV0::Result, call_id)?;
            }
            CodexLineageRecordEvidence::Ambiguous(call_id) => {
                self.push(CodexLineageFactKindV0::Ambiguous, call_id)?;
            }
            CodexLineageRecordEvidence::AmbiguousDigests(digests) => {
                for digest in digests {
                    self.push_digest(CodexLineageFactKindV0::Ambiguous, *digest)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn seal(&mut self) {
        if self.sealed {
            return;
        }
        let previous_len = self.facts.len();
        self.facts.sort_unstable();
        let mut read = 0_usize;
        let mut write = 0_usize;
        while read < self.facts.len() {
            let digest = self.facts[read].call_id_sha256;
            let mut call_count = 0_usize;
            let mut result_count = 0_usize;
            let mut ambiguous = false;
            while read < self.facts.len() && self.facts[read].call_id_sha256 == digest {
                match self.facts[read].kind {
                    CodexLineageFactKindV0::Call => call_count = call_count.saturating_add(1),
                    CodexLineageFactKindV0::Result => result_count = result_count.saturating_add(1),
                    CodexLineageFactKindV0::Ambiguous => ambiguous = true,
                }
                read = read.saturating_add(1);
            }
            if call_count > 1 || result_count > 1 {
                ambiguous = true;
            }
            for kind in [
                (call_count != 0).then_some(CodexLineageFactKindV0::Call),
                (result_count != 0).then_some(CodexLineageFactKindV0::Result),
                ambiguous.then_some(CodexLineageFactKindV0::Ambiguous),
            ]
            .into_iter()
            .flatten()
            {
                self.facts[write] = CodexLineageFactV0 {
                    call_id_sha256: digest,
                    kind,
                };
                write = write.saturating_add(1);
            }
        }
        self.facts.truncate(write);
        let released_facts = previous_len.saturating_sub(write);
        self.charged_facts = self
            .charged_facts
            .checked_sub(released_facts)
            .expect("Codex lineage logical-fact accounting is balanced");
        self.budget.release_facts(released_facts);
        self.sealed = true;
    }

    pub(super) fn mark(&self) -> CodexLineageFactMarkV0 {
        CodexLineageFactMarkV0 {
            len: self.facts.len(),
            has_unattributed_ambiguity: self.has_unattributed_ambiguity,
        }
    }

    pub(super) fn restore(&mut self, mark: CodexLineageFactMarkV0) {
        let released_facts = self.facts.len().saturating_sub(mark.len);
        self.facts.truncate(mark.len);
        self.charged_facts = self
            .charged_facts
            .checked_sub(released_facts)
            .expect("Codex lineage logical-fact accounting is balanced");
        self.budget.release_facts(released_facts);
        self.has_unattributed_ambiguity = mark.has_unattributed_ambiguity;
    }

    pub(crate) fn presence(
        &self,
        origin_call_id: &str,
        result_call_id: &str,
    ) -> CodexLineageFactPresenceV0 {
        if origin_call_id.is_empty() || result_call_id.is_empty() {
            return CodexLineageFactPresenceV0::Unproven;
        }
        let origin = codex_lineage_call_id_digest(origin_call_id);
        let result = codex_lineage_call_id_digest(result_call_id);
        let has_call = self.contains(origin, CodexLineageFactKindV0::Call);
        let has_result = self.contains(result, CodexLineageFactKindV0::Result);
        let ambiguous = self.has_unattributed_ambiguity
            || self.contains(origin, CodexLineageFactKindV0::Ambiguous)
            || self.contains(result, CodexLineageFactKindV0::Ambiguous);
        if has_call && has_result && !ambiguous {
            CodexLineageFactPresenceV0::Present
        } else if ambiguous || has_call || has_result {
            CodexLineageFactPresenceV0::Unproven
        } else {
            CodexLineageFactPresenceV0::Absent
        }
    }

    fn push(&mut self, kind: CodexLineageFactKindV0, call_id: &str) -> Result<()> {
        if call_id.is_empty() || self.sealed {
            self.has_unattributed_ambiguity = true;
            return Ok(());
        }
        self.push_digest(kind, codex_lineage_call_id_digest(call_id))
    }

    fn push_digest(&mut self, kind: CodexLineageFactKindV0, digest: [u8; 32]) -> Result<()> {
        if self.sealed {
            self.has_unattributed_ambiguity = true;
            return Ok(());
        }
        if self.facts.len() == self.facts.capacity() {
            let requested = LINEAGE_FACT_GROWTH;
            let bytes = requested
                .checked_mul(size_of::<CodexLineageFactV0>())
                .ok_or_else(lineage_exhausted)?;
            self.budget.charge(bytes)?;
            if self.facts.try_reserve_exact(requested).is_err() {
                self.budget.release(bytes);
                return Err(lineage_exhausted());
            }
            let actual_facts = self.facts.capacity().saturating_sub(self.facts.len());
            let actual = actual_facts.saturating_mul(size_of::<CodexLineageFactV0>());
            if actual > bytes {
                if let Err(error) = self.budget.charge(actual - bytes) {
                    self.facts.shrink_to(self.facts.len());
                    self.budget.release(bytes);
                    return Err(error);
                }
            } else {
                self.budget.release(bytes - actual);
            }
            self.charged = self.charged.saturating_add(actual);
        }
        self.budget.charge_facts(1)?;
        let Some(charged_facts) = self.charged_facts.checked_add(1) else {
            self.budget.release_facts(1);
            return Err(lineage_exhausted());
        };
        self.charged_facts = charged_facts;
        self.facts.push(CodexLineageFactV0 {
            call_id_sha256: digest,
            kind,
        });
        Ok(())
    }

    fn contains(&self, digest: [u8; 32], kind: CodexLineageFactKindV0) -> bool {
        self.facts
            .binary_search(&CodexLineageFactV0 {
                call_id_sha256: digest,
                kind,
            })
            .is_ok()
    }
}

impl Drop for CodexLineageFactsV0 {
    fn drop(&mut self) {
        self.budget.release(self.charged);
        self.budget.release_facts(self.charged_facts);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexLineageFactPresenceV0 {
    Present,
    Absent,
    Unproven,
}

fn lineage_exhausted() -> CaptureError {
    CaptureError::InvalidPayload(CODEX_LINEAGE_EXHAUSTED_SENTINEL.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::codex::nativepath::tests::{discover_one, session_meta, write_source};

    #[test]
    fn fact_capacity_reservation_failure_is_typed_and_non_panicking() {
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(256, 64));
        let mut facts = CodexLineageFactsV0::new(budget).unwrap();
        assert!(matches!(
            facts.record(CodexLineageRecordEvidence::Call("bounded-call")),
            Err(CaptureError::InvalidPayload(detail))
                if detail == CODEX_LINEAGE_EXHAUSTED_SENTINEL
        ));
    }

    #[test]
    fn duplicate_exact_facts_compact_to_ambiguity() {
        let budget = Arc::new(CodexLineageFactBudgetV0::default());
        let mut facts = CodexLineageFactsV0::new(budget).unwrap();
        facts
            .record(CodexLineageRecordEvidence::Call("duplicate"))
            .unwrap();
        facts
            .record(CodexLineageRecordEvidence::Call("duplicate"))
            .unwrap();
        facts
            .record(CodexLineageRecordEvidence::Result("duplicate"))
            .unwrap();
        facts.seal();
        assert_eq!(
            facts.presence("duplicate", "duplicate"),
            CodexLineageFactPresenceV0::Unproven
        );
    }

    #[test]
    fn logical_fact_budget_counts_entries_not_allocator_growth() {
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(1024 * 1024, 2));
        let mut first = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
        first
            .record(CodexLineageRecordEvidence::Call("first"))
            .unwrap();
        first.seal();
        assert_eq!(budget.facts.load(Ordering::Acquire), 1);

        let mut second = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
        second
            .record(CodexLineageRecordEvidence::Call("second"))
            .unwrap();
        assert_eq!(budget.facts.load(Ordering::Acquire), 2);
        assert!(matches!(
            second.record(CodexLineageRecordEvidence::Call("exhausted")),
            Err(CaptureError::InvalidPayload(detail))
                if detail == CODEX_LINEAGE_EXHAUSTED_SENTINEL
        ));
        second.seal();
        assert_eq!(budget.facts.load(Ordering::Acquire), 2);
    }

    #[test]
    fn thousands_of_small_fact_sets_are_charged_by_live_facts() {
        const SETS: usize = 6_073;
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(
            MAX_LINEAGE_FACT_BYTES_PER_TASK,
            SETS,
        ));
        let mut retained = Vec::with_capacity(SETS);
        for index in 0..SETS {
            let mut facts = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
            facts
                .record(CodexLineageRecordEvidence::Call(&format!(
                    "small-set-{index}"
                )))
                .unwrap();
            facts.seal();
            retained.push(facts);
        }
        assert_eq!(budget.facts.load(Ordering::Acquire), SETS);
        assert!(budget.charged.load(Ordering::Acquire) < MAX_LINEAGE_FACT_BYTES_PER_TASK);
        drop(retained);
        assert_eq!(budget.facts.load(Ordering::Acquire), 0);
        assert_eq!(budget.charged.load(Ordering::Acquire), 0);
    }

    #[test]
    fn rollback_and_seal_release_logical_fact_charges() {
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(1024 * 1024, 8));
        {
            let mut facts = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
            facts
                .record(CodexLineageRecordEvidence::Call("retained"))
                .unwrap();
            let mark = facts.mark();
            facts
                .record(CodexLineageRecordEvidence::Call("rolled-back"))
                .unwrap();
            facts.restore(mark);
            assert_eq!(budget.facts.load(Ordering::Acquire), 1);
            facts
                .record(CodexLineageRecordEvidence::Ambiguous("duplicate"))
                .unwrap();
            facts
                .record(CodexLineageRecordEvidence::Ambiguous("duplicate"))
                .unwrap();
            assert_eq!(budget.facts.load(Ordering::Acquire), 3);
            facts.seal();
            assert_eq!(budget.facts.load(Ordering::Acquire), 2);
        }
        assert_eq!(budget.facts.load(Ordering::Acquire), 0);
        assert_eq!(budget.charged.load(Ordering::Acquire), 0);
    }

    #[test]
    fn exact_checkpoint_replay_extracts_prefix_facts_from_its_certifying_pass() {
        let call = serde_json::json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {"type": "function_call", "call_id": "checkpoint-call"}
        });
        let result = serde_json::json!({
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "checkpoint-call",
                "output": "complete"
            }
        });
        let contents = format!(
            "{}{call}\n{result}\n",
            session_meta("checkpoint-lineage-owner")
        );
        let (_temp, path) = write_source(&contents);
        let source = discover_one(&path, "checkpoint-lineage-owner");
        let mut initial = CodexNativeScanner::new_source_backed_v0(source, None).unwrap();
        while initial.next_page().unwrap().is_some() {}
        let initial = initial.finish().unwrap();
        let proof = initial
            .bind_checkpoint(
                "checkpoint-lineage-source",
                CodexCheckpointGeneration::new(1),
            )
            .unwrap()
            .unwrap();

        let budget = Arc::new(CodexLineageFactBudgetV0::default());
        let facts = CodexLineageFactsV0::new(budget).unwrap();
        let source = discover_one(&path, "checkpoint-lineage-owner");
        let mut replay =
            CodexNativeScanner::new_source_backed_with_lineage_v0(source, Some(&proof), facts)
                .unwrap();
        assert!(replay.next_page().unwrap().is_none());
        let replay = replay.finish().unwrap();
        let facts = replay.lineage_facts.unwrap();
        assert_eq!(
            facts.presence("checkpoint-call", "checkpoint-call"),
            CodexLineageFactPresenceV0::Present
        );
    }
}
