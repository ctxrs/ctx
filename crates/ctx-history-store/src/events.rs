use ctx_history_core::{compute_payload_hash, CaptureProvider, Event, EventRole, EventType};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, nonnegative_i64_to_u64, optional_timestamp_ms, optional_uuid_string,
    parse_json, parse_optional_uuid, parse_text_enum, parse_uuid, timestamp_ms,
};
use crate::native_path_group::event_bind_bytes;
use crate::result_storage::{durable_event, provider_output_is_retained_failure};
use crate::search::projections::{
    adjust_semantic_searchable_item_stats, insert_event_search_projection_for_event,
    semantic_searchable_document_count_for_event,
    semantic_searchable_document_count_from_stored_event, upsert_event_search_projection_for_event,
};
use crate::sync::sync_metadata_from_row;
use crate::{Result, Store, StoreError};

const PROVIDER_EVENT_HASH_AUTHORITY_KEY: &str = "provider_event_hash_authority";

#[derive(Default)]
struct NativePathEventBindAccounting {
    enabled: bool,
    bytes: usize,
}

impl NativePathEventBindAccounting {
    fn enabled() -> Self {
        Self {
            enabled: true,
            bytes: 0,
        }
    }

    fn record_event_write(&mut self, event: &Event) -> Result<()> {
        if self.enabled {
            self.bytes = self.bytes.saturating_add(event_bind_bytes(event)?);
        }
        Ok(())
    }

    fn search_bytes(&mut self) -> Option<&mut usize> {
        self.enabled.then_some(&mut self.bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderEventHashAuthority {
    ProviderSupplied,
    NormalizedPayloadFallback,
}

impl ProviderEventHashAuthority {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderSupplied => "provider_supplied",
            Self::NormalizedPayloadFallback => "normalized_payload_fallback",
        }
    }
}

impl Store {
    pub(crate) fn bind_event_identity_alias(
        &self,
        alias_id: Uuid,
        event_id: Uuid,
        reason: &str,
        created_at_ms: i64,
    ) -> Result<()> {
        if alias_id == event_id {
            return Ok(());
        }
        let target_exists = self
            .conn
            .query_row(
                "SELECT 1 FROM events WHERE id = ?1",
                params![event_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let alias_is_event = self
            .conn
            .query_row(
                "SELECT 1 FROM events WHERE id = ?1",
                params![alias_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let existing = self.event_alias_target_id(alias_id)?;
        if !target_exists || alias_is_event || existing.is_some_and(|existing| existing != event_id)
        {
            return Err(StoreError::NativePathEventIdentityAliasConflict);
        }
        if existing.is_none() {
            self.conn.execute(
                "INSERT INTO event_aliases (alias_id, event_id, reason, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    alias_id.to_string(),
                    event_id.to_string(),
                    reason,
                    created_at_ms
                ],
            )?;
        }
        Ok(())
    }

    pub fn provider_event_dedupe_key(
        provider: CaptureProvider,
        external_session_id: &str,
        provider_index: u64,
        payload_hash: &str,
    ) -> String {
        format!(
            "provider:{}:{}:{}:{}",
            provider.as_str(),
            external_session_id,
            provider_index,
            payload_hash
        )
    }

    pub fn provider_source_event_dedupe_key(
        source_id: Uuid,
        provider_index: u64,
        payload_hash: &str,
    ) -> String {
        format!("provider-source:{source_id}:{provider_index}:{payload_hash}")
    }

    pub fn provider_event_dedupe_key_with_payload_hash(
        dedupe_key: &str,
        payload_hash: &str,
    ) -> Option<String> {
        let parsed = parse_provider_event_dedupe_key(dedupe_key)?;
        Some(if let Some(source_id) = parsed.source_id {
            format!(
                "provider-source:{source_id}:{}:{payload_hash}",
                parsed.provider_index
            )
        } else {
            format!(
                "provider:{}:{}:{}:{payload_hash}",
                parsed.provider, parsed.external_session_id, parsed.provider_index
            )
        })
    }

    pub fn upsert_event(&self, event: &Event) -> Result<Uuid> {
        self.with_atomic_write(|| {
            self.upsert_event_inner(event, &mut NativePathEventBindAccounting::default())
        })
    }

    pub(crate) fn upsert_event_with_native_path_accounting(&self, event: &Event) -> Result<usize> {
        let mut accounting = NativePathEventBindAccounting::enabled();
        self.write_event(event, &mut accounting)?;
        Ok(accounting.bytes)
    }

    fn upsert_event_inner(
        &self,
        event: &Event,
        accounting: &mut NativePathEventBindAccounting,
    ) -> Result<Uuid> {
        let event = durable_event(event)?;
        let event = event.as_ref();
        if let Some(dedupe_key) = &event.dedupe_key {
            reject_provider_event_hash_conflict(&self.conn, dedupe_key)?;
            if let Some(existing_id) = self
                .conn
                .query_row(
                    "SELECT id FROM events WHERE dedupe_key = ?1",
                    params![dedupe_key],
                    |row| parse_uuid(row.get::<_, String>(0)?),
                )
                .optional()?
            {
                return Ok(existing_id);
            }
        }
        self.write_event(event, accounting)
    }

    fn write_event(
        &self,
        event: &Event,
        accounting: &mut NativePathEventBindAccounting,
    ) -> Result<Uuid> {
        let event = durable_event(event)?;
        let event = event.as_ref();
        let cold_load = self.native_cold_write_scope_active();
        let previous_searchable_count = if cold_load {
            0
        } else {
            semantic_searchable_document_count_from_stored_event(&self.conn, event.id)?
        };

        accounting.record_event_write(event)?;
        self.conn
            .prepare_cached(
                r#"
                INSERT INTO events
                (id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
                ON CONFLICT(id) DO UPDATE SET
                    seq = excluded.seq,
                    history_record_id = excluded.history_record_id,
                    session_id = excluded.session_id,
                    run_id = excluded.run_id,
                    event_type = excluded.event_type,
                    role = excluded.role,
                    occurred_at_ms = excluded.occurred_at_ms,
                    capture_source_id = excluded.capture_source_id,
                    payload_json = excluded.payload_json,
                    payload_blob_id = excluded.payload_blob_id,
                    dedupe_key = excluded.dedupe_key,
                    visibility = excluded.visibility,
                    fidelity = excluded.fidelity,
                    sync_state = excluded.sync_state,
                    sync_version = excluded.sync_version,
                    deleted_at_ms = excluded.deleted_at_ms,
                    metadata_json = excluded.metadata_json
                "#,
            )?
            .execute(params![
                    event.id.to_string(),
                    event.seq as i64,
                    optional_uuid_string(event.history_record_id),
                    optional_uuid_string(event.session_id),
                    optional_uuid_string(event.run_id),
                    event.event_type.as_str(),
                    event.role.map(|role| role.as_str()),
                    timestamp_ms(event.occurred_at),
                    optional_uuid_string(event.capture_source_id),
                    serde_json::to_string(&event.payload)?,
                    optional_uuid_string(event.payload_blob_id),
                    event.dedupe_key.as_deref(),
                    event.sync.visibility.as_str(),
                    event.sync.fidelity.as_str(),
                    event.sync.sync_state.as_str(),
                    event.sync.sync_version as i64,
                    optional_timestamp_ms(event.sync.deleted_at),
                    serde_json::to_string(&event.sync.metadata)?,
                ])?;
        if !cold_load {
            upsert_event_search_projection_for_event(
                &self.conn,
                event.id,
                event,
                self.event_search_projection_capabilities()?,
                accounting.search_bytes(),
            )?;
            adjust_semantic_searchable_item_stats(
                &self.conn,
                previous_searchable_count,
                semantic_searchable_document_count_for_event(event),
                accounting.search_bytes(),
            )?;
        }
        let id = if let Some(dedupe_key) = &event.dedupe_key {
            self.event_id_by_dedupe_key(dedupe_key)?
        } else {
            event.id
        };
        self.journal_event_mutated(id)?;
        Ok(id)
    }

    /// Reconciles an event produced by the provider normalization pipeline.
    ///
    /// Provider-supplied hashes are authoritative and differing values always conflict. Fallback
    /// hashes describe ctx's normalized payload rather than provider identity, so normalization
    /// changes may replace the existing event in place when the stored row is also known to use a
    /// fallback hash. Rows written before hash authority was recorded are recognized only when the
    /// stored hash exactly matches the stored normalized body.
    pub fn reconcile_provider_event(
        &self,
        event: &Event,
        incoming_authority: ProviderEventHashAuthority,
    ) -> Result<bool> {
        self.with_atomic_write(|| {
            self.reconcile_provider_event_inner(
                event,
                incoming_authority,
                None,
                &mut NativePathEventBindAccounting::default(),
            )
        })
    }

    pub(crate) fn reconcile_provider_event_with_native_path_accounting(
        &self,
        event: &Event,
        incoming_authority: ProviderEventHashAuthority,
    ) -> Result<(bool, usize)> {
        let mut accounting = NativePathEventBindAccounting::enabled();
        let result = self.with_atomic_write(|| {
            self.reconcile_provider_event_inner(event, incoming_authority, None, &mut accounting)
        })?;
        Ok((result, accounting.bytes))
    }

    pub(crate) fn reconcile_provider_event_migrating_exact_legacy_provider_hash_with_native_path_accounting(
        &self,
        event: &Event,
        exact_legacy_provider_hash: &str,
    ) -> Result<(bool, usize)> {
        if exact_legacy_provider_hash.is_empty() {
            return Err(StoreError::InvalidNativePathLegacyProviderHashMigration);
        }
        let mut event = event.clone();
        event.sync.metadata[PROVIDER_EVENT_HASH_AUTHORITY_KEY] = serde_json::Value::String(
            ProviderEventHashAuthority::NormalizedPayloadFallback
                .as_str()
                .to_owned(),
        );
        let mut accounting = NativePathEventBindAccounting::enabled();
        let result = self.with_atomic_write(|| {
            self.reconcile_provider_event_inner(
                &event,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
                Some(exact_legacy_provider_hash),
                &mut accounting,
            )
        })?;
        Ok((result, accounting.bytes))
    }

    fn reconcile_provider_event_inner(
        &self,
        event: &Event,
        incoming_authority: ProviderEventHashAuthority,
        exact_legacy_provider_hash: Option<&str>,
        accounting: &mut NativePathEventBindAccounting,
    ) -> Result<bool> {
        if !provider_output_is_retained_failure(event) {
            return Ok(false);
        }
        let Some(incoming_key) = event.dedupe_key.as_deref() else {
            return self.insert_event_if_absent_inner(event, accounting);
        };
        let Some(incoming) = parse_provider_event_dedupe_key(incoming_key) else {
            return self.insert_event_if_absent_inner(event, accounting);
        };
        if self.insert_event_if_absent_without_conflict_check(event, accounting)? {
            return Ok(true);
        }
        let Some(existing) = provider_event_with_same_identity(&self.conn, &incoming)? else {
            return Ok(false);
        };
        let existing_key = existing
            .dedupe_key
            .as_deref()
            .and_then(parse_provider_event_dedupe_key)
            .ok_or_else(|| StoreError::ProviderEventConflict {
                provider: incoming.provider.clone(),
                external_session_id: incoming.external_session_id.clone(),
                provider_index: incoming.provider_index,
                existing_hash: "invalid-provider-event-dedupe-key".to_owned(),
                new_hash: incoming.payload_hash.clone(),
            })?;
        let existing_authority = stored_provider_event_hash_authority(&existing, &existing_key)?;
        let hashes_match = existing_key.payload_hash == incoming.payload_hash;

        if existing_authority == ProviderEventHashAuthority::ProviderSupplied {
            if let Some(expected_hash) = exact_legacy_provider_hash {
                if incoming_authority != ProviderEventHashAuthority::NormalizedPayloadFallback
                    || existing_key.payload_hash != expected_hash
                {
                    return Err(provider_event_conflict(
                        &incoming,
                        &existing_key.payload_hash,
                    ));
                }
                let mut replacement = event.clone();
                replacement.id = existing.id;
                replacement.seq = existing.seq;
                self.write_event(&replacement, accounting)?;
                return Ok(false);
            }
        }

        if !hashes_match
            && (incoming_authority == ProviderEventHashAuthority::ProviderSupplied
                || existing_authority == ProviderEventHashAuthority::ProviderSupplied)
        {
            return Err(provider_event_conflict(
                &incoming,
                &existing_key.payload_hash,
            ));
        }

        // A matching identity hash is a strict replay: legacy payload, source,
        // and metadata remain immutable. Fallback normalization migrations
        // below are permitted only when their normalized payload hash changes.
        if hashes_match {
            if existing.sync.deleted_at.is_some() && event.sync.deleted_at.is_none() {
                let mut restoration = existing;
                restoration.sync.deleted_at = None;
                self.write_event(&restoration, accounting)?;
            }
            return Ok(false);
        }

        let mut replacement = event.clone();
        replacement.id = existing.id;
        replacement.seq = existing.seq;
        self.write_event(&replacement, accounting)?;
        Ok(false)
    }

    pub fn insert_event_if_absent(&self, event: &Event) -> Result<bool> {
        self.with_atomic_write(|| {
            self.insert_event_if_absent_inner(event, &mut NativePathEventBindAccounting::default())
        })
    }

    fn insert_event_if_absent_inner(
        &self,
        event: &Event,
        accounting: &mut NativePathEventBindAccounting,
    ) -> Result<bool> {
        let inserted = self.insert_event_if_absent_without_conflict_check(event, accounting)?;
        if !inserted {
            if let Some(dedupe_key) = &event.dedupe_key {
                reject_provider_event_hash_conflict(&self.conn, dedupe_key)?;
            }
        }
        Ok(inserted)
    }

    fn insert_event_if_absent_without_conflict_check(
        &self,
        event: &Event,
        accounting: &mut NativePathEventBindAccounting,
    ) -> Result<bool> {
        let event = durable_event(event)?;
        let event = event.as_ref();
        let cold_load = self.native_cold_write_scope_active();
        accounting.record_event_write(event)?;
        let changed = self
                .conn
                .prepare_cached(
                    r#"
                    INSERT OR IGNORE INTO events
                    (id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
                    "#,
                )?
                .execute(params![
                    event.id.to_string(),
                    event.seq as i64,
                    optional_uuid_string(event.history_record_id),
                    optional_uuid_string(event.session_id),
                    optional_uuid_string(event.run_id),
                    event.event_type.as_str(),
                    event.role.map(|role| role.as_str()),
                    timestamp_ms(event.occurred_at),
                    optional_uuid_string(event.capture_source_id),
                    serde_json::to_string(&event.payload)?,
                    optional_uuid_string(event.payload_blob_id),
                    event.dedupe_key.as_deref(),
                    event.sync.visibility.as_str(),
                    event.sync.fidelity.as_str(),
                    event.sync.sync_state.as_str(),
                    event.sync.sync_version as i64,
                    optional_timestamp_ms(event.sync.deleted_at),
                    serde_json::to_string(&event.sync.metadata)?,
                ])?;
        if changed > 0 {
            if !cold_load {
                insert_event_search_projection_for_event(
                    &self.conn,
                    event,
                    self.event_search_projection_capabilities()?,
                    accounting.search_bytes(),
                )?;
                adjust_semantic_searchable_item_stats(
                    &self.conn,
                    0,
                    semantic_searchable_document_count_for_event(event),
                    accounting.search_bytes(),
                )?;
            }
            self.journal_event_mutated(event.id)?;
        }
        Ok(changed > 0)
    }

    pub fn event_id_by_dedupe_key(&self, dedupe_key: &str) -> Result<Uuid> {
        self.conn
            .query_row(
                "SELECT id FROM events WHERE dedupe_key = ?1",
                params![dedupe_key],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn event_id_by_seq(&self, seq: u64) -> Result<Uuid> {
        self.conn
            .query_row(
                "SELECT id FROM events WHERE seq = ?1",
                params![seq as i64],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn get_event(&self, id: Uuid) -> Result<Event> {
        self.conn
            .query_row(
                event_select_sql(
                    "WHERE id = COALESCE(
                        (SELECT event_id FROM event_aliases WHERE alias_id = ?1),
                        ?1
                    )",
                )
                .as_str(),
                params![id.to_string()],
                event_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn event_alias_target_id(&self, alias_id: Uuid) -> Result<Option<Uuid>> {
        self.conn
            .query_row(
                "SELECT event_id FROM event_aliases WHERE alias_id = ?1",
                params![alias_id.to_string()],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn events_by_id_prefix(&self, prefix: &str) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql(
                "WHERE id IN (
                    SELECT id FROM events WHERE id LIKE ?1
                    UNION
                    SELECT event_id FROM event_aliases WHERE alias_id LIKE ?1
                ) ORDER BY id LIMIT 2",
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![format!("{prefix}%")], event_from_row)?;
        collect_rows(rows)
    }

    pub fn events_for_session(&self, session_id: Uuid) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql("WHERE session_id = ?1 ORDER BY seq, occurred_at_ms").as_str(),
        )?;
        let rows = stmt.query_map(params![session_id.to_string()], event_from_row)?;
        collect_rows(rows)
    }

    pub fn events_for_session_limited(&self, session_id: Uuid, limit: usize) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql("WHERE session_id = ?1 ORDER BY seq, occurred_at_ms LIMIT ?2")
                .as_str(),
        )?;
        let rows = stmt.query_map(
            params![
                session_id.to_string(),
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            event_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn event_for_session_by_type_and_payload_string(
        &self,
        session_id: Uuid,
        event_type: EventType,
        payload_path: &str,
        expected: &str,
    ) -> Result<Option<Event>> {
        self.conn
            .query_row(
                event_select_sql(
                    "WHERE session_id = ?1 AND event_type = ?2 AND json_extract(payload_json, ?3) = ?4 ORDER BY seq DESC LIMIT 1",
                )
                .as_str(),
                params![
                    session_id.to_string(),
                    event_type.as_str(),
                    payload_path,
                    expected
                ],
                event_from_row,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn events_for_session_window(
        &self,
        event: &Event,
        before: usize,
        after: usize,
    ) -> Result<Vec<Event>> {
        let Some(session_id) = event.session_id else {
            return Ok(vec![event.clone()]);
        };
        let event_seq = i64::try_from(event.seq).unwrap_or(i64::MAX);
        let mut events = if before == 0 {
            Vec::new()
        } else {
            let mut stmt = self.conn.prepare(
                    event_select_sql(
                        "WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC, occurred_at_ms DESC LIMIT ?3",
                    )
                    .as_str(),
                )?;
            let rows = stmt.query_map(
                params![
                    session_id.to_string(),
                    event_seq,
                    i64::try_from(before).unwrap_or(i64::MAX)
                ],
                event_from_row,
            )?;
            let mut rows = collect_rows(rows)?;
            rows.reverse();
            rows
        };
        events.push(event.clone());
        if after > 0 {
            let mut stmt = self.conn.prepare(
                event_select_sql(
                    "WHERE session_id = ?1 AND seq > ?2 ORDER BY seq, occurred_at_ms LIMIT ?3",
                )
                .as_str(),
            )?;
            let rows = stmt.query_map(
                params![
                    session_id.to_string(),
                    event_seq,
                    i64::try_from(after).unwrap_or(i64::MAX)
                ],
                event_from_row,
            )?;
            events.extend(collect_rows(rows)?);
        }
        Ok(events)
    }

    pub fn events_for_record(&self, record_id: Uuid) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
                event_select_sql(
                    r#"
                    WHERE history_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                       OR run_id IN (
                            SELECT id FROM runs
                            WHERE history_record_id = ?1
                               OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                       )
                    ORDER BY seq, occurred_at_ms
                    "#,
                )
                .as_str(),
            )?;
        let rows = stmt.query_map(params![record_id.to_string()], event_from_row)?;
        collect_rows(rows)
    }

    pub(crate) fn list_events(&self) -> Result<Vec<Event>> {
        let mut stmt = self
            .conn
            .prepare(event_select_sql("ORDER BY seq, occurred_at_ms, id").as_str())?;
        let rows = stmt.query_map([], event_from_row)?;
        collect_rows(rows)
    }

    pub fn max_events_per_history_record(&self) -> Result<i64> {
        let max_events = self.conn.query_row(
            r#"
                SELECT COALESCE(MAX(event_count), 0)
                FROM (
                    SELECT COUNT(*) AS event_count
                    FROM events
                    GROUP BY history_record_id
                )
                "#,
            [],
            |row| row.get(0),
        )?;
        Ok(max_events)
    }

    pub fn has_at_least_events(&self, threshold: i64) -> Result<bool> {
        if threshold <= 0 {
            return Ok(true);
        }
        let exists = self.conn.query_row(
            r#"
                SELECT EXISTS(
                    SELECT 1
                    FROM events
                    LIMIT 1 OFFSET ?1
                )
                "#,
            params![threshold - 1],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(exists != 0)
    }
}

fn provider_event_with_same_identity(
    conn: &Connection,
    incoming: &ParsedProviderEventDedupeKey,
) -> Result<Option<Event>> {
    let prefix = provider_event_dedupe_key_prefix(incoming);
    let upper_bound = provider_event_dedupe_key_upper_bound(&prefix);
    conn.query_row(
        event_select_sql("WHERE dedupe_key >= ?1 AND dedupe_key < ?2 ORDER BY dedupe_key LIMIT 1")
            .as_str(),
        params![prefix, upper_bound],
        event_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn stored_provider_event_hash_authority(
    event: &Event,
    parsed_key: &ParsedProviderEventDedupeKey,
) -> Result<ProviderEventHashAuthority> {
    match event
        .sync
        .metadata
        .get(PROVIDER_EVENT_HASH_AUTHORITY_KEY)
        .and_then(serde_json::Value::as_str)
    {
        Some("provider_supplied") => return Ok(ProviderEventHashAuthority::ProviderSupplied),
        Some("normalized_payload_fallback") => {
            return Ok(ProviderEventHashAuthority::NormalizedPayloadFallback)
        }
        Some(_) => return Ok(ProviderEventHashAuthority::ProviderSupplied),
        None => {}
    }

    let Some(body) = event.payload.get("body") else {
        return Ok(ProviderEventHashAuthority::ProviderSupplied);
    };
    if compute_payload_hash(body)? == parsed_key.payload_hash {
        Ok(ProviderEventHashAuthority::NormalizedPayloadFallback)
    } else {
        Ok(ProviderEventHashAuthority::ProviderSupplied)
    }
}

fn provider_event_conflict(
    incoming: &ParsedProviderEventDedupeKey,
    existing_hash: &str,
) -> StoreError {
    StoreError::ProviderEventConflict {
        provider: incoming.provider.clone(),
        external_session_id: incoming.external_session_id.clone(),
        provider_index: incoming.provider_index,
        existing_hash: existing_hash.to_owned(),
        new_hash: incoming.payload_hash.clone(),
    }
}

pub(crate) fn reject_provider_event_hash_conflict(
    conn: &Connection,
    dedupe_key: &str,
) -> Result<()> {
    let Some(parsed) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    let prefix = provider_event_dedupe_key_prefix(&parsed);
    let upper_bound = provider_event_dedupe_key_upper_bound(&prefix);
    let mut stmt = conn.prepare(
        "SELECT dedupe_key FROM events
         WHERE dedupe_key >= ?1 AND dedupe_key < ?2
         ORDER BY dedupe_key",
    )?;
    let rows = stmt.query_map(params![prefix, upper_bound], |row| row.get::<_, String>(0))?;
    reject_provider_event_hash_conflict_from_rows(dedupe_key, rows)
}

pub(crate) fn reject_provider_event_hash_conflict_tx(
    tx: &Transaction<'_>,
    dedupe_key: &str,
) -> Result<()> {
    let Some(parsed) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    let prefix = provider_event_dedupe_key_prefix(&parsed);
    let upper_bound = provider_event_dedupe_key_upper_bound(&prefix);
    let mut stmt = tx.prepare(
        "SELECT dedupe_key FROM events
         WHERE dedupe_key >= ?1 AND dedupe_key < ?2
         ORDER BY dedupe_key",
    )?;
    let rows = stmt.query_map(params![prefix, upper_bound], |row| row.get::<_, String>(0))?;
    reject_provider_event_hash_conflict_from_rows(dedupe_key, rows)
}

pub(crate) fn reject_provider_event_hash_conflict_from_rows(
    dedupe_key: &str,
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<String>>,
) -> Result<()> {
    let Some(incoming) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    for row in rows {
        let existing_key = row?;
        let Some(existing) = parse_provider_event_dedupe_key(&existing_key) else {
            continue;
        };
        if existing.has_same_event_identity(&incoming)
            && existing.payload_hash != incoming.payload_hash
        {
            return Err(StoreError::ProviderEventConflict {
                provider: incoming.provider,
                external_session_id: incoming.external_session_id,
                provider_index: incoming.provider_index,
                existing_hash: existing.payload_hash,
                new_hash: incoming.payload_hash,
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedProviderEventDedupeKey {
    pub(crate) provider: String,
    pub(crate) external_session_id: String,
    pub(crate) source_id: Option<String>,
    pub(crate) provider_index: u64,
    pub(crate) payload_hash: String,
}

impl ParsedProviderEventDedupeKey {
    fn has_same_event_identity(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.external_session_id == other.external_session_id
            && self.source_id == other.source_id
            && self.provider_index == other.provider_index
    }
}

fn provider_event_dedupe_key_prefix(parsed: &ParsedProviderEventDedupeKey) -> String {
    if let Some(source_id) = &parsed.source_id {
        format!("provider-source:{source_id}:{}:", parsed.provider_index)
    } else {
        format!(
            "provider:{}:{}:{}:",
            parsed.provider, parsed.external_session_id, parsed.provider_index
        )
    }
}

fn provider_event_dedupe_key_upper_bound(prefix: &str) -> String {
    let mut upper_bound = prefix.to_owned();
    upper_bound.push(char::MAX);
    upper_bound
}

pub(crate) fn parse_provider_event_dedupe_key(
    dedupe_key: &str,
) -> Option<ParsedProviderEventDedupeKey> {
    if let Some(rest) = dedupe_key.strip_prefix("provider-source:") {
        let mut parts = rest.splitn(3, ':');
        let source_id = parts.next()?.to_owned();
        let provider_index = parts.next()?.parse().ok()?;
        let payload_hash = parts.next()?.to_owned();
        if source_id.is_empty() || payload_hash.is_empty() {
            return None;
        }
        return Some(ParsedProviderEventDedupeKey {
            provider: "provider-source".to_owned(),
            external_session_id: source_id.clone(),
            source_id: Some(source_id),
            provider_index,
            payload_hash,
        });
    }

    let mut parts = dedupe_key.splitn(5, ':');
    let prefix = parts.next()?;
    if prefix != "provider" {
        return None;
    }
    let provider = parts.next()?.to_owned();
    let external_session_id = parts.next()?.to_owned();
    let provider_index = parts.next()?.parse().ok()?;
    let payload_hash = parts.next()?.to_owned();
    if provider.is_empty() || external_session_id.is_empty() || payload_hash.is_empty() {
        None
    } else {
        Some(ParsedProviderEventDedupeKey {
            provider,
            external_session_id,
            source_id: None,
            provider_index,
            payload_hash,
        })
    }
}

pub(crate) fn event_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM events {tail}"
    )
}

pub(crate) fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        seq: nonnegative_i64_to_u64(row.get(1)?)?,
        history_record_id: parse_optional_uuid(row.get(2)?)?,
        session_id: parse_optional_uuid(row.get(3)?)?,
        run_id: parse_optional_uuid(row.get(4)?)?,
        event_type: parse_text_enum::<EventType>(row.get::<_, String>(5)?)?,
        role: row
            .get::<_, Option<String>>(6)?
            .map(parse_text_enum::<EventRole>)
            .transpose()?,
        occurred_at: ms_to_time(row.get(7)?)?,
        capture_source_id: parse_optional_uuid(row.get(8)?)?,
        payload: parse_json(row.get::<_, String>(9)?)?,
        payload_blob_id: parse_optional_uuid(row.get(10)?)?,
        dedupe_key: row.get(11)?,
        sync: sync_metadata_from_row(row, 12, 13, 14, 15, 16, 17)?,
    })
}

#[cfg(test)]
mod tests;
