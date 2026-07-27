use std::collections::BTreeSet;

use ctx_history_core::CaptureProvider;
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::source_locators::locator_storage_key;
use crate::{Result, Store, StoreError};

const MAX_GENERATION_TEXT_BYTES: usize = 4 * 1024;
const CAPTURE_SOURCE_KIND: &str = "capture_source";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativePathSourceEntityKind {
    Session,
    SessionEdge,
    Run,
    Event,
    FileTouch,
}

impl NativePathSourceEntityKind {
    const RETIREMENT_ORDER: [Self; 5] = [
        Self::SessionEdge,
        Self::Run,
        Self::Event,
        Self::FileTouch,
        Self::Session,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::SessionEdge => "session_edge",
            Self::Run => "run",
            Self::Event => "event",
            Self::FileTouch => "file_touch",
        }
    }

    const fn order(self) -> i64 {
        match self {
            Self::SessionEdge => 0,
            Self::Run => 1,
            Self::Event => 2,
            Self::FileTouch => 3,
            Self::Session => 4,
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "session" => Some(Self::Session),
            "session_edge" => Some(Self::SessionEdge),
            "run" => Some(Self::Run),
            "event" => Some(Self::Event),
            "file_touch" => Some(Self::FileTouch),
            _ => None,
        }
    }

    const fn retirement_storage(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Session => (
                "sessions",
                "capture_source_id",
                "idx_sessions_source_generation_retirement",
            ),
            Self::SessionEdge => (
                "session_edges",
                "source_id",
                "idx_session_edges_source_generation_retirement",
            ),
            Self::Run => ("runs", "source_id", "idx_runs_source_generation_retirement"),
            Self::Event => (
                "events",
                "capture_source_id",
                "idx_events_source_generation_retirement",
            ),
            Self::FileTouch => (
                "files_touched",
                "source_id",
                "idx_files_touched_source_generation_retirement",
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePathSourceGenerationKey {
    pub provider: CaptureProvider,
    pub source_format: String,
    pub machine_id: String,
    pub canonical_source_identity: String,
    pub locator_identity: String,
    pub cursor_stream: String,
    pub source_revision: String,
    pub generation_id: String,
}

impl NativePathSourceGenerationKey {
    pub(crate) fn bound_value_bytes(&self) -> Result<usize> {
        let fields = [
            self.provider.as_str(),
            self.source_format.as_str(),
            self.machine_id.as_str(),
            self.canonical_source_identity.as_str(),
            self.locator_identity.as_str(),
            self.cursor_stream.as_str(),
            self.source_revision.as_str(),
            self.generation_id.as_str(),
        ];
        if fields.iter().any(|value| value.is_empty())
            || fields
                .iter()
                .any(|value| value.len() > MAX_GENERATION_TEXT_BYTES)
        {
            return Err(StoreError::NativePathSourceGenerationConflict);
        }
        Ok(fields
            .iter()
            .map(|value| value.len())
            .fold(0_usize, usize::saturating_add))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativePathRetainedSourceEntities {
    pub capture_source_ids: Vec<Uuid>,
    pub session_ids: Vec<Uuid>,
    pub session_edge_ids: Vec<Uuid>,
    pub run_ids: Vec<Uuid>,
    pub event_ids: Vec<Uuid>,
    pub file_touch_ids: Vec<Uuid>,
}

impl NativePathRetainedSourceEntities {
    pub(crate) fn len(&self) -> usize {
        self.capture_source_ids
            .len()
            .saturating_add(self.session_ids.len())
            .saturating_add(self.session_edge_ids.len())
            .saturating_add(self.run_ids.len())
            .saturating_add(self.event_ids.len())
            .saturating_add(self.file_touch_ids.len())
    }

    pub(crate) fn bound_value_bytes(&self) -> usize {
        self.len().saturating_mul(16)
    }

    fn iter(&self) -> impl Iterator<Item = (&'static str, Uuid)> + '_ {
        self.capture_source_ids
            .iter()
            .copied()
            .map(|id| (CAPTURE_SOURCE_KIND, id))
            .chain(
                self.session_ids
                    .iter()
                    .copied()
                    .map(|id| (NativePathSourceEntityKind::Session.as_str(), id)),
            )
            .chain(
                self.session_edge_ids
                    .iter()
                    .copied()
                    .map(|id| (NativePathSourceEntityKind::SessionEdge.as_str(), id)),
            )
            .chain(
                self.run_ids
                    .iter()
                    .copied()
                    .map(|id| (NativePathSourceEntityKind::Run.as_str(), id)),
            )
            .chain(
                self.event_ids
                    .iter()
                    .copied()
                    .map(|id| (NativePathSourceEntityKind::Event.as_str(), id)),
            )
            .chain(
                self.file_touch_ids
                    .iter()
                    .copied()
                    .map(|id| (NativePathSourceEntityKind::FileTouch.as_str(), id)),
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePathSourceEntityFrontier {
    pub kind: NativePathSourceEntityKind,
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePathSourceRetirementPage {
    pub next_after: Option<NativePathSourceEntityFrontier>,
    pub done: bool,
    pub inspected: usize,
    pub retired: usize,
}

#[derive(Debug)]
pub(crate) struct NativePathSourceRetirementCandidate {
    pub(crate) kind: NativePathSourceEntityKind,
    pub(crate) id: Uuid,
    pub(crate) retained: bool,
}

pub(crate) enum NativePathSourceRetirementPreparation {
    Replay(NativePathSourceRetirementPage),
    Work {
        candidates: Vec<NativePathSourceRetirementCandidate>,
        next_after: Option<NativePathSourceEntityFrontier>,
        done: bool,
    },
}

enum NativePathSourceRetirementRequest {
    Replay(NativePathSourceRetirementPage),
    Work { state: String },
}

#[derive(Debug)]
struct GenerationState {
    state: String,
    frontier: Option<NativePathSourceEntityFrontier>,
    last_request: Option<NativePathSourceEntityFrontier>,
    last_next: Option<NativePathSourceEntityFrontier>,
    last_done: bool,
    last_inspected: usize,
    last_retired: usize,
}

impl Store {
    pub(crate) fn stage_source_generation_page_tx(
        &self,
        key: &NativePathSourceGenerationKey,
        retained: &NativePathRetainedSourceEntities,
    ) -> Result<()> {
        key.bound_value_bytes()?;
        if retained.len() == 0 || retained.capture_source_ids.is_empty() {
            return Err(StoreError::NativePathSourceGenerationConflict);
        }
        let unique = retained.iter().collect::<BTreeSet<_>>();
        if unique.len() != retained.len() {
            return Err(StoreError::NativePathSourceGenerationConflict);
        }
        let locator = self.authorized_generation_alias_group(key)?;
        let locator_identity = locator_storage_key(&key.locator_identity);
        // A route revision may advance while an earlier generation is only
        // partially staged or retiring. Once the new key is authorized by the
        // exact current route, that stale generation can no longer resume and
        // must not permanently occupy the one-active-generation slot. An
        // exact same-revision generation remains protected so crash recovery
        // must resume its durable generation ID instead of silently replacing
        // it.
        self.conn.execute(
            "DELETE FROM native_path_source_generations
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND locator_identity = ?4 AND generation_id <> ?5
               AND state IN ('staging', 'retiring')
               AND (
                   cursor_stream <> ?6
                   OR canonical_source_identity <> ?7
                   OR source_revision <> ?8
               )",
            params![
                key.provider.as_str(),
                &key.source_format,
                &key.machine_id,
                &locator_identity,
                &key.generation_id,
                &key.cursor_stream,
                &key.canonical_source_identity,
                &key.source_revision,
            ],
        )?;
        self.conn.execute(
            "DELETE FROM native_path_source_generations
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND locator_identity = ?4 AND generation_id <> ?5
               AND state = 'complete'",
            params![
                key.provider.as_str(),
                &key.source_format,
                &key.machine_id,
                &locator_identity,
                &key.generation_id,
            ],
        )?;
        self.conn.execute(
            "INSERT INTO native_path_source_generations (
                 provider, source_format, machine_id, locator_identity,
                 generation_id, cursor_stream, canonical_source_identity,
                 source_revision, state
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'staging')
             ON CONFLICT(
                 provider, source_format, machine_id, locator_identity,
                 generation_id
             ) DO NOTHING",
            params![
                key.provider.as_str(),
                &key.source_format,
                &key.machine_id,
                &locator_identity,
                &key.generation_id,
                &key.cursor_stream,
                &key.canonical_source_identity,
                &key.source_revision,
            ],
        )?;
        let state = self.generation_state(key)?;
        if state.state != "staging" {
            return Err(StoreError::NativePathSourceGenerationConflict);
        }

        for capture_source_id in &retained.capture_source_ids {
            let authorized = self.conn.query_row(
                "SELECT EXISTS(
                     SELECT 1
                     FROM capture_sources source
                     JOIN capture_source_provider_routes route
                       ON route.capture_source_id = source.id
                     WHERE source.id = ?1
                       AND source.provider = ?2
                       AND source.source_format = ?3
                       AND source.machine_id = ?4
                       AND source.source_identity = ?5
                       AND route.provider = ?2
                       AND route.source_format = ?3
                       AND route.machine_id = ?4
                       AND route.alias_group_identity = ?6
                 )",
                params![
                    capture_source_id.to_string(),
                    key.provider.as_str(),
                    &key.source_format,
                    &key.machine_id,
                    &key.canonical_source_identity,
                    &locator,
                ],
                |row| row.get::<_, i64>(0),
            )? != 0;
            if !authorized {
                return Err(StoreError::NativePathSourceGenerationConflict);
            }
        }

        let mut insert = self.conn.prepare_cached(
            "INSERT OR IGNORE INTO native_path_source_generation_entities (
                 provider, source_format, machine_id, locator_identity,
                 generation_id, entity_kind, entity_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for (kind, id) in retained.iter() {
            insert.execute(params![
                key.provider.as_str(),
                &key.source_format,
                &key.machine_id,
                &locator_identity,
                &key.generation_id,
                kind,
                id.to_string(),
            ])?;
        }
        Ok(())
    }

    pub(crate) fn prepare_source_generation_retirement_page_tx(
        &self,
        key: &NativePathSourceGenerationKey,
        after: Option<&NativePathSourceEntityFrontier>,
        limit: usize,
    ) -> Result<NativePathSourceRetirementPreparation> {
        let state = match self.source_generation_retirement_request(key, after)? {
            NativePathSourceRetirementRequest::Replay(page) => {
                return Ok(NativePathSourceRetirementPreparation::Replay(page));
            }
            NativePathSourceRetirementRequest::Work { state } => state,
        };
        let locator_identity = locator_storage_key(&key.locator_identity);
        if state == "staging" {
            let changed = self.conn.execute(
                "UPDATE native_path_source_generations SET state = 'retiring'
                 WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
                   AND locator_identity = ?4 AND generation_id = ?5
                   AND state = 'staging'",
                params![
                    key.provider.as_str(),
                    &key.source_format,
                    &key.machine_id,
                    &locator_identity,
                    &key.generation_id,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::NativePathSourceGenerationConflict);
            }
        }
        self.source_generation_retirement_work(key, after, limit)
    }

    pub(crate) fn preview_source_generation_retirement_page_tx(
        &self,
        key: &NativePathSourceGenerationKey,
        after: Option<&NativePathSourceEntityFrontier>,
        limit: usize,
    ) -> Result<NativePathSourceRetirementPage> {
        match self.source_generation_retirement_request(key, after)? {
            NativePathSourceRetirementRequest::Replay(page) => Ok(page),
            NativePathSourceRetirementRequest::Work { .. } => {
                let NativePathSourceRetirementPreparation::Work {
                    candidates,
                    next_after,
                    done,
                } = self.source_generation_retirement_work(key, after, limit)?
                else {
                    return Err(StoreError::NativePathSourceGenerationConflict);
                };
                Ok(NativePathSourceRetirementPage {
                    next_after,
                    done,
                    inspected: candidates.len(),
                    retired: candidates
                        .iter()
                        .filter(|candidate| !candidate.retained)
                        .count(),
                })
            }
        }
    }

    fn source_generation_retirement_request(
        &self,
        key: &NativePathSourceGenerationKey,
        after: Option<&NativePathSourceEntityFrontier>,
    ) -> Result<NativePathSourceRetirementRequest> {
        key.bound_value_bytes()?;
        self.authorized_generation_alias_group(key)?;
        let state = self.generation_state(key)?;
        if state.state == "complete" && frontiers_equal(after, state.last_request.as_ref()) {
            return Ok(NativePathSourceRetirementRequest::Replay(
                NativePathSourceRetirementPage {
                    next_after: state.last_next,
                    done: true,
                    inspected: state.last_inspected,
                    retired: state.last_retired,
                },
            ));
        }
        if state.state == "complete" {
            return Err(StoreError::NativePathSourceGenerationConflict);
        }
        if state.state == "retiring" && frontiers_equal(after, state.last_request.as_ref()) {
            return Ok(NativePathSourceRetirementRequest::Replay(
                NativePathSourceRetirementPage {
                    next_after: state.last_next,
                    done: state.last_done,
                    inspected: state.last_inspected,
                    retired: state.last_retired,
                },
            ));
        }
        if !frontiers_equal(after, state.frontier.as_ref()) {
            return Err(StoreError::NativePathSourceGenerationConflict);
        }
        if state.state != "staging" && state.state != "retiring" {
            return Err(StoreError::NativePathSourceGenerationConflict);
        }
        Ok(NativePathSourceRetirementRequest::Work { state: state.state })
    }

    fn source_generation_retirement_work(
        &self,
        key: &NativePathSourceGenerationKey,
        after: Option<&NativePathSourceEntityFrontier>,
        limit: usize,
    ) -> Result<NativePathSourceRetirementPreparation> {
        if limit == 0 {
            return Err(StoreError::NativePathSourceGenerationConflict);
        }
        let locator_identity = locator_storage_key(&key.locator_identity);
        let mut remaining = limit;
        let mut candidates = Vec::new();
        let mut next_after = after.cloned();

        for kind in NativePathSourceEntityKind::RETIREMENT_ORDER {
            if after.is_some_and(|frontier| frontier.kind.order() > kind.order()) {
                continue;
            }
            let (table, owner_column, keyset_index) = kind.retirement_storage();
            let mut owner_after = String::new();
            let mut entity_after = String::new();
            let mut current_owner = None;

            if let Some(frontier) = after.filter(|frontier| frontier.kind == kind) {
                entity_after = frontier.id.to_string();
                let recover_owner = format!(
                    "SELECT entity.{owner_column}
                     FROM {table} entity
                     WHERE entity.id = ?6
                       AND EXISTS(
                           SELECT 1
                           FROM native_path_source_generation_entities staged
                           WHERE staged.provider = ?1
                             AND staged.source_format = ?2
                             AND staged.machine_id = ?3
                             AND staged.locator_identity = ?4
                             AND staged.generation_id = ?5
                             AND staged.entity_kind = 'capture_source'
                             AND staged.entity_id = entity.{owner_column}
                       )"
                );
                current_owner = self
                    .conn
                    .prepare_cached(&recover_owner)?
                    .query_row(
                        params![
                            key.provider.as_str(),
                            &key.source_format,
                            &key.machine_id,
                            &locator_identity,
                            &key.generation_id,
                            &entity_after,
                        ],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if current_owner.is_none() {
                    return Err(StoreError::NativePathSourceGenerationConflict);
                }
            }

            loop {
                if current_owner.is_none() {
                    let next_owner = format!(
                        "SELECT staged.entity_id
                         FROM native_path_source_generation_entities staged
                         WHERE staged.provider = ?1
                           AND staged.source_format = ?2
                           AND staged.machine_id = ?3
                           AND staged.locator_identity = ?4
                           AND staged.generation_id = ?5
                           AND staged.entity_kind = 'capture_source'
                           AND staged.entity_id > ?6
                           AND EXISTS(
                               SELECT 1
                               FROM {table} entity
                               WHERE entity.{owner_column} = staged.entity_id
                                 AND entity.deleted_at_ms IS NULL
                           )
                         ORDER BY staged.entity_id
                         LIMIT 1"
                    );
                    current_owner = self
                        .conn
                        .prepare_cached(&next_owner)?
                        .query_row(
                            params![
                                key.provider.as_str(),
                                &key.source_format,
                                &key.machine_id,
                                &locator_identity,
                                &key.generation_id,
                                &owner_after,
                            ],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()?;
                    let Some(_) = current_owner else {
                        break;
                    };
                    entity_after.clear();
                }

                let owner = current_owner
                    .as_deref()
                    .ok_or(StoreError::NativePathSourceGenerationConflict)?;
                let query = format!(
                    "SELECT entity.id,
                            EXISTS(
                                SELECT 1
                                FROM native_path_source_generation_entities kept
                                WHERE kept.provider = ?1
                                  AND kept.source_format = ?2
                                  AND kept.machine_id = ?3
                                  AND kept.locator_identity = ?4
                                  AND kept.generation_id = ?5
                                  AND kept.entity_kind = ?6
                                  AND kept.entity_id = entity.id
                            )
                     FROM {table} entity INDEXED BY {keyset_index}
                     WHERE entity.{owner_column} = ?7
                       AND entity.deleted_at_ms IS NULL
                       AND entity.id > ?8
                     ORDER BY entity.id
                     LIMIT ?9"
                );
                let mut page = self
                    .conn
                    .prepare_cached(&query)?
                    .query_map(
                        params![
                            key.provider.as_str(),
                            &key.source_format,
                            &key.machine_id,
                            &locator_identity,
                            &key.generation_id,
                            kind.as_str(),
                            owner,
                            &entity_after,
                            i64::try_from(remaining.saturating_add(1))
                                .map_err(|_| StoreError::NativePathSourceGenerationConflict)?,
                        ],
                        |row| {
                            let id = row.get::<_, String>(0)?;
                            let id = Uuid::parse_str(&id).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    0,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?;
                            Ok(NativePathSourceRetirementCandidate {
                                kind,
                                id,
                                retained: row.get::<_, i64>(1)? != 0,
                            })
                        },
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let has_more = page.len() > remaining;
                if has_more {
                    page.pop();
                }
                remaining = remaining.saturating_sub(page.len());
                if let Some(last) = page.last() {
                    next_after = Some(NativePathSourceEntityFrontier { kind, id: last.id });
                }
                candidates.extend(page);
                if has_more {
                    return Ok(NativePathSourceRetirementPreparation::Work {
                        candidates,
                        next_after,
                        done: false,
                    });
                }
                owner_after = owner.to_owned();
                current_owner = None;
            }
        }

        Ok(NativePathSourceRetirementPreparation::Work {
            candidates,
            next_after,
            done: true,
        })
    }

    pub(crate) fn finish_source_generation_retirement_page_tx(
        &self,
        key: &NativePathSourceGenerationKey,
        request: Option<&NativePathSourceEntityFrontier>,
        page: &NativePathSourceRetirementPage,
    ) -> Result<()> {
        let locator_identity = locator_storage_key(&key.locator_identity);
        let request_kind = request.map(|value| value.kind.as_str());
        let request_id = request.map(|value| value.id.to_string());
        let next_kind = page.next_after.as_ref().map(|value| value.kind.as_str());
        let next_id = page.next_after.as_ref().map(|value| value.id.to_string());
        let changed = self.conn.execute(
            "UPDATE native_path_source_generations
             SET state = CASE WHEN ?6 THEN 'complete' ELSE 'retiring' END,
                 frontier_kind = ?7, frontier_id = ?8,
                 last_request_kind = ?9, last_request_id = ?10,
                 last_next_kind = ?7, last_next_id = ?8,
                 last_done = ?6, last_inspected = ?11, last_retired = ?12
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND locator_identity = ?4 AND generation_id = ?5
               AND state IN ('staging', 'retiring')",
            params![
                key.provider.as_str(),
                &key.source_format,
                &key.machine_id,
                &locator_identity,
                &key.generation_id,
                page.done,
                next_kind,
                next_id,
                request_kind,
                request_id,
                i64::try_from(page.inspected)
                    .map_err(|_| StoreError::NativePathSourceGenerationConflict)?,
                i64::try_from(page.retired)
                    .map_err(|_| StoreError::NativePathSourceGenerationConflict)?,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::NativePathSourceGenerationConflict);
        }
        Ok(())
    }

    fn authorized_generation_alias_group(
        &self,
        key: &NativePathSourceGenerationKey,
    ) -> Result<String> {
        let locator_identity = locator_storage_key(&key.locator_identity);
        self.conn
            .query_row(
                "SELECT locator.alias_group_identity
                 FROM provider_source_locators locator
                 WHERE locator.provider = ?1 AND locator.source_format = ?2
                   AND locator.machine_id = ?3 AND locator.locator_identity = ?4
                   AND locator.cursor_stream = ?5
                   AND locator.canonical_source_identity = ?6
                   AND locator.source_revision = ?7 AND locator.is_current = 1
                   AND EXISTS(
                       SELECT 1
                       FROM capture_source_provider_routes route
                       JOIN capture_sources source
                         ON source.id = route.capture_source_id
                       WHERE route.provider = locator.provider
                         AND route.source_format = locator.source_format
                         AND route.machine_id = locator.machine_id
                         AND route.alias_group_identity = locator.alias_group_identity
                         AND source.provider = locator.provider
                         AND source.source_format = locator.source_format
                         AND source.machine_id = locator.machine_id
                         AND source.source_identity = locator.canonical_source_identity
                   )",
                params![
                    key.provider.as_str(),
                    &key.source_format,
                    &key.machine_id,
                    &locator_identity,
                    &key.cursor_stream,
                    &key.canonical_source_identity,
                    &key.source_revision,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StoreError::NativePathSourceGenerationConflict)
    }

    fn generation_state(&self, key: &NativePathSourceGenerationKey) -> Result<GenerationState> {
        let locator_identity = locator_storage_key(&key.locator_identity);
        self.conn
            .query_row(
                "SELECT state, frontier_kind, frontier_id,
                        last_request_kind, last_request_id,
                        last_next_kind, last_next_id,
                        last_done, last_inspected, last_retired
                 FROM native_path_source_generations
                 WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
                   AND locator_identity = ?4 AND generation_id = ?5
                   AND cursor_stream = ?6 AND canonical_source_identity = ?7
                   AND source_revision = ?8",
                params![
                    key.provider.as_str(),
                    &key.source_format,
                    &key.machine_id,
                    &locator_identity,
                    &key.generation_id,
                    &key.cursor_stream,
                    &key.canonical_source_identity,
                    &key.source_revision,
                ],
                |row| {
                    Ok(GenerationState {
                        state: row.get(0)?,
                        frontier: frontier_from_columns(row.get(1)?, row.get(2)?)?,
                        last_request: frontier_from_columns(row.get(3)?, row.get(4)?)?,
                        last_next: frontier_from_columns(row.get(5)?, row.get(6)?)?,
                        last_done: row.get::<_, i64>(7)? != 0,
                        last_inspected: usize::try_from(row.get::<_, i64>(8)?)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, 0))?,
                        last_retired: usize::try_from(row.get::<_, i64>(9)?)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, 0))?,
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::NativePathSourceGenerationConflict)
    }
}

fn frontier_from_columns(
    kind: Option<String>,
    id: Option<String>,
) -> rusqlite::Result<Option<NativePathSourceEntityFrontier>> {
    match (kind, id) {
        (None, None) => Ok(None),
        (Some(kind), Some(id)) => Ok(Some(NativePathSourceEntityFrontier {
            kind: NativePathSourceEntityKind::from_str(&kind).ok_or_else(|| {
                rusqlite::Error::InvalidColumnType(
                    0,
                    "frontier_kind".to_owned(),
                    rusqlite::types::Type::Text,
                )
            })?,
            id: Uuid::parse_str(&id).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        })),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn frontiers_equal(
    left: Option<&NativePathSourceEntityFrontier>,
    right: Option<&NativePathSourceEntityFrontier>,
) -> bool {
    left == right
}
