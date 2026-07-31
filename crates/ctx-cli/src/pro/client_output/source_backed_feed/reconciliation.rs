#[cfg(test)]
use std::cell::Cell;
use std::cmp::Ordering;

use anyhow::{anyhow, Result};
use ctx_pro_host_protocol::SourceRemoval;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StaleRemovalReconciliationWork {
    pub(crate) digest_comparisons: u64,
}

#[cfg(test)]
thread_local! {
    static STALE_REMOVAL_RECONCILIATION_WORK: Cell<StaleRemovalReconciliationWork> =
        const { Cell::new(StaleRemovalReconciliationWork { digest_comparisons: 0 }) };
}

pub(super) fn match_stale_source_removals<'a>(
    stale_source_ids: &[[u8; 32]],
    removals: &'a [SourceRemoval],
) -> Result<Vec<([u8; 32], &'a SourceRemoval)>> {
    #[cfg(test)]
    STALE_REMOVAL_RECONCILIATION_WORK.set(StaleRemovalReconciliationWork::default());

    let mut matches = Vec::with_capacity(stale_source_ids.len());
    let mut removal_index = 0;
    // BTreeMap progress keys and validated manifest removals are both strictly
    // ordered by this digest, so one forward-only merge covers every match.
    for source_id in stale_source_ids {
        loop {
            let removal = removals.get(removal_index).ok_or_else(missing_removal)?;
            let removal_source_id = removal.deletion.source().identity().digest();
            #[cfg(test)]
            STALE_REMOVAL_RECONCILIATION_WORK.set(
                STALE_REMOVAL_RECONCILIATION_WORK
                    .get()
                    .increment_digest_comparisons(),
            );
            match removal_source_id.cmp(source_id) {
                Ordering::Less => {
                    removal_index = removal_index.saturating_add(1);
                }
                Ordering::Equal => {
                    matches.push((*source_id, removal));
                    removal_index = removal_index.saturating_add(1);
                    break;
                }
                Ordering::Greater => return Err(missing_removal()),
            }
        }
    }
    Ok(matches)
}

fn missing_removal() -> anyhow::Error {
    anyhow!("source_changed: Pro source is absent from the manifest without a certified deletion")
}

#[cfg(test)]
impl StaleRemovalReconciliationWork {
    const fn increment_digest_comparisons(mut self) -> Self {
        self.digest_comparisons = self.digest_comparisons.saturating_add(1);
        self
    }
}

#[cfg(test)]
pub(crate) fn stale_removal_reconciliation_work_for_test() -> StaleRemovalReconciliationWork {
    STALE_REMOVAL_RECONCILIATION_WORK.get()
}
