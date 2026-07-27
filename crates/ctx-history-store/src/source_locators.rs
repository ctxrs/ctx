use std::{
    fmt,
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ctx_history_core::CaptureProvider;
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::connection::{parse_text_enum, parse_uuid};
use crate::{Result, Store, StoreError};

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderSourceLocatorObservation {
    pub provider: CaptureProvider,
    pub source_format: String,
    pub machine_id: String,
    /// Opaque, platform-aware identity of the exact provider source path.
    pub locator_identity: String,
    /// Path-scoped cursor stream proposed by the current adapter.
    pub cursor_stream: String,
    /// Root-scoped canonical identity proposed for a newly seen source.
    pub proposed_source_identity: String,
    /// Local-only exact locator. It is never part of a hosted projection.
    pub raw_source_path: Option<String>,
    /// Bounded provider observation fingerprint, excluding the locator path.
    pub source_revision: String,
    pub observed_at_ms: i64,
}

impl fmt::Debug for ProviderSourceLocatorObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderSourceLocatorObservation")
            .field("provider", &self.provider)
            .field("source_format", &self.source_format)
            .field("machine_id", &self.machine_id)
            .field("locator_identity_bytes", &self.locator_identity.len())
            .field("cursor_stream_bytes", &self.cursor_stream.len())
            .field(
                "proposed_source_identity_bytes",
                &self.proposed_source_identity.len(),
            )
            .field(
                "raw_source_path",
                &self.raw_source_path.as_ref().map(|_| "<local-path>"),
            )
            .field("source_revision_bytes", &self.source_revision.len())
            .field("observed_at_ms", &self.observed_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderSourceRouteBinding {
    provider: CaptureProvider,
    source_format: String,
    machine_id: String,
    canonical_source_identity: String,
    alias_group_identity: String,
}

impl fmt::Debug for ProviderSourceRouteBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderSourceRouteBinding")
            .field("provider", &self.provider)
            .field("source_format", &self.source_format)
            .field("machine_id", &self.machine_id)
            .field(
                "canonical_source_identity_bytes",
                &self.canonical_source_identity.len(),
            )
            .field(
                "alias_group_identity_bytes",
                &self.alias_group_identity.len(),
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderSourceLocatorResolution {
    pub canonical_source_identity: String,
    pub relocated: bool,
    route_binding: ProviderSourceRouteBinding,
}

impl ProviderSourceLocatorResolution {
    pub fn route_binding(&self) -> ProviderSourceRouteBinding {
        self.route_binding.clone()
    }
}

impl fmt::Debug for ProviderSourceLocatorResolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderSourceLocatorResolution")
            .field(
                "canonical_source_identity_bytes",
                &self.canonical_source_identity.len(),
            )
            .field("relocated", &self.relocated)
            .field("route_binding", &self.route_binding)
            .finish()
    }
}

/// The exact current local provider path authorized for one event.
///
/// This is route authorization, not proof that source bytes are unchanged.
/// A content broker must combine it with the event's typed verified-content
/// locator, dispatch to that provider-family resolver, and require the resolver
/// to verify its provider-specific source identity/snapshot and addressed-record
/// digest after opening. It must not reuse the capture source's historical path
/// or interpret `source_revision` as a universal file digest.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthorizedSourceRoute {
    event_id: Uuid,
    capture_source_id: Uuid,
    provider: CaptureProvider,
    source_format: String,
    machine_id: String,
    canonical_source_identity: String,
    raw_source_path: PathBuf,
    source_revision: String,
}

impl AuthorizedSourceRoute {
    pub const fn event_id(&self) -> Uuid {
        self.event_id
    }

    pub const fn capture_source_id(&self) -> Uuid {
        self.capture_source_id
    }

    pub const fn provider(&self) -> CaptureProvider {
        self.provider
    }

    pub fn source_format(&self) -> &str {
        &self.source_format
    }

    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }

    pub fn canonical_source_identity(&self) -> &str {
        &self.canonical_source_identity
    }

    pub fn path(&self) -> &Path {
        &self.raw_source_path
    }

    /// Opaque revision evidence used by provider-source reconciliation.
    ///
    /// Its construction differs by provider and it is not a portable
    /// `SourceSnapshot`. A broker may pass it only to a provider-specific
    /// verifier that understands that provider's revision contract.
    pub fn source_revision(&self) -> &str {
        &self.source_revision
    }
}

impl fmt::Debug for AuthorizedSourceRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizedSourceRoute")
            .field("event_id", &self.event_id)
            .field("capture_source_id", &self.capture_source_id)
            .field("provider", &self.provider)
            .field("source_format", &self.source_format)
            .field("machine_id", &self.machine_id)
            .field(
                "canonical_source_identity_bytes",
                &self.canonical_source_identity.len(),
            )
            .field("raw_source_path", &"<local-path>")
            .field("source_revision_bytes", &self.source_revision.len())
            .finish()
    }
}

#[derive(Debug)]
struct LocatorRow {
    locator_identity: String,
    canonical_source_identity: String,
    alias_group_identity: String,
    raw_source_path: Option<String>,
    source_revision: String,
    is_current: bool,
    is_relocation_alias: bool,
}

impl Store {
    /// Reconciles one exact provider source without scanning for alternatives.
    ///
    /// A new locator aliases an existing source only when the provider's full
    /// bounded revision fingerprint has one unique current match and the prior
    /// exact locator is gone. Known historical aliases fail closed if another
    /// current locator still exists or the revision no longer matches.
    pub fn reconcile_provider_source_locator(
        &self,
        observation: &ProviderSourceLocatorObservation,
    ) -> Result<ProviderSourceLocatorResolution> {
        self.begin_immediate_batch()?;
        let result = self.reconcile_provider_source_locator_tx(observation);
        match result {
            Ok(resolution) => match self.commit_batch() {
                Ok(()) => Ok(resolution),
                Err(error) => {
                    let _ = self.rollback_batch();
                    Err(error)
                }
            },
            Err(error) => {
                self.rollback_batch()?;
                Err(error)
            }
        }
    }

    fn reconcile_provider_source_locator_tx(
        &self,
        observation: &ProviderSourceLocatorObservation,
    ) -> Result<ProviderSourceLocatorResolution> {
        if let Some(exact) = self.provider_source_locator_by_identity(observation)? {
            if exact.is_current {
                self.update_provider_source_locator(
                    &exact,
                    observation,
                    true,
                    exact.is_relocation_alias,
                )?;
                return Ok(resolution(observation, &exact, exact.is_relocation_alias));
            }
            let current = self
                .current_provider_source_locator(observation, &exact.alias_group_identity)?
                .ok_or_else(|| StoreError::ProviderSourceRelocationAmbiguous {
                    provider: observation.provider.as_str().to_owned(),
                    source_format: observation.source_format.clone(),
                })?;
            if current.source_revision != observation.source_revision
                || !exact_locator_is_missing(current.raw_source_path.as_deref())
            {
                return Err(StoreError::ProviderSourceRelocationAmbiguous {
                    provider: observation.provider.as_str().to_owned(),
                    source_format: observation.source_format.clone(),
                });
            }
            self.set_provider_source_current(observation, &current, &exact)?;
            self.update_provider_source_locator(&exact, observation, true, true)?;
            return Ok(resolution(observation, &exact, true));
        }

        let candidates = self.provider_source_revision_candidates(observation)?;
        let relocation = match candidates.as_slice() {
            [candidate] if exact_locator_is_missing(candidate.raw_source_path.as_deref()) => {
                Some(candidate)
            }
            _ => None,
        };
        if let Some(candidate) = relocation {
            self.conn.execute(
                "UPDATE provider_source_locators SET is_current = 0
                 WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
                   AND alias_group_identity = ?4 AND is_current = 1",
                params![
                    observation.provider.as_str(),
                    observation.source_format,
                    observation.machine_id,
                    candidate.alias_group_identity,
                ],
            )?;
            self.insert_provider_source_locator(
                observation,
                &candidate.canonical_source_identity,
                &candidate.alias_group_identity,
                true,
            )?;
            return Ok(ProviderSourceLocatorResolution {
                canonical_source_identity: candidate.canonical_source_identity.clone(),
                relocated: true,
                route_binding: route_binding(
                    observation,
                    &candidate.canonical_source_identity,
                    &candidate.alias_group_identity,
                ),
            });
        }

        self.insert_provider_source_locator(
            observation,
            &observation.proposed_source_identity,
            &locator_storage_key(&observation.locator_identity),
            false,
        )?;
        Ok(ProviderSourceLocatorResolution {
            canonical_source_identity: observation.proposed_source_identity.clone(),
            relocated: false,
            route_binding: route_binding(
                observation,
                &observation.proposed_source_identity,
                &locator_storage_key(&observation.locator_identity),
            ),
        })
    }

    /// Binds one persisted capture source to the exact physical alias group
    /// selected during provider-source reconciliation. The binding is local
    /// authorization state and does not affect the semantic projection journal.
    pub fn bind_capture_source_provider_route(
        &self,
        capture_source_id: Uuid,
        binding: &ProviderSourceRouteBinding,
    ) -> Result<()> {
        self.with_import_batch_write(|| {
            self.bind_capture_source_provider_route_inner(capture_source_id, binding)
        })
    }

    fn bind_capture_source_provider_route_inner(
        &self,
        capture_source_id: Uuid,
        binding: &ProviderSourceRouteBinding,
    ) -> Result<()> {
        let expected = self
            .conn
            .query_row(
                "SELECT provider, source_format, machine_id, source_identity
                 FROM capture_sources WHERE id = ?1",
                [capture_source_id.to_string()],
                |row| {
                    Ok((
                        parse_text_enum::<CaptureProvider>(row.get::<_, String>(0)?)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let (matches_capture_source, matches_except_machine) = expected.map_or(
            (false, false),
            |(provider, source_format, machine_id, canonical_source_identity)| {
                let stable_fields_match = provider == binding.provider
                    && source_format.as_deref() == Some(binding.source_format.as_str())
                    && canonical_source_identity.as_deref()
                        == Some(binding.canonical_source_identity.as_str());
                (
                    stable_fields_match && machine_id == binding.machine_id,
                    stable_fields_match,
                )
            },
        );
        let locator_exists = self.conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM provider_source_locators
                 WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
                   AND canonical_source_identity = ?4 AND alias_group_identity = ?5
             )",
            params![
                binding.provider.as_str(),
                binding.source_format,
                binding.machine_id,
                binding.canonical_source_identity,
                binding.alias_group_identity,
            ],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !locator_exists {
            return Err(StoreError::CaptureSourceProviderRouteConflict { capture_source_id });
        }
        if !matches_capture_source {
            // `machine_id` is operator-configurable. A later import may rename
            // the same machine while retaining the exact path and provider
            // revision. Preserve the already-authorized route in that one
            // byte-equivalent case; all other rebinding attempts fail closed.
            let equivalent = self.equivalent_current_provider_route(capture_source_id, binding)?;
            if matches_except_machine && equivalent {
                return Ok(());
            }
            return Err(StoreError::CaptureSourceProviderRouteConflict { capture_source_id });
        }

        self.conn.execute(
            "INSERT INTO capture_source_provider_routes
             (capture_source_id, provider, source_format, machine_id, alias_group_identity)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(capture_source_id) DO NOTHING",
            params![
                capture_source_id.to_string(),
                binding.provider.as_str(),
                binding.source_format,
                binding.machine_id,
                binding.alias_group_identity,
            ],
        )?;
        let persisted = self.conn.query_row(
            "SELECT provider, source_format, machine_id, alias_group_identity
             FROM capture_source_provider_routes WHERE capture_source_id = ?1",
            [capture_source_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )?;
        let proposed = (
            binding.provider.as_str().to_owned(),
            binding.source_format.clone(),
            binding.machine_id.clone(),
            binding.alias_group_identity.clone(),
        );
        if persisted != proposed {
            if self.equivalent_current_provider_route(capture_source_id, binding)? {
                self.conn.execute(
                    "UPDATE capture_source_provider_routes
                     SET provider = ?2, source_format = ?3, machine_id = ?4,
                         alias_group_identity = ?5
                     WHERE capture_source_id = ?1",
                    params![
                        capture_source_id.to_string(),
                        binding.provider.as_str(),
                        binding.source_format,
                        binding.machine_id,
                        binding.alias_group_identity,
                    ],
                )?;
                return Ok(());
            }
            return Err(StoreError::CaptureSourceProviderRouteConflict { capture_source_id });
        }
        Ok(())
    }

    fn equivalent_current_provider_route(
        &self,
        capture_source_id: Uuid,
        proposed: &ProviderSourceRouteBinding,
    ) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM capture_source_provider_routes bound
                 JOIN provider_source_locators current
                   ON current.provider = bound.provider
                  AND current.source_format = bound.source_format
                  AND current.machine_id = bound.machine_id
                  AND current.alias_group_identity = bound.alias_group_identity
                  AND current.is_current = 1
                 JOIN provider_source_locators candidate
                   ON candidate.provider = ?2
                  AND candidate.source_format = ?3
                  AND candidate.machine_id = ?4
                  AND candidate.alias_group_identity = ?5
                  AND candidate.is_current = 1
                 WHERE bound.capture_source_id = ?1
                   AND current.canonical_source_identity = ?6
                   AND candidate.canonical_source_identity = ?6
                   AND current.raw_source_path IS NOT NULL
                   AND current.raw_source_path = candidate.raw_source_path
                   AND current.source_revision = candidate.source_revision
             )",
            params![
                capture_source_id.to_string(),
                proposed.provider.as_str(),
                proposed.source_format,
                proposed.machine_id,
                proposed.alias_group_identity,
                proposed.canonical_source_identity,
            ],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    /// Resolves the one current physical provider source authorized for an
    /// event. Historical capture paths are deliberately never consulted.
    pub fn authorized_source_route_for_event(
        &self,
        event_id: Uuid,
    ) -> Result<AuthorizedSourceRoute> {
        let mut statement = self.conn.prepare(
            "SELECT e.id, cs.id, cs.provider, cs.source_format, cs.machine_id,
                    cs.source_identity, locator.raw_source_path, locator.source_revision
             FROM events e
             JOIN capture_sources cs ON cs.id = e.capture_source_id
             JOIN capture_source_provider_routes route ON route.capture_source_id = cs.id
             JOIN provider_source_locators locator
               ON locator.provider = route.provider
              AND locator.source_format = route.source_format
              AND locator.machine_id = route.machine_id
              AND locator.alias_group_identity = route.alias_group_identity
              AND locator.is_current = 1
             WHERE e.id = ?1
               AND route.provider = cs.provider
               AND route.source_format = cs.source_format
               AND route.machine_id = cs.machine_id
               AND locator.canonical_source_identity = cs.source_identity
               AND locator.raw_source_path IS NOT NULL
               AND locator.raw_source_path <> ''
             ORDER BY locator.locator_identity
             LIMIT 2",
        )?;
        let rows = statement.query_map([event_id.to_string()], authorized_source_route_row)?;
        let mut routes = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        match routes.len() {
            0 => Err(StoreError::AuthorizedSourceRouteUnavailable { event_id }),
            1 => Ok(routes.pop().expect("one route was counted")),
            _ => Err(StoreError::AuthorizedSourceRouteAmbiguous { event_id }),
        }
    }

    fn provider_source_locator_by_identity(
        &self,
        observation: &ProviderSourceLocatorObservation,
    ) -> Result<Option<LocatorRow>> {
        let locator_identity = locator_storage_key(&observation.locator_identity);
        self.conn
            .query_row(
                "SELECT locator_identity, canonical_source_identity, alias_group_identity,
                        raw_source_path, source_revision, is_current, is_relocation_alias
                 FROM provider_source_locators
                 WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
                   AND locator_identity = ?4",
                params![
                    observation.provider.as_str(),
                    observation.source_format,
                    observation.machine_id,
                    locator_identity,
                ],
                locator_row,
            )
            .optional()
            .map_err(Into::into)
    }

    fn current_provider_source_locator(
        &self,
        observation: &ProviderSourceLocatorObservation,
        alias_group_identity: &str,
    ) -> Result<Option<LocatorRow>> {
        self.conn
            .query_row(
                "SELECT locator_identity, canonical_source_identity, alias_group_identity,
                        raw_source_path, source_revision, is_current, is_relocation_alias
                 FROM provider_source_locators
                 WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
                   AND alias_group_identity = ?4 AND is_current = 1",
                params![
                    observation.provider.as_str(),
                    observation.source_format,
                    observation.machine_id,
                    alias_group_identity,
                ],
                locator_row,
            )
            .optional()
            .map_err(Into::into)
    }

    fn provider_source_revision_candidates(
        &self,
        observation: &ProviderSourceLocatorObservation,
    ) -> Result<Vec<LocatorRow>> {
        let mut statement = self.conn.prepare(
            "SELECT locator_identity, canonical_source_identity, alias_group_identity,
                    raw_source_path, source_revision, is_current, is_relocation_alias
             FROM provider_source_locators
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND source_revision = ?4 AND is_current = 1
             ORDER BY canonical_source_identity, alias_group_identity LIMIT 3",
        )?;
        let rows = statement.query_map(
            params![
                observation.provider.as_str(),
                observation.source_format,
                observation.machine_id,
                observation.source_revision,
            ],
            locator_row,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn insert_provider_source_locator(
        &self,
        observation: &ProviderSourceLocatorObservation,
        canonical_source_identity: &str,
        alias_group_identity: &str,
        is_relocation_alias: bool,
    ) -> Result<()> {
        let locator_identity = locator_storage_key(&observation.locator_identity);
        self.conn.execute(
            "INSERT INTO provider_source_locators
             (provider, source_format, machine_id, locator_identity, cursor_stream,
              canonical_source_identity, alias_group_identity, raw_source_path,
              source_revision, is_current, is_relocation_alias, observed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11)",
            params![
                observation.provider.as_str(),
                observation.source_format,
                observation.machine_id,
                locator_identity,
                observation.cursor_stream,
                canonical_source_identity,
                alias_group_identity,
                observation.raw_source_path,
                observation.source_revision,
                i64::from(is_relocation_alias),
                observation.observed_at_ms,
            ],
        )?;
        Ok(())
    }

    fn update_provider_source_locator(
        &self,
        row: &LocatorRow,
        observation: &ProviderSourceLocatorObservation,
        is_current: bool,
        is_relocation_alias: bool,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE provider_source_locators
             SET cursor_stream = ?1, raw_source_path = ?2, source_revision = ?3,
                 is_current = ?4, is_relocation_alias = ?5, observed_at_ms = ?6
             WHERE provider = ?7 AND source_format = ?8 AND machine_id = ?9
               AND locator_identity = ?10 AND alias_group_identity = ?11",
            params![
                observation.cursor_stream,
                observation.raw_source_path,
                observation.source_revision,
                i64::from(is_current),
                i64::from(is_relocation_alias),
                observation.observed_at_ms,
                observation.provider.as_str(),
                observation.source_format,
                observation.machine_id,
                row.locator_identity,
                row.alias_group_identity,
            ],
        )?;
        Ok(())
    }

    fn set_provider_source_current(
        &self,
        observation: &ProviderSourceLocatorObservation,
        old_current: &LocatorRow,
        new_current: &LocatorRow,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE provider_source_locators SET is_current = 0
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND locator_identity = ?4 AND alias_group_identity = ?5",
            params![
                observation.provider.as_str(),
                observation.source_format,
                observation.machine_id,
                old_current.locator_identity,
                old_current.alias_group_identity,
            ],
        )?;
        self.conn.execute(
            "UPDATE provider_source_locators SET is_current = 1
             WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
               AND locator_identity = ?4 AND alias_group_identity = ?5",
            params![
                observation.provider.as_str(),
                observation.source_format,
                observation.machine_id,
                new_current.locator_identity,
                new_current.alias_group_identity,
            ],
        )?;
        Ok(())
    }
}

fn locator_storage_key(identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-provider-locator-storage-v1\0");
    hasher.update((identity.len() as u64).to_be_bytes());
    hasher.update(identity.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

fn locator_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LocatorRow> {
    Ok(LocatorRow {
        locator_identity: row.get(0)?,
        canonical_source_identity: row.get(1)?,
        alias_group_identity: row.get(2)?,
        raw_source_path: row.get(3)?,
        source_revision: row.get(4)?,
        is_current: row.get::<_, i64>(5)? != 0,
        is_relocation_alias: row.get::<_, i64>(6)? != 0,
    })
}

fn resolution(
    observation: &ProviderSourceLocatorObservation,
    row: &LocatorRow,
    relocated: bool,
) -> ProviderSourceLocatorResolution {
    ProviderSourceLocatorResolution {
        canonical_source_identity: row.canonical_source_identity.clone(),
        relocated,
        route_binding: route_binding(
            observation,
            &row.canonical_source_identity,
            &row.alias_group_identity,
        ),
    }
}

fn route_binding(
    observation: &ProviderSourceLocatorObservation,
    canonical_source_identity: &str,
    alias_group_identity: &str,
) -> ProviderSourceRouteBinding {
    ProviderSourceRouteBinding {
        provider: observation.provider,
        source_format: observation.source_format.clone(),
        machine_id: observation.machine_id.clone(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        alias_group_identity: alias_group_identity.to_owned(),
    }
}

fn authorized_source_route_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuthorizedSourceRoute> {
    Ok(AuthorizedSourceRoute {
        event_id: parse_uuid(row.get::<_, String>(0)?)?,
        capture_source_id: parse_uuid(row.get::<_, String>(1)?)?,
        provider: parse_text_enum::<CaptureProvider>(row.get::<_, String>(2)?)?,
        source_format: row.get(3)?,
        machine_id: row.get(4)?,
        canonical_source_identity: row.get(5)?,
        raw_source_path: PathBuf::from(row.get::<_, String>(6)?),
        source_revision: row.get(7)?,
    })
}

fn exact_locator_is_missing(path: Option<&str>) -> bool {
    let Some(path) = path.filter(|path| !path.is_empty()) else {
        return false;
    };
    matches!(
        std::fs::symlink_metadata(Path::new(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

#[cfg(test)]
mod tests;
