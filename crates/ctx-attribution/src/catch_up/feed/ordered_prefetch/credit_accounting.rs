use super::*;

#[path = "frontier.rs"]
mod frontier;
pub(in super::super) use frontier::EncodedCreditResize;
use frontier::OversizeReplacement;

#[derive(Default)]
struct EncodedPageCreditState {
    in_use: usize,
    high_water: usize,
    cancelled: bool,
    // Oversized speculative pages may consume the complete credit budget.
    // Keep them behind the source currently demanded by the ordered consumer
    // so a later source cannot block an earlier source's successor page.
    ordered_demand: Option<usize>,
    // Ordered oversized requests stop later speculative pages from occupying
    // credits that the coordinator cannot release before reaching the request.
    oversize_waiters: BTreeSet<usize>,
}

pub(in super::super) struct EncodedPageCredits {
    capacity: usize,
    state: Mutex<EncodedPageCreditState>,
    available: Condvar,
}

impl EncodedPageCredits {
    pub(in super::super) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(EncodedPageCreditState::default()),
            available: Condvar::new(),
        }
    }

    pub(in super::super) fn acquire(
        self: &Arc<Self>,
        bytes: usize,
    ) -> Result<Option<EncodedPageCredit>> {
        self.acquire_ordered(bytes, None, None)
    }

    pub(in super::super) fn acquire_prefetched(
        self: &Arc<Self>,
        bytes: usize,
        source_ordinal: usize,
        controls: &CurrentPrefetchControls,
    ) -> Result<Option<EncodedPageCredit>> {
        self.acquire_ordered(bytes, Some(source_ordinal), Some(controls))
    }

    fn acquire_ordered(
        self: &Arc<Self>,
        bytes: usize,
        source_ordinal: Option<usize>,
        controls: Option<&CurrentPrefetchControls>,
    ) -> Result<Option<EncodedPageCredit>> {
        if bytes > self.capacity {
            bail!(
                "invalid_request: planned Core page requires {bytes} encoded bytes, exceeding the {}-byte prefetch budget",
                self.capacity
            );
        }
        let oversize_source =
            source_ordinal.filter(|_| bytes > CORE_PREFETCH_PAGE_ENCODED_BYTE_BUDGET);
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        if let Some(source_ordinal) = oversize_source {
            state.oversize_waiters.insert(source_ordinal);
            drop(state);
            // Later workers retain their prepared pages until ordered demand,
            // so they can discard that speculation and release exact credits.
            if let Some(controls) = controls {
                controls.yield_later_sources(source_ordinal);
            }
            state = self
                .state
                .lock()
                .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        }
        while !state.cancelled {
            let first_oversize_waiter = state.oversize_waiters.first().copied();
            let waits_for_ordered_demand = oversize_source.is_some_and(|source_ordinal| {
                state
                    .ordered_demand
                    .is_none_or(|ordered_demand| source_ordinal > ordered_demand)
            });
            let waits_for_earlier_oversize = match (source_ordinal, oversize_source) {
                (Some(source_ordinal), Some(_)) => first_oversize_waiter != Some(source_ordinal),
                (Some(source_ordinal), None) => {
                    first_oversize_waiter.is_some_and(|waiter| waiter < source_ordinal)
                }
                (None, _) => false,
            };
            let exceeds_capacity = state
                .in_use
                .checked_add(bytes)
                .is_none_or(|total| total > self.capacity);
            if !waits_for_ordered_demand && !waits_for_earlier_oversize && !exceeds_capacity {
                break;
            }
            state = self
                .available
                .wait(state)
                .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        }
        if state.cancelled {
            if let Some(source_ordinal) = oversize_source {
                state.oversize_waiters.remove(&source_ordinal);
            }
            return Ok(None);
        }
        if let Some(source_ordinal) = oversize_source {
            state.oversize_waiters.remove(&source_ordinal);
        }
        state.in_use = state
            .in_use
            .checked_add(bytes)
            .ok_or_else(|| anyhow!("internal: Core prefetch credit overflowed"))?;
        state.high_water = state.high_water.max(state.in_use);
        Ok(Some(EncodedPageCredit {
            owner: Arc::clone(self),
            bytes,
        }))
    }

    pub(in super::super) fn should_yield_to_oversize(&self, source_ordinal: usize) -> Result<bool> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        if state.cancelled {
            bail!("internal: Core prefetch was cancelled");
        }
        Ok(state
            .oversize_waiters
            .first()
            .is_some_and(|waiter| *waiter < source_ordinal))
    }

    fn grow_ordered(
        &self,
        additional_bytes: usize,
        target_bytes: usize,
        source_ordinal: Option<usize>,
        controls: Option<&CurrentPrefetchControls>,
    ) -> Result<bool> {
        if target_bytes > self.capacity {
            bail!(
                "invalid_request: Core page requires {target_bytes} encoded bytes, exceeding the {}-byte prefetch budget",
                self.capacity
            );
        }
        let oversize_source =
            source_ordinal.filter(|_| target_bytes > CORE_PREFETCH_PAGE_ENCODED_BYTE_BUDGET);
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        if let Some(source_ordinal) = oversize_source {
            state.oversize_waiters.insert(source_ordinal);
            drop(state);
            if let Some(controls) = controls {
                controls.yield_later_sources(source_ordinal);
            }
            state = self
                .state
                .lock()
                .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        }
        while !state.cancelled {
            let waits_for_ordered_demand = oversize_source.is_some_and(|source_ordinal| {
                state
                    .ordered_demand
                    .is_none_or(|ordered_demand| source_ordinal > ordered_demand)
            });
            let first_oversize_waiter = state.oversize_waiters.first().copied();
            let waits_for_earlier_oversize = source_ordinal.is_some_and(|source_ordinal| {
                first_oversize_waiter.is_some_and(|waiter| waiter < source_ordinal)
            });
            let exceeds_capacity = state
                .in_use
                .checked_add(additional_bytes)
                .is_none_or(|total| total > self.capacity);
            if !waits_for_ordered_demand && !waits_for_earlier_oversize && !exceeds_capacity {
                break;
            }
            state = self
                .available
                .wait(state)
                .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        }
        if state.cancelled {
            if let Some(source_ordinal) = oversize_source {
                state.oversize_waiters.remove(&source_ordinal);
            }
            return Ok(false);
        }
        if let Some(source_ordinal) = oversize_source {
            state.oversize_waiters.remove(&source_ordinal);
        }
        state.in_use = state
            .in_use
            .checked_add(additional_bytes)
            .ok_or_else(|| anyhow!("internal: Core prefetch credit overflowed"))?;
        state.high_water = state.high_water.max(state.in_use);
        drop(state);
        self.available.notify_all();
        Ok(true)
    }

    fn replace_with_ordered_oversize(
        &self,
        prior_bytes: usize,
        target_bytes: usize,
        source_ordinal: Option<usize>,
        controls: Option<&CurrentPrefetchControls>,
    ) -> Result<OversizeReplacement> {
        if target_bytes > self.capacity {
            bail!(
                "invalid_request: Core page requires {target_bytes} encoded bytes, exceeding the {}-byte prefetch budget",
                self.capacity
            );
        }
        let oversize_source = source_ordinal.ok_or_else(|| {
            anyhow!("internal: oversized prefetched Core page omitted its source order")
        })?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        state.oversize_waiters.insert(oversize_source);
        drop(state);
        self.available.notify_all();
        if let Some(controls) = controls {
            controls.yield_later_sources(oversize_source);
        }
        state = self
            .state
            .lock()
            .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        if state.cancelled {
            state.oversize_waiters.remove(&oversize_source);
            return Ok(OversizeReplacement::Cancelled);
        }
        let waits_for_ordered_demand = state
            .ordered_demand
            .is_none_or(|ordered_demand| oversize_source > ordered_demand);
        let waits_for_earlier_oversize = state
            .oversize_waiters
            .first()
            .is_some_and(|waiter| *waiter < oversize_source);
        let replaced_total = state
            .in_use
            .checked_sub(prior_bytes)
            .and_then(|remaining| remaining.checked_add(target_bytes))
            .ok_or_else(|| {
                anyhow!("internal: Core prefetch replacement released unowned credits")
            })?;
        if waits_for_ordered_demand || waits_for_earlier_oversize || replaced_total > self.capacity
        {
            return Ok(OversizeReplacement::RetryAfterRelease);
        }
        state.oversize_waiters.remove(&oversize_source);
        state.in_use = replaced_total;
        state.high_water = state.high_water.max(state.in_use);
        drop(state);
        self.available.notify_all();
        Ok(OversizeReplacement::Acquired)
    }

    pub(in super::super) fn cancel(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.cancelled = true;
            state.oversize_waiters.clear();
        }
        self.available.notify_all();
    }

    pub(in super::super) fn snapshot(&self) -> Result<(usize, usize)> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        Ok((state.in_use, state.high_water))
    }

    fn release(&self, bytes: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.in_use = state.in_use.saturating_sub(bytes);
        }
        self.available.notify_all();
    }
}

pub(in super::super) struct EncodedPageCredit {
    owner: Arc<EncodedPageCredits>,
    bytes: usize,
}
