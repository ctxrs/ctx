//! Bounded Hermes session-context and ancestry resolution.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use ctx_history_core::{
    derive_session_id, AgentType, NativeSessionKey, SessionIdentityInput, SourceKey,
    StableEntityId, TypedKey,
};

use super::{
    super::layout::{HermesSchema, SessionField},
    HERMES_LOGICAL_SESSION_KIND, HERMES_SESSION_NAMESPACE,
};
use crate::{
    provider::{
        native_ingestion::{NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS},
        normalization::provider_required_timestamp_seconds,
    },
    CaptureError,
};

pub(super) const HERMES_PARENT_CHAIN_MAX_DEPTH: usize = 256;
pub(super) const HERMES_ANCESTRY_MAX_ROWS: usize = 256;
pub(super) const HERMES_ANCESTRY_QUERY_MAX_ROWS: usize = HERMES_ANCESTRY_MAX_ROWS + 1;
pub(super) const HERMES_CONTEXT_CACHE_MAX_ROWS: usize = 256;
pub(super) const HERMES_CONTEXT_CACHE_MAX_BYTES: usize = NATIVE_INGESTION_PAGE_MAX_BYTES;
// Sixty-four direct rows can each retain a bounded session key, parent key, and three metadata
// fields. Two native pages cover that fixed product envelope without making ordinary rows compete.
pub(super) const HERMES_DIRECT_CONTEXT_WORKSET_MAX_BYTES: usize =
    NATIVE_INGESTION_PAGE_MAX_BYTES * 2;
// Ancestors retain only session and parent keys, so one native page is the stricter envelope.
pub(super) const HERMES_ANCESTRY_WORKSET_MAX_BYTES: usize = NATIVE_INGESTION_PAGE_MAX_BYTES;
const HERMES_SESSION_METADATA_MAX_CHARS: usize = 8 * 1024;
pub(super) const HERMES_SESSION_KEY_MAX_BYTES: usize = 64 * 1024;
const HERMES_CONTEXT_ENTRY_OVERHEAD_BYTES: usize = 512;
const HERMES_LINK_ENTRY_OVERHEAD_BYTES: usize = 192;
pub(super) const HERMES_DIRECT_CONTEXT_RESIDENT_MAX_BYTES: usize =
    HERMES_DIRECT_CONTEXT_WORKSET_MAX_BYTES
        + HERMES_CONTEXT_ENTRY_OVERHEAD_BYTES
        + HERMES_SESSION_KEY_MAX_BYTES * 2
        + HERMES_SESSION_METADATA_MAX_CHARS * 4 * 3;
pub(super) const HERMES_ANCESTRY_RESIDENT_MAX_BYTES: usize = HERMES_ANCESTRY_WORKSET_MAX_BYTES
    + HERMES_LINK_ENTRY_OVERHEAD_BYTES
    + HERMES_SESSION_KEY_MAX_BYTES * 2;

#[derive(Debug, Clone)]
pub(super) struct HermesSessionContext {
    pub(super) session_id: StableEntityId,
    pub(super) parent_session_id: Option<StableEntityId>,
    pub(super) root_session_id: StableEntityId,
    pub(super) branch: Option<String>,
    pub(super) agent_type: String,
    pub(super) is_primary: bool,
    pub(super) workspace: Option<String>,
    pub(super) cwd: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) enum HermesSessionResolution {
    Context(Arc<HermesSessionContext>),
    Missing,
    Rejected(Arc<str>),
}

impl HermesSessionResolution {
    fn rejected(reason: impl Into<String>) -> Self {
        Self::Rejected(Arc::from(reason.into()))
    }

    fn owned_bytes(&self) -> usize {
        match self {
            Self::Context(context) => HERMES_CONTEXT_ENTRY_OVERHEAD_BYTES
                .saturating_add(context.branch.as_deref().map(str::len).unwrap_or(0))
                .saturating_add(context.agent_type.len())
                .saturating_add(context.workspace.as_deref().map(str::len).unwrap_or(0))
                .saturating_add(context.cwd.as_deref().map(str::len).unwrap_or(0)),
            Self::Missing => HERMES_CONTEXT_ENTRY_OVERHEAD_BYTES,
            Self::Rejected(reason) => {
                HERMES_CONTEXT_ENTRY_OVERHEAD_BYTES.saturating_add(reason.len())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HermesContextMemoCounters {
    pub(super) direct_query_batches: u64,
    pub(super) ancestry_query_batches: u64,
    pub(super) max_query_batches_per_page: u64,
    pub(super) max_direct_rows_per_query: u64,
    pub(super) max_ancestry_rows_per_query: u64,
    pub(super) max_direct_bytes_per_query: u64,
    pub(super) max_ancestry_bytes_per_query: u64,
    pub(super) peak_cache_rows: u64,
    pub(super) peak_cache_bytes: u64,
}

struct CachedResolution {
    resolution: HermesSessionResolution,
    owned_bytes: usize,
    last_used: u64,
}

pub(super) struct HermesSessionContextMemo<'connection> {
    conn: &'connection rusqlite::Connection,
    schema: HermesSchema,
    source: SourceKey,
    cache: BTreeMap<String, CachedResolution>,
    cache_bytes: usize,
    clock: u64,
    counters: HermesContextMemoCounters,
}

impl<'connection> HermesSessionContextMemo<'connection> {
    pub(super) fn new(
        conn: &'connection rusqlite::Connection,
        schema: &HermesSchema,
        source: &SourceKey,
    ) -> Self {
        Self {
            conn,
            schema: schema.clone(),
            source: source.clone(),
            cache: BTreeMap::new(),
            cache_bytes: 0,
            clock: 0,
            counters: HermesContextMemoCounters::default(),
        }
    }

    pub(super) fn counters(&self) -> HermesContextMemoCounters {
        self.counters
    }

    pub(super) fn resolve_page(
        &mut self,
        provider_session_ids: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, HermesSessionResolution>, CaptureError> {
        if provider_session_ids.len() > NATIVE_INGESTION_PAGE_MAX_UNITS {
            return Err(CaptureError::SystemInvariant(
                "Hermes context request exceeded the native page row bound",
            ));
        }
        let queries_before = self
            .counters
            .direct_query_batches
            .saturating_add(self.counters.ancestry_query_batches);
        let mut resolved = BTreeMap::new();
        let mut missing = BTreeSet::new();
        for provider_session_id in provider_session_ids {
            if provider_session_id.len() > HERMES_SESSION_KEY_MAX_BYTES {
                let resolution = HermesSessionResolution::rejected(format!(
                    "Hermes session identifier exceeds the {HERMES_SESSION_KEY_MAX_BYTES}-byte Core key bound"
                ));
                self.cache_insert(provider_session_id.clone(), resolution.clone())?;
                resolved.insert(provider_session_id.clone(), resolution);
            } else if let Some(cached) = self.cache_get(provider_session_id)? {
                resolved.insert(provider_session_id.clone(), cached);
            } else {
                missing.insert(provider_session_id.clone());
            }
        }

        if !missing.is_empty() {
            match self.load_direct_rows(&missing)? {
                BoundedDirectRows::Exceeded => {
                    for provider_session_id in missing {
                        let resolution = HermesSessionResolution::rejected(format!(
                            "Hermes direct session contexts exceed the {HERMES_DIRECT_CONTEXT_WORKSET_MAX_BYTES}-byte page workset bound"
                        ));
                        self.cache_insert(provider_session_id.clone(), resolution.clone())?;
                        resolved.insert(provider_session_id, resolution);
                    }
                }
                BoundedDirectRows::Rows(direct_rows) => {
                    let empty_ancestry = BTreeMap::new();
                    for provider_session_id in missing {
                        let resolution = match direct_rows.get(&provider_session_id) {
                            None => HermesSessionResolution::Missing,
                            Some(row) if row.error_code != 0 => {
                                HermesSessionResolution::rejected(direct_error(
                                    &provider_session_id,
                                    row.error_code,
                                ))
                            }
                            Some(row) if row.parent_session_id.is_none() => self
                                .context_resolution(
                                    &provider_session_id,
                                    row,
                                    &direct_rows,
                                    &empty_ancestry,
                                )?,
                            Some(row) => match self.load_ancestry(row)? {
                                BoundedAncestry::Exceeded => HermesSessionResolution::rejected(
                                    format!(
                                        "Hermes session ancestry exceeds the {HERMES_ANCESTRY_MAX_ROWS}-row or {HERMES_ANCESTRY_WORKSET_MAX_BYTES}-byte per-session bound"
                                    ),
                                ),
                                BoundedAncestry::Links(links) => self.context_resolution(
                                    &provider_session_id,
                                    row,
                                    &direct_rows,
                                    &links,
                                )?,
                            },
                        };
                        self.cache_insert(provider_session_id.clone(), resolution.clone())?;
                        resolved.insert(provider_session_id, resolution);
                    }
                }
            }
        }

        let queries_after = self
            .counters
            .direct_query_batches
            .saturating_add(self.counters.ancestry_query_batches);
        self.counters.max_query_batches_per_page = self
            .counters
            .max_query_batches_per_page
            .max(queries_after.saturating_sub(queries_before));
        Ok(resolved)
    }

    fn load_direct_rows(
        &mut self,
        provider_session_ids: &BTreeSet<String>,
    ) -> Result<BoundedDirectRows, CaptureError> {
        self.counters.direct_query_batches = checked_counter(
            self.counters.direct_query_batches,
            "direct context query batches",
        )?;
        let sessions = self.schema.sessions();
        let lookup_index = quoted_identifier(self.schema.session_id_lookup_index());
        let id = sessions.expression(SessionField::Id)?;
        let parent = sessions.expression(SessionField::ParentSessionId)?;
        let started_at = sessions.expression(SessionField::StartedAt)?;
        let ended_at = sessions.expression(SessionField::EndedAt)?;
        let cwd = sessions.expression(SessionField::Cwd)?;
        let branch = sessions.expression(SessionField::GitBranch)?;
        let repo = sessions.expression(SessionField::GitRepoRoot)?;
        let placeholders = placeholders(provider_session_ids.len());
        let safe_parent = safe_optional_key(parent);
        let safe_started_at = safe_real(started_at, false);
        let safe_ended_at = safe_real(ended_at, true);
        let safe_cwd = safe_metadata(cwd);
        let safe_branch = safe_metadata(branch);
        let safe_repo = safe_metadata(repo);
        let error_code = format!(
            "case \
             when typeof({parent}) not in ('null', 'text') then 1 \
             when typeof({parent}) = 'text' and octet_length({parent}) > {HERMES_SESSION_KEY_MAX_BYTES} then 2 \
             when typeof({started_at}) not in ('integer', 'real') then 3 \
             when typeof({ended_at}) not in ('null', 'integer', 'real') then 4 \
             when typeof({cwd}) not in ('null', 'text') then 5 \
             when typeof({branch}) not in ('null', 'text') then 6 \
             when typeof({repo}) not in ('null', 'text') then 7 \
             else 0 end"
        );
        let sql = format!(
            "select {id}, {safe_parent}, {safe_started_at}, {safe_ended_at}, \
                    {safe_cwd}, {safe_branch}, {safe_repo}, {error_code} \
             from sessions s indexed by {lookup_index} \
             where typeof({id}) = 'text' \
               and octet_length({id}) <= {HERMES_SESSION_KEY_MAX_BYTES} \
               and {id} collate binary in ({placeholders}) \
             order by {id} collate binary, s.rowid"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(
            rusqlite::params_from_iter(provider_session_ids.iter()),
            |row| {
                let id = row.get::<_, String>(0)?;
                let error_code = row.get::<_, i64>(7)?;
                Ok((
                    id,
                    DirectSessionRow {
                        parent_session_id: row.get(1)?,
                        started_at: row.get(2)?,
                        ended_at: row.get(3)?,
                        cwd: row.get(4)?,
                        branch: row.get(5)?,
                        workspace: row.get(6)?,
                        error_code,
                    },
                ))
            },
        )?;
        let mut loaded = BTreeMap::new();
        let mut owned_bytes = 0_usize;
        let mut row_count = 0_u64;
        for row in rows {
            let (id, row) = row?;
            row_count = row_count.saturating_add(1);
            owned_bytes = owned_bytes
                .saturating_add(id.len())
                .saturating_add(row.owned_bytes());
            self.counters.max_direct_rows_per_query =
                self.counters.max_direct_rows_per_query.max(row_count);
            self.counters.max_direct_bytes_per_query = self
                .counters
                .max_direct_bytes_per_query
                .max(owned_bytes as u64);
            if row_count > NATIVE_INGESTION_PAGE_MAX_UNITS as u64
                || owned_bytes > HERMES_DIRECT_CONTEXT_WORKSET_MAX_BYTES
            {
                return Ok(BoundedDirectRows::Exceeded);
            }
            if loaded.insert(id.clone(), row).is_some() {
                return Err(CaptureError::InvalidPayload(format!(
                    "Hermes session {id} is duplicated"
                )));
            }
        }
        Ok(BoundedDirectRows::Rows(loaded))
    }

    fn load_ancestry(
        &mut self,
        direct_row: &DirectSessionRow,
    ) -> Result<BoundedAncestry, CaptureError> {
        let Some(seed) = direct_row.parent_session_id.as_ref() else {
            return Ok(BoundedAncestry::Links(BTreeMap::new()));
        };
        self.counters.ancestry_query_batches = checked_counter(
            self.counters.ancestry_query_batches,
            "ancestry query batches",
        )?;
        let sessions = self.schema.sessions();
        let lookup_index = quoted_identifier(self.schema.session_id_lookup_index());
        let id = sessions.expression(SessionField::Id)?;
        let parent = sessions.expression(SessionField::ParentSessionId)?;
        let safe_parent = safe_optional_key(parent);
        let parent_error = format!(
            "case \
             when typeof({parent}) not in ('null', 'text') then 1 \
             when typeof({parent}) = 'text' and octet_length({parent}) > {HERMES_SESSION_KEY_MAX_BYTES} then 2 \
             else 0 end"
        );
        let sql = format!(
            "with recursive ancestry(rowid, id, parent_session_id, parent_error) as ( \
                 select s.rowid, {id}, {safe_parent}, {parent_error} \
                 from sessions s indexed by {lookup_index} \
                 where typeof({id}) = 'text' \
                   and octet_length({id}) <= {HERMES_SESSION_KEY_MAX_BYTES} \
                   and {id} collate binary = ?1 \
                 union \
                 select s.rowid, {id}, {safe_parent}, {parent_error} \
                 from sessions s indexed by {lookup_index} join ancestry child \
                   on {id} collate binary = child.parent_session_id collate binary \
                 where child.parent_error = 0 \
                   and typeof({id}) = 'text' \
                   and octet_length({id}) <= {HERMES_SESSION_KEY_MAX_BYTES} \
                 limit {HERMES_ANCESTRY_QUERY_MAX_ROWS} \
             ) \
             select id, parent_session_id, parent_error from ancestry"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map([seed], |row| {
            let id = row.get::<_, String>(0)?;
            let error_code = row.get::<_, i64>(2)?;
            Ok((
                id,
                ParentLink {
                    parent_session_id: row.get(1)?,
                    error_code,
                },
            ))
        })?;
        let mut links = BTreeMap::new();
        let mut owned_bytes = 0_usize;
        let mut row_count = 0_u64;
        for row in rows {
            let (id, link) = row?;
            row_count = row_count.saturating_add(1);
            owned_bytes = owned_bytes
                .saturating_add(HERMES_LINK_ENTRY_OVERHEAD_BYTES)
                .saturating_add(id.len())
                .saturating_add(link.parent_session_id.as_deref().map(str::len).unwrap_or(0));
            self.counters.max_ancestry_rows_per_query =
                self.counters.max_ancestry_rows_per_query.max(row_count);
            self.counters.max_ancestry_bytes_per_query = self
                .counters
                .max_ancestry_bytes_per_query
                .max(owned_bytes as u64);
            if row_count > HERMES_ANCESTRY_MAX_ROWS as u64
                || owned_bytes > HERMES_ANCESTRY_WORKSET_MAX_BYTES
            {
                return Ok(BoundedAncestry::Exceeded);
            }
            if links.insert(id.clone(), link).is_some() {
                return Err(CaptureError::InvalidPayload(format!(
                    "Hermes session {id} is duplicated"
                )));
            }
        }
        Ok(BoundedAncestry::Links(links))
    }

    fn context_resolution(
        &mut self,
        provider_session_id: &str,
        row: &DirectSessionRow,
        direct_rows: &BTreeMap<String, DirectSessionRow>,
        ancestry: &BTreeMap<String, ParentLink>,
    ) -> Result<HermesSessionResolution, CaptureError> {
        match self.build_context(provider_session_id, row, direct_rows, ancestry) {
            Ok(context) => Ok(HermesSessionResolution::Context(Arc::new(context))),
            Err(CaptureError::InvalidPayload(reason)) => {
                Ok(HermesSessionResolution::rejected(reason))
            }
            Err(error) => Err(error),
        }
    }

    fn build_context(
        &mut self,
        provider_session_id: &str,
        row: &DirectSessionRow,
        direct_rows: &BTreeMap<String, DirectSessionRow>,
        ancestry: &BTreeMap<String, ParentLink>,
    ) -> Result<HermesSessionContext, CaptureError> {
        provider_required_timestamp_seconds(row.started_at, "Hermes session started_at")?;
        row.ended_at
            .map(|value| provider_required_timestamp_seconds(value, "Hermes session ended_at"))
            .transpose()?;
        let session_id = hermes_session_id(&self.source, provider_session_id)?;
        let parent_session_id = row
            .parent_session_id
            .as_deref()
            .map(|parent| hermes_session_id(&self.source, parent))
            .transpose()?;
        let root_session_id =
            self.resolve_root_session_id(provider_session_id, row, direct_rows, ancestry)?;
        let is_primary = row.parent_session_id.is_none();
        Ok(HermesSessionContext {
            session_id,
            parent_session_id,
            root_session_id,
            branch: row.branch.clone(),
            agent_type: if is_primary {
                AgentType::Primary
            } else {
                AgentType::Subagent
            }
            .as_str()
            .to_owned(),
            is_primary,
            workspace: row.workspace.clone(),
            cwd: row.cwd.clone(),
        })
    }

    fn resolve_root_session_id(
        &mut self,
        provider_session_id: &str,
        row: &DirectSessionRow,
        direct_rows: &BTreeMap<String, DirectSessionRow>,
        ancestry: &BTreeMap<String, ParentLink>,
    ) -> Result<StableEntityId, CaptureError> {
        let mut root = provider_session_id;
        let mut parent = row.parent_session_id.as_deref();
        let mut visited = BTreeSet::new();
        visited.insert(root.to_owned());
        for _ in 0..HERMES_PARENT_CHAIN_MAX_DEPTH {
            let Some(parent_id) = parent else {
                return hermes_session_id(&self.source, root);
            };
            if !visited.insert(parent_id.to_owned()) {
                return Err(CaptureError::InvalidPayload(format!(
                    "Hermes session {} has a cyclic parent chain",
                    provider_session_id
                )));
            }
            if let Some(HermesSessionResolution::Context(context)) = self.cache_get(parent_id)? {
                return Ok(context.root_session_id);
            }
            let (next_parent, error) = if let Some(direct) = direct_rows.get(parent_id) {
                (
                    direct.parent_session_id.as_deref(),
                    direct_parent_error(parent_id, direct.error_code),
                )
            } else if let Some(link) = ancestry.get(parent_id) {
                (
                    link.parent_session_id.as_deref(),
                    parent_error_reason(parent_id, link.error_code),
                )
            } else {
                return Err(CaptureError::InvalidPayload(format!(
                    "Hermes session {} depends on missing parent session {parent_id}",
                    provider_session_id
                )));
            };
            if let Some(error) = error {
                return Err(CaptureError::InvalidPayload(error));
            }
            root = parent_id;
            parent = next_parent;
            if parent.is_none() {
                return hermes_session_id(&self.source, root);
            }
        }
        Err(CaptureError::InvalidPayload(format!(
            "Hermes session {} exceeds the {}-level parent bound",
            provider_session_id, HERMES_PARENT_CHAIN_MAX_DEPTH
        )))
    }

    fn cache_get(
        &mut self,
        provider_session_id: &str,
    ) -> Result<Option<HermesSessionResolution>, CaptureError> {
        self.clock = checked_counter(self.clock, "context cache clock")?;
        Ok(self.cache.get_mut(provider_session_id).map(|entry| {
            entry.last_used = self.clock;
            entry.resolution.clone()
        }))
    }

    fn cache_insert(
        &mut self,
        provider_session_id: String,
        resolution: HermesSessionResolution,
    ) -> Result<(), CaptureError> {
        self.clock = checked_counter(self.clock, "context cache clock")?;
        if let Some(previous) = self.cache.remove(&provider_session_id) {
            self.cache_bytes = self.cache_bytes.saturating_sub(previous.owned_bytes);
        }
        let owned_bytes = provider_session_id
            .len()
            .saturating_add(resolution.owned_bytes());
        if owned_bytes > HERMES_CONTEXT_CACHE_MAX_BYTES {
            return Ok(());
        }
        while self.cache.len() >= HERMES_CONTEXT_CACHE_MAX_ROWS
            || self.cache_bytes.saturating_add(owned_bytes) > HERMES_CONTEXT_CACHE_MAX_BYTES
        {
            let Some(eviction) = self
                .cache
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(evicted) = self.cache.remove(&eviction) {
                self.cache_bytes = self.cache_bytes.saturating_sub(evicted.owned_bytes);
            }
        }
        self.cache_bytes = self.cache_bytes.saturating_add(owned_bytes);
        self.cache.insert(
            provider_session_id,
            CachedResolution {
                resolution,
                owned_bytes,
                last_used: self.clock,
            },
        );
        self.counters.peak_cache_rows = self.counters.peak_cache_rows.max(self.cache.len() as u64);
        self.counters.peak_cache_bytes =
            self.counters.peak_cache_bytes.max(self.cache_bytes as u64);
        Ok(())
    }
}

enum BoundedDirectRows {
    Rows(BTreeMap<String, DirectSessionRow>),
    Exceeded,
}

enum BoundedAncestry {
    Links(BTreeMap<String, ParentLink>),
    Exceeded,
}

struct DirectSessionRow {
    parent_session_id: Option<String>,
    started_at: f64,
    ended_at: Option<f64>,
    cwd: Option<String>,
    branch: Option<String>,
    workspace: Option<String>,
    error_code: i64,
}

impl DirectSessionRow {
    fn owned_bytes(&self) -> usize {
        HERMES_CONTEXT_ENTRY_OVERHEAD_BYTES
            .saturating_add(self.parent_session_id.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(self.cwd.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(self.branch.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(self.workspace.as_deref().map(str::len).unwrap_or(0))
    }
}

struct ParentLink {
    parent_session_id: Option<String>,
    error_code: i64,
}

fn hermes_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> Result<StableEntityId, CaptureError> {
    let native_session_key = NativeSessionKey::native_id(
        HERMES_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: HERMES_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn placeholders(count: usize) -> String {
    (1..=count)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn quoted_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn safe_optional_key(expression: &str) -> String {
    format!(
        "case when typeof({expression}) = 'text' and octet_length({expression}) <= {HERMES_SESSION_KEY_MAX_BYTES} then {expression} else null end"
    )
}

fn safe_real(expression: &str, optional: bool) -> String {
    if optional {
        format!(
            "case when typeof({expression}) in ('integer', 'real') then {expression} else null end"
        )
    } else {
        format!(
            "case when typeof({expression}) in ('integer', 'real') then {expression} else 0.0 end"
        )
    }
}

fn safe_metadata(expression: &str) -> String {
    format!(
        "case when typeof({expression}) = 'text' then substr({expression}, 1, {HERMES_SESSION_METADATA_MAX_CHARS}) else null end"
    )
}

fn direct_error(id: &str, code: i64) -> String {
    let detail = match code {
        1 => "parent_session_id has an invalid SQLite storage class",
        2 => "parent_session_id exceeds the Core session-key bound",
        3 => "started_at has an invalid SQLite storage class",
        4 => "ended_at has an invalid SQLite storage class",
        5 => "cwd has an invalid SQLite storage class",
        6 => "git_branch has an invalid SQLite storage class",
        7 => "git_repo_root has an invalid SQLite storage class",
        _ => "context projection returned an invalid error code",
    };
    format!("Hermes session {id} {detail}")
}

fn parent_error_reason(id: &str, code: i64) -> Option<String> {
    let detail = match code {
        0 => return None,
        1 => "parent_session_id has an invalid SQLite storage class",
        2 => "parent_session_id exceeds the Core session-key bound",
        _ => "ancestry projection returned an invalid error code",
    };
    Some(format!("Hermes session {id} {detail}"))
}

fn direct_parent_error(id: &str, code: i64) -> Option<String> {
    match code {
        1 | 2 => parent_error_reason(id, code),
        _ => None,
    }
}

fn checked_counter(value: u64, name: &str) -> Result<u64, CaptureError> {
    value
        .checked_add(1)
        .ok_or_else(|| CaptureError::InvalidPayload(format!("Hermes {name} overflowed")))
}
