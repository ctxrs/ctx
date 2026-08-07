use super::*;

#[derive(Debug, Clone, Copy)]
struct GenerationComponentCacheStateV0 {
    remaining_owners: usize,
    leases: usize,
    loaded: bool,
    last_used: u64,
}

#[derive(Debug)]
pub(super) struct GenerationLineageCacheV0 {
    components: Vec<GenerationComponentCacheStateV0>,
    clock: u64,
    peak_loaded_components: usize,
    component_loads: usize,
}

impl CodexOutcomeLineageAuthorityV0 {
    pub(in super::super) fn initialize_generation_spill(
        &mut self,
        owner_counts: &HashMap<u64, usize>,
    ) -> CodexSourceBackedResultV0<()> {
        let mut components = Vec::with_capacity(self.component_members.len());
        for component in 0..self.component_members.len() {
            let component = u64::try_from(component)
                .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            let remaining_owners = owner_counts.get(&component).copied().unwrap_or(0);
            components.push(GenerationComponentCacheStateV0 {
                remaining_owners,
                leases: 0,
                loaded: false,
                last_used: 0,
            });
        }
        self.generation_spill = Some(Mutex::new(tempfile::tempfile()?));
        self.generation_cache = Some(Mutex::new(GenerationLineageCacheV0 {
            components,
            clock: 0,
            peak_loaded_components: 0,
            component_loads: 0,
        }));
        Ok(())
    }

    pub(in super::super) fn spill_generation_component(
        &mut self,
        component: u64,
    ) -> CodexSourceBackedResultV0<()> {
        let component = usize::try_from(component)
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let members = self
            .component_members
            .get(component)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let spill = self
            .generation_spill
            .as_ref()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let mut spill = spill
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let facts = self
            .facts
            .get_mut()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        for index in members {
            let state = facts
                .get_mut(*index)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            match std::mem::replace(state, LineageFactsStateV0::Pending) {
                LineageFactsStateV0::Ready(mut lineage_facts) => {
                    let record = lineage_facts
                        .spill_to(&mut spill)
                        .map_err(map_lineage_capture_error)?;
                    *self
                        .generation_spill_entries
                        .get_mut(*index)
                        .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)? =
                        Some(record);
                }
                LineageFactsStateV0::CompleteLeaf => {
                    *state = LineageFactsStateV0::CompleteLeaf;
                }
                LineageFactsStateV0::OutsideRoute => {
                    *state = LineageFactsStateV0::OutsideRoute;
                }
                LineageFactsStateV0::Pending | LineageFactsStateV0::Released => {
                    return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
                }
            }
        }
        Ok(())
    }

    pub(in super::super) fn generation_component_has_spilled_facts(
        &self,
        component: u64,
    ) -> CodexSourceBackedResultV0<bool> {
        let component = usize::try_from(component)
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let members = self
            .component_members
            .get(component)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        Ok(members.iter().any(|index| {
            self.generation_spill_entries
                .get(*index)
                .is_some_and(Option::is_some)
        }))
    }

    pub(in super::super) fn lease_generation_component(
        &self,
        component: u64,
    ) -> CodexSourceBackedResultV0<()> {
        let component = usize::try_from(component)
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let cache = self
            .generation_cache
            .as_ref()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let mut cache = cache
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let current = cache
            .components
            .get(component)
            .copied()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        if current.remaining_owners == 0 {
            return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
        }
        cache.clock = cache.clock.saturating_add(1);
        let clock = cache.clock;
        if current.loaded {
            let state = cache
                .components
                .get_mut(component)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            state.leases = state
                .leases
                .checked_add(1)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
            state.last_used = clock;
            return Ok(());
        }

        let loaded = cache.components.iter().filter(|state| state.loaded).count();
        if loaded == CODEX_GENERATION_LINEAGE_COMPONENTS_PER_WAVE {
            let victim = cache
                .components
                .iter()
                .enumerate()
                .filter(|(_, state)| state.loaded && state.leases == 0)
                .min_by_key(|(index, state)| (state.last_used, *index))
                .map(|(index, _)| index)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            self.deactivate_generation_component_facts(victim)?;
            cache.components[victim].loaded = false;
        } else if loaded > CODEX_GENERATION_LINEAGE_COMPONENTS_PER_WAVE {
            return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
        }

        self.restore_generation_component_facts(component)?;
        cache.component_loads = cache.component_loads.saturating_add(1);
        let state = cache
            .components
            .get_mut(component)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        state.loaded = true;
        state.leases = 1;
        state.last_used = clock;
        let loaded = cache.components.iter().filter(|state| state.loaded).count();
        cache.peak_loaded_components = cache.peak_loaded_components.max(loaded);
        Ok(())
    }

    pub(in super::super) fn release_generation_component(
        &self,
        component: u64,
    ) -> CodexSourceBackedResultV0<()> {
        let component = usize::try_from(component)
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let cache = self
            .generation_cache
            .as_ref()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let mut cache = cache
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        cache.clock = cache.clock.saturating_add(1);
        let clock = cache.clock;
        let state = cache
            .components
            .get_mut(component)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        if !state.loaded || state.leases == 0 || state.remaining_owners == 0 {
            return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable);
        }
        state.leases = state.leases.saturating_sub(1);
        state.remaining_owners = state.remaining_owners.saturating_sub(1);
        state.last_used = clock;
        if state.leases == 0 && state.remaining_owners == 0 {
            self.deactivate_generation_component_facts(component)?;
            state.loaded = false;
        }
        Ok(())
    }

    fn restore_generation_component_facts(
        &self,
        component: usize,
    ) -> CodexSourceBackedResultV0<()> {
        let members = self
            .component_members
            .get(component)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let spill = self
            .generation_spill
            .as_ref()
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let mut spill = spill
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let mut facts = self
            .facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let mut restored = Vec::new();
        for index in members {
            match facts.get(*index) {
                Some(LineageFactsStateV0::CompleteLeaf) => continue,
                Some(LineageFactsStateV0::Pending) => {}
                Some(LineageFactsStateV0::OutsideRoute)
                | Some(LineageFactsStateV0::Ready(_))
                | Some(LineageFactsStateV0::Released)
                | None => return Err(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable),
            }
            let record = self
                .generation_spill_entries
                .get(*index)
                .and_then(|entry| *entry)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            let budget = self
                .component_budgets
                .get(component)
                .cloned()
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            let lineage_facts = CodexLineageFactsV0::restore_from(&mut spill, record, budget)
                .map_err(map_lineage_capture_error)?;
            restored.push((*index, lineage_facts));
        }
        for (index, lineage_facts) in restored {
            *facts
                .get_mut(index)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)? =
                LineageFactsStateV0::Ready(lineage_facts);
        }
        Ok(())
    }

    fn deactivate_generation_component_facts(
        &self,
        component: usize,
    ) -> CodexSourceBackedResultV0<()> {
        let members = self
            .component_members
            .get(component)
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        let mut facts = self
            .facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        for index in members {
            let state = facts
                .get_mut(*index)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
            if matches!(state, LineageFactsStateV0::Ready(_)) {
                *state = LineageFactsStateV0::Pending;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(in super::super) fn set_generation_component_budget_limits(
        &mut self,
        byte_limit: usize,
        fact_limit: usize,
    ) {
        self.component_budgets = (0..self.component_members.len())
            .map(|_| {
                Arc::new(CodexLineageFactBudgetV0::with_limits(
                    byte_limit, fact_limit,
                ))
            })
            .collect();
    }

    #[cfg(test)]
    pub(in super::super) fn generation_component_metrics(
        &self,
    ) -> (usize, usize, usize, usize, usize) {
        let (active, peak, component_loads) = self
            .generation_cache
            .as_ref()
            .and_then(|cache| cache.lock().ok())
            .map(|cache| {
                (
                    cache.components.iter().filter(|state| state.loaded).count(),
                    cache.peak_loaded_components,
                    cache.component_loads,
                )
            })
            .unwrap_or((0, 0, 0));
        let max_current_bytes = self
            .component_budgets
            .iter()
            .map(|budget| budget.charges_for_test().0)
            .max()
            .unwrap_or(0);
        let max_peak_bytes = self
            .component_budgets
            .iter()
            .map(|budget| budget.peak_charge_for_test())
            .max()
            .unwrap_or(0);
        (
            active,
            peak,
            max_current_bytes,
            max_peak_bytes,
            component_loads,
        )
    }
}
