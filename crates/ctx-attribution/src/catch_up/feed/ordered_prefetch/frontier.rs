use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OversizeReplacement {
    Acquired,
    RetryAfterRelease,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super::super) enum EncodedCreditResize {
    Ready,
    RetryOrderedOversize,
    Cancelled,
}

impl EncodedPageCredits {
    pub(in super::super::super) fn advance_ordered_demand(
        &self,
        source_ordinal: usize,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        if state
            .ordered_demand
            .is_some_and(|ordered_demand| source_ordinal < ordered_demand)
        {
            bail!("internal: Core prefetch ordered demand moved backwards");
        }
        state.ordered_demand = Some(source_ordinal);
        drop(state);
        self.available.notify_all();
        Ok(())
    }
}

impl EncodedPageCredit {
    pub(in super::super::super) fn resize_to(
        &mut self,
        bytes: usize,
        prefetch_position: Option<(usize, &CurrentPrefetchControls)>,
    ) -> Result<EncodedCreditResize> {
        if bytes > self.bytes {
            if bytes > CORE_PREFETCH_PAGE_ENCODED_BYTE_BUDGET && prefetch_position.is_some() {
                let prior_bytes = self.bytes;
                let (source_ordinal, controls) = prefetch_position
                    .map(|(ordinal, controls)| (Some(ordinal), Some(controls)))
                    .unwrap_or((None, None));
                return match self.owner.replace_with_ordered_oversize(
                    prior_bytes,
                    bytes,
                    source_ordinal,
                    controls,
                )? {
                    OversizeReplacement::Acquired => {
                        self.bytes = bytes;
                        Ok(EncodedCreditResize::Ready)
                    }
                    OversizeReplacement::RetryAfterRelease => {
                        Ok(EncodedCreditResize::RetryOrderedOversize)
                    }
                    OversizeReplacement::Cancelled => Ok(EncodedCreditResize::Cancelled),
                };
            }
            let additional = bytes - self.bytes;
            let (source_ordinal, controls) = prefetch_position
                .map(|(ordinal, controls)| (Some(ordinal), Some(controls)))
                .unwrap_or((None, None));
            if !self
                .owner
                .grow_ordered(additional, bytes, source_ordinal, controls)?
            {
                return Ok(EncodedCreditResize::Cancelled);
            }
            self.bytes = bytes;
            return Ok(EncodedCreditResize::Ready);
        }
        let released = self.bytes - bytes;
        self.bytes = bytes;
        self.owner.release(released);
        Ok(EncodedCreditResize::Ready)
    }
}

impl Drop for EncodedPageCredit {
    fn drop(&mut self) {
        self.owner.release(self.bytes);
    }
}
