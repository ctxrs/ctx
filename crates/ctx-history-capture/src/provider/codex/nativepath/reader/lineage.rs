use std::{
    mem::size_of,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use super::*;
use crate::provider::codex::nativepath::record::codex_lineage_call_id_digest;

// One authority component owns one semantic budget. The shared JSONL runner
// admits at most 16 components at a time, bounding live lineage fact vectors
// at 1 GiB independently of total corpus breadth.
const MAX_LINEAGE_FACT_BYTES_PER_COMPONENT: usize = 64 * 1024 * 1024;
pub(crate) const CODEX_LINEAGE_EXHAUSTED_SENTINEL: &str = "Codex lineage working set exhausted";
// Keep a defensive logical-count ceiling, but derive it from the same fixed-
// width memory budget instead of imposing an unrelated lower corpus-size cap.
const MAX_LINEAGE_FACTS_PER_COMPONENT: usize =
    MAX_LINEAGE_FACT_BYTES_PER_COMPONENT / size_of::<CodexLineageFactV0>();
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
            byte_limit: MAX_LINEAGE_FACT_BYTES_PER_COMPONENT,
            fact_limit: MAX_LINEAGE_FACTS_PER_COMPONENT,
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

    #[cfg(test)]
    pub(in crate::provider::codex::nativepath) fn charges_for_test(&self) -> (usize, usize) {
        (
            self.charged.load(Ordering::Acquire),
            self.facts.load(Ordering::Acquire),
        )
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
    conservative: bool,
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
        let (charged, conservative) = match budget.charge(LINEAGE_CONTAINER_CHARGE) {
            Ok(()) => (LINEAGE_CONTAINER_CHARGE, false),
            Err(error) if is_lineage_capacity_exhaustion(&error) => (0, true),
            Err(error) => return Err(error),
        };
        Ok(Self {
            facts: Vec::new(),
            has_unattributed_ambiguity: false,
            sealed: conservative,
            conservative,
            charged,
            charged_facts: 0,
            budget,
        })
    }

    pub(super) fn record(&mut self, evidence: CodexLineageRecordEvidence<'_>) -> Result<()> {
        if self.conservative {
            return Ok(());
        }
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

    #[cfg(test)]
    pub(in crate::provider::codex::nativepath) fn record_for_test(
        &mut self,
        evidence: CodexLineageRecordEvidence<'_>,
    ) -> Result<()> {
        self.record(evidence)
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
        if self.conservative {
            return CodexLineageFactPresenceV0::Unproven;
        }
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
        if self.conservative {
            return Ok(());
        }
        if self.sealed {
            self.has_unattributed_ambiguity = true;
            return Ok(());
        }
        if self.facts.len() == self.facts.capacity() && !self.reserve_more(LINEAGE_FACT_GROWTH)? {
            return Ok(());
        }
        if let Err(error) = self.budget.charge_facts(1) {
            if is_lineage_capacity_exhaustion(&error) {
                self.discard_and_seal_conservatively(0);
                return Ok(());
            }
            return Err(error);
        }
        let Some(charged_facts) = self.charged_facts.checked_add(1) else {
            self.budget.release_facts(1);
            return Err(lineage_accounting_invariant());
        };
        self.charged_facts = charged_facts;
        self.facts.push(CodexLineageFactV0 {
            call_id_sha256: digest,
            kind,
        });
        Ok(())
    }

    fn reserve_more(&mut self, requested: usize) -> Result<bool> {
        if self.facts.len() != self.facts.capacity() {
            return Err(lineage_accounting_invariant());
        }
        let Some(bytes) = requested.checked_mul(size_of::<CodexLineageFactV0>()) else {
            return Err(lineage_accounting_invariant());
        };
        if let Err(error) = self.budget.charge(bytes) {
            if is_lineage_capacity_exhaustion(&error) {
                self.discard_and_seal_conservatively(0);
                return Ok(false);
            }
            return Err(error);
        }
        if self.facts.try_reserve_exact(requested).is_err() {
            // The configured byte/fact ceilings are deterministic semantic
            // bounds and degrade to Unproven. An allocator refusal depends on
            // ambient system pressure, so keep it retryable instead of making
            // lineage output vary with the host's transient memory state.
            self.budget.release(bytes);
            return Err(lineage_exhausted());
        }

        let Some(actual_facts) = self.facts.capacity().checked_sub(self.facts.len()) else {
            self.discard_and_seal_conservatively(bytes);
            return Err(lineage_accounting_invariant());
        };
        let Some(actual) = actual_facts.checked_mul(size_of::<CodexLineageFactV0>()) else {
            self.discard_and_seal_conservatively(bytes);
            return Err(lineage_accounting_invariant());
        };
        if actual > bytes {
            if let Err(error) = self.budget.charge(actual - bytes) {
                self.discard_and_seal_conservatively(bytes);
                if is_lineage_capacity_exhaustion(&error) {
                    return Ok(false);
                }
                return Err(error);
            }
        } else {
            self.budget.release(bytes - actual);
        }
        let Some(charged) = self.charged.checked_add(actual) else {
            self.discard_and_seal_conservatively(actual);
            return Err(lineage_accounting_invariant());
        };
        self.charged = charged;
        Ok(true)
    }

    fn discard_and_seal_conservatively(&mut self, pending_bytes: usize) {
        drop(std::mem::take(&mut self.facts));
        self.budget.release(pending_bytes);
        self.budget.release(self.charged);
        self.budget.release_facts(self.charged_facts);
        self.has_unattributed_ambiguity = false;
        self.sealed = true;
        self.conservative = true;
        self.charged = 0;
        self.charged_facts = 0;
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

fn is_lineage_capacity_exhaustion(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::InvalidPayload(detail) if detail == CODEX_LINEAGE_EXHAUSTED_SENTINEL
    )
}

fn lineage_accounting_invariant() -> CaptureError {
    CaptureError::SystemInvariant("Codex lineage fact accounting overflowed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::codex::nativepath::tests::{discover_one, session_meta, write_source};

    fn assert_conservative(facts: &CodexLineageFactsV0) {
        assert!(facts.sealed);
        assert!(facts.conservative);
        assert!(facts.facts.is_empty());
        assert_eq!(facts.facts.capacity(), 0);
        assert_eq!(facts.charged, 0);
        assert_eq!(facts.charged_facts, 0);
    }

    #[test]
    fn byte_limit_exhaustion_discards_and_degrades_nonfatally() {
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(
            LINEAGE_CONTAINER_CHARGE,
            64,
        ));
        let mut facts = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
        facts
            .record(CodexLineageRecordEvidence::Call("bounded-call"))
            .unwrap();

        assert_conservative(&facts);
        assert_eq!(
            facts.presence("bounded-call", "bounded-call"),
            CodexLineageFactPresenceV0::Unproven
        );
        assert_eq!(budget.charged.load(Ordering::Acquire), 0);
        assert_eq!(budget.facts.load(Ordering::Acquire), 0);
    }

    #[test]
    fn allocator_reservation_exhaustion_remains_retryable() {
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(
            usize::MAX,
            usize::MAX,
        ));
        let mut facts = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
        facts
            .record(CodexLineageRecordEvidence::Call("retained-0"))
            .unwrap();
        let retained_capacity = facts.facts.capacity();
        for index in 1..retained_capacity {
            facts
                .record(CodexLineageRecordEvidence::Call(&format!(
                    "retained-{index}"
                )))
                .unwrap();
        }
        assert_eq!(facts.facts.len(), facts.facts.capacity());
        let requested = (isize::MAX as usize / size_of::<CodexLineageFactV0>()) + 1;

        assert!(matches!(
            facts.reserve_more(requested),
            Err(CaptureError::InvalidPayload(detail))
                if detail == CODEX_LINEAGE_EXHAUSTED_SENTINEL
        ));
        assert!(!facts.conservative);
        assert_eq!(facts.facts.len(), retained_capacity);
        assert_eq!(budget.facts.load(Ordering::Acquire), retained_capacity);
    }

    #[test]
    fn non_capacity_reservation_invariant_remains_an_error() {
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(1024 * 1024, 64));
        let mut facts = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
        facts
            .record(CodexLineageRecordEvidence::Call("retained"))
            .unwrap();

        assert!(matches!(
            facts.reserve_more(1),
            Err(CaptureError::SystemInvariant(
                "Codex lineage fact accounting overflowed"
            ))
        ));
        assert!(!facts.conservative);
        assert_eq!(facts.facts.len(), 1);
        assert_eq!(budget.facts.load(Ordering::Acquire), 1);
    }

    #[test]
    fn constructor_container_exhaustion_returns_an_uncharged_conservative_set() {
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(
            LINEAGE_CONTAINER_CHARGE,
            1,
        ));
        let retained = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
        let mut conservative = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();

        assert_conservative(&conservative);
        conservative
            .record(CodexLineageRecordEvidence::Call("ignored"))
            .unwrap();
        assert_eq!(
            conservative.presence("ignored", "missing"),
            CodexLineageFactPresenceV0::Unproven
        );
        assert_eq!(
            budget.charged.load(Ordering::Acquire),
            LINEAGE_CONTAINER_CHARGE
        );
        drop(conservative);
        assert_eq!(
            budget.charged.load(Ordering::Acquire),
            LINEAGE_CONTAINER_CHARGE
        );
        drop(retained);
        assert_eq!(budget.charged.load(Ordering::Acquire), 0);
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
    fn fact_limit_exhaustion_discards_only_the_exhausted_set() {
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(1024 * 1024, 1));
        let first = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
        let mut second = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
        second
            .record(CodexLineageRecordEvidence::Call("retained"))
            .unwrap();
        assert_eq!(budget.facts.load(Ordering::Acquire), 1);

        second
            .record(CodexLineageRecordEvidence::Result("exhausted"))
            .unwrap();
        assert_conservative(&second);
        assert_eq!(budget.facts.load(Ordering::Acquire), 0);
        assert_eq!(
            budget.charged.load(Ordering::Acquire),
            LINEAGE_CONTAINER_CHARGE
        );
        drop(second);
        assert_eq!(
            budget.charged.load(Ordering::Acquire),
            LINEAGE_CONTAINER_CHARGE
        );
        drop(first);
        assert_eq!(budget.charged.load(Ordering::Acquire), 0);
    }

    #[test]
    fn conservative_drop_releases_each_charge_exactly_once() {
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(1024 * 1024, 1));
        let survivor = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
        let mut degraded = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
        degraded
            .record(CodexLineageRecordEvidence::Call("retained"))
            .unwrap();
        degraded
            .record(CodexLineageRecordEvidence::Result("exhausted"))
            .unwrap();

        assert_conservative(&degraded);
        assert_eq!(budget.facts.load(Ordering::Acquire), 0);
        assert_eq!(
            budget.charged.load(Ordering::Acquire),
            LINEAGE_CONTAINER_CHARGE
        );
        drop(degraded);
        assert_eq!(budget.facts.load(Ordering::Acquire), 0);
        assert_eq!(
            budget.charged.load(Ordering::Acquire),
            LINEAGE_CONTAINER_CHARGE
        );
        drop(survivor);
        assert_eq!(budget.facts.load(Ordering::Acquire), 0);
        assert_eq!(budget.charged.load(Ordering::Acquire), 0);
    }

    #[test]
    fn conservative_presence_is_deterministically_unproven_and_records_are_noops() {
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(1024 * 1024, 1));
        let mut facts = CodexLineageFactsV0::new(Arc::clone(&budget)).unwrap();
        facts
            .record(CodexLineageRecordEvidence::Call("retained"))
            .unwrap();
        facts
            .record(CodexLineageRecordEvidence::Result("exhausted"))
            .unwrap();
        let mark = facts.mark();
        let digests = [codex_lineage_call_id_digest("ignored-digest")];

        for (origin, result) in [
            ("retained", "retained"),
            ("missing", "missing"),
            ("", "missing"),
            ("missing", ""),
        ] {
            assert_eq!(
                facts.presence(origin, result),
                CodexLineageFactPresenceV0::Unproven
            );
        }
        facts.record(CodexLineageRecordEvidence::None).unwrap();
        facts
            .record(CodexLineageRecordEvidence::UnattributedAmbiguity)
            .unwrap();
        facts
            .record(CodexLineageRecordEvidence::Call("ignored-call"))
            .unwrap();
        facts
            .record(CodexLineageRecordEvidence::Result("ignored-result"))
            .unwrap();
        facts
            .record(CodexLineageRecordEvidence::Ambiguous("ignored-ambiguous"))
            .unwrap();
        facts
            .record(CodexLineageRecordEvidence::AmbiguousDigests(&digests))
            .unwrap();
        facts.restore(mark);
        facts.seal();

        assert_conservative(&facts);
        assert_eq!(
            facts.presence("ignored-call", "ignored-result"),
            CodexLineageFactPresenceV0::Unproven
        );
        assert_eq!(budget.charged.load(Ordering::Acquire), 0);
        assert_eq!(budget.facts.load(Ordering::Acquire), 0);
    }

    #[test]
    fn thousands_of_small_fact_sets_are_charged_by_live_facts() {
        const SETS: usize = 6_073;
        let budget = Arc::new(CodexLineageFactBudgetV0::with_limits(
            MAX_LINEAGE_FACT_BYTES_PER_COMPONENT,
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
        assert!(budget.charged.load(Ordering::Acquire) < MAX_LINEAGE_FACT_BYTES_PER_COMPONENT);
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
