use super::*;

impl NativePathPublicationGroup<'_> {
    /// Reads and classifies every required cursor row inside this transaction.
    /// Duplicate, empty, missing, extra, mixed, malformed, or stale sets fail
    /// closed and poison the group.
    pub fn classify_cursor_set(
        &mut self,
        publication_id: &str,
        transitions: &[NativePathCursorTransition],
    ) -> Result<NativePathCursorSetClassification> {
        self.ensure_mutable()?;
        if publication_id.is_empty()
            || transitions.is_empty()
            || transitions.len() != self.coordinator.source_count()
            || self.attempted_mutation_units != 0
            || !matches!(self.cursor_state, CursorPublicationState::None)
        {
            return self.poison_with(StoreError::InvalidNativePathCursorSet);
        }
        let unique = transitions
            .iter()
            .map(|transition| transition.key.clone())
            .collect::<BTreeSet<_>>();
        if unique.len() != transitions.len() {
            return self.poison_with(StoreError::InvalidNativePathCursorSet);
        }

        let rows = match transitions
            .iter()
            .map(|transition| {
                self.store.get_sync_cursor(
                    transition.key.team_id(),
                    transition.key.device_id(),
                    transition.key.stream(),
                )
            })
            .collect::<Result<Vec<_>>>()
        {
            Ok(rows) => rows,
            Err(error) => return self.poison_with(error),
        };

        let all_expected = rows.iter().zip(transitions).all(|(row, transition)| {
            row.as_ref().map(|cursor| cursor.cursor.as_str())
                == transition.expected_cursor.as_deref()
        });

        let mut common_checkpoint: Option<Option<JournalCheckpoint>> = None;
        let mut all_next = true;
        for (row, transition) in rows.iter().zip(transitions) {
            let Some(row) = row else {
                all_next = false;
                break;
            };
            let Ok(envelope) = decode_cursor_envelope(&row.cursor) else {
                all_next = false;
                break;
            };
            let canonical = match encode_cursor_envelope(&envelope) {
                Ok(canonical) => canonical,
                Err(error) => return self.poison_with(error),
            };
            if canonical != row.cursor
                || envelope.publication_id != publication_id
                || envelope.provider_cursor != transition.next.cursor
            {
                all_next = false;
                break;
            }
            match &common_checkpoint {
                Some(checkpoint) if checkpoint != &envelope.journal_checkpoint => {
                    all_next = false;
                    break;
                }
                None => common_checkpoint = Some(envelope.journal_checkpoint.clone()),
                Some(_) => {}
            }
        }

        if all_expected == all_next {
            return self.poison_with(StoreError::NativePathCursorConflict);
        }
        if all_expected {
            self.cursor_state = CursorPublicationState::Expected {
                publication_id: publication_id.to_owned(),
                transitions: transitions.to_vec(),
                rows,
            };
            return Ok(NativePathCursorSetClassification::AllExpected);
        }

        let checkpoint = common_checkpoint.unwrap_or(None);
        match self
            .store
            .verify_projection_journal_checkpoint_in_transaction(checkpoint.as_ref())
        {
            Ok(true) => {}
            Ok(false) => return self.poison_with(StoreError::NativePathCursorConflict),
            Err(error) => return self.poison_with(error),
        }
        self.checkpoint.clone_from(&checkpoint);
        self.journal_prepared = true;
        let cursors = rows
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(StoreError::NativePathCursorConflict)?;
        self.cursor_state = CursorPublicationState::AlreadyCommitted { cursors };
        Ok(NativePathCursorSetClassification::AllNextSameGroup { checkpoint })
    }

    /// Flushes the group collector in bounded chunks and returns the exact
    /// checkpoint from this still-open SQLite transaction.
    pub fn prepare_journal_checkpoint(&mut self) -> Result<Option<JournalCheckpoint>> {
        self.ensure_open()?;
        if self.is_poisoned() {
            return Err(StoreError::NativePathGroupPoisoned);
        }
        if self.journal_prepared {
            return Ok(self.checkpoint.clone());
        }
        let flush_result = self.with_write_scope(|store| {
            let mut collector = store.projection_journal_group_collector.borrow_mut();
            let collector = collector
                .as_mut()
                .ok_or(StoreError::NativePathGroupPoisoned)?;
            let (records, bytes) = collector.seal_and_flush(&store.conn)?;
            let checkpoint = store.projection_journal_checkpoint_in_transaction()?;
            Ok((records, bytes, checkpoint))
        });
        let (records, bytes, checkpoint) = match flush_result {
            Ok(value) => value,
            Err(error) => return self.poison_with(error),
        };
        if let Err(error) = validate_limit(
            "actual journal records",
            records,
            NATIVE_PATH_MAX_JOURNAL_RECORDS,
        ) {
            return self.poison_with(error);
        }
        if let Err(error) = validate_limit(
            "uncompressed journal encoding bytes",
            bytes,
            NATIVE_PATH_MAX_JOURNAL_BYTES,
        ) {
            return self.poison_with(error);
        }
        self.journal_records = records;
        self.journal_uncompressed_bytes = bytes;
        self.checkpoint = checkpoint;
        self.journal_prepared = true;
        Ok(self.checkpoint.clone())
    }

    /// Publishes the previously classified all-expected cursor set using its
    /// exact freshly read rows. The Store embeds one common exact checkpoint.
    pub fn publish_cursor_set(&mut self) -> Result<()> {
        self.ensure_open()?;
        if self.is_poisoned() {
            return Err(StoreError::NativePathGroupPoisoned);
        }
        if !self.journal_prepared {
            return self.poison_with(StoreError::InvalidNativePathCursorSet);
        }

        let state = std::mem::replace(&mut self.cursor_state, CursorPublicationState::None);
        let CursorPublicationState::Expected {
            publication_id,
            transitions,
            rows,
        } = state
        else {
            return self.poison_with(StoreError::InvalidNativePathCursorSet);
        };

        let mut next = Vec::with_capacity(transitions.len());
        let mut encoded_bytes = 0_usize;
        for (transition, current) in transitions.iter().zip(&rows) {
            if !transition.key.matches(&transition.next) {
                return self.poison_with(StoreError::InvalidNativePathCursorSet);
            }
            let envelope = NativePathCommittedCursorEnvelope {
                version: NATIVE_PATH_CURSOR_ENVELOPE_VERSION,
                publication_id: publication_id.clone(),
                provider_cursor: transition.next.cursor.clone(),
                journal_checkpoint: self.checkpoint.clone(),
            };
            let mut cursor = transition.next.clone();
            cursor.cursor = match encode_cursor_envelope(&envelope) {
                Ok(encoded) => encoded,
                Err(error) => return self.poison_with(error),
            };
            encoded_bytes =
                encoded_bytes.saturating_add(encoded_cursor_cas_bytes(current.as_ref(), &cursor));
            next.push(cursor);
        }
        self.charge_core_mutations(next.len(), encoded_bytes)?;
        let result = self.with_write_scope(|store| {
            for ((current, transition), next) in rows.iter().zip(&transitions).zip(&next) {
                if !transition.key.matches(next)
                    || !store.compare_and_set_sync_cursor(current.as_ref(), next)?
                {
                    return Err(StoreError::NativePathCursorConflict);
                }
            }
            transitions
                .iter()
                .zip(&next)
                .map(|(transition, proposed)| {
                    let committed = store
                        .get_sync_cursor(
                            transition.key.team_id(),
                            transition.key.device_id(),
                            transition.key.stream(),
                        )?
                        .ok_or(StoreError::NativePathCursorConflict)?;
                    if committed.cursor != proposed.cursor {
                        return Err(StoreError::NativePathCursorConflict);
                    }
                    Ok(committed)
                })
                .collect::<Result<Vec<_>>>()
        });
        match result {
            Ok(cursors) => {
                self.cursor_state = CursorPublicationState::Published { cursors };
                Ok(())
            }
            Err(error) => self.poison_with(error),
        }
    }
}
