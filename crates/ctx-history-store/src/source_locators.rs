use std::{fmt, path::Path};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ctx_history_core::CaptureProvider;
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSourceLocatorResolution {
    pub canonical_source_identity: String,
    pub relocated: bool,
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
                return Ok(resolution(&exact, exact.is_relocation_alias));
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
            return Ok(resolution(&exact, true));
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
        })
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

fn resolution(row: &LocatorRow, relocated: bool) -> ProviderSourceLocatorResolution {
    ProviderSourceLocatorResolution {
        canonical_source_identity: row.canonical_source_identity.clone(),
        relocated,
    }
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
mod tests {
    use super::*;

    fn observation(
        path: &Path,
        locator: &str,
        cursor: &str,
        revision: &str,
    ) -> ProviderSourceLocatorObservation {
        ProviderSourceLocatorObservation {
            provider: CaptureProvider::Codex,
            source_format: "codex_session_jsonl".to_owned(),
            machine_id: "machine-1".to_owned(),
            locator_identity: locator.to_owned(),
            cursor_stream: cursor.to_owned(),
            proposed_source_identity: format!("identity-{locator}"),
            raw_source_path: Some(path.to_string_lossy().into_owned()),
            source_revision: revision.to_owned(),
            observed_at_ms: 1,
        }
    }

    #[test]
    fn changed_source_at_a_new_locator_never_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let old_path = temp.path().join("old.jsonl");
        let new_path = temp.path().join("new.jsonl");
        std::fs::write(&old_path, b"old source").unwrap();
        let store = Store::open(temp.path().join("ctx.db")).unwrap();
        let first = store
            .reconcile_provider_source_locator(&observation(
                &old_path,
                "old-locator",
                "old-cursor",
                "revision-1",
            ))
            .unwrap();
        std::fs::remove_file(&old_path).unwrap();
        std::fs::write(&new_path, b"rewritten source").unwrap();
        let rewritten = store
            .reconcile_provider_source_locator(&observation(
                &new_path,
                "new-locator",
                "new-cursor",
                "revision-2",
            ))
            .unwrap();

        assert!(!rewritten.relocated);
        assert_ne!(
            rewritten.canonical_source_identity,
            first.canonical_source_identity
        );
    }

    fn assert_shared_canonical_source_allows_multiple_current_physical_sources() {
        let temp = tempfile::tempdir().unwrap();
        let first_path = temp.path().join("first.jsonl");
        let second_path = temp.path().join("second.jsonl");
        let moved_path = temp.path().join("moved-first.jsonl");
        std::fs::write(&first_path, b"first source").unwrap();
        std::fs::write(&second_path, b"second source").unwrap();
        let store = Store::open(temp.path().join("ctx.db")).unwrap();
        let mut first = observation(&first_path, "first", "cursor-first", "revision-first");
        first.proposed_source_identity = "shared-root-identity".to_owned();
        let mut second = observation(&second_path, "second", "cursor-second", "revision-second");
        second.proposed_source_identity = "shared-root-identity".to_owned();

        assert!(
            !store
                .reconcile_provider_source_locator(&first)
                .unwrap()
                .relocated
        );
        assert!(
            !store
                .reconcile_provider_source_locator(&second)
                .unwrap()
                .relocated
        );

        std::fs::rename(&first_path, &moved_path).unwrap();
        let mut moved = observation(
            &moved_path,
            "moved-first",
            "cursor-moved-first",
            "revision-first",
        );
        moved.proposed_source_identity = "new-root-identity".to_owned();
        let resolution = store.reconcile_provider_source_locator(&moved).unwrap();
        assert!(resolution.relocated);
        assert_eq!(resolution.canonical_source_identity, "shared-root-identity");

        let second_replay = store.reconcile_provider_source_locator(&second).unwrap();
        assert!(!second_replay.relocated);
        assert_eq!(
            second_replay.canonical_source_identity,
            "shared-root-identity"
        );
    }

    #[test]
    fn unique_missing_locator_reconciles_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("ctx.db");
        let old_path = temp.path().join("old.jsonl");
        let new_path = temp.path().join("new.jsonl");
        std::fs::write(&old_path, b"same provider source").unwrap();
        let store = Store::open(&database).unwrap();
        let first = store
            .reconcile_provider_source_locator(&observation(
                &old_path,
                "old-locator",
                "old-cursor",
                "revision-1",
            ))
            .unwrap();
        assert!(!first.relocated);
        std::fs::rename(&old_path, &new_path).unwrap();
        let moved = store
            .reconcile_provider_source_locator(&observation(
                &new_path,
                "new-locator",
                "new-cursor",
                "revision-1",
            ))
            .unwrap();
        assert!(moved.relocated);
        assert_eq!(moved.canonical_source_identity, "identity-old-locator");
        drop(store);

        let reopened = Store::open(&database).unwrap();
        let appended = reopened
            .reconcile_provider_source_locator(&observation(
                &new_path,
                "new-locator",
                "new-cursor",
                "revision-2",
            ))
            .unwrap();
        assert!(appended.relocated);
        assert_eq!(appended.canonical_source_identity, "identity-old-locator");

        std::fs::rename(&new_path, &old_path).unwrap();
        let moved_back = reopened
            .reconcile_provider_source_locator(&observation(
                &old_path,
                "old-locator",
                "old-cursor",
                "revision-2",
            ))
            .unwrap();
        assert!(moved_back.relocated);
        assert_eq!(moved_back.canonical_source_identity, "identity-old-locator");
    }

    #[test]
    fn identical_live_sources_never_alias_and_known_alias_collision_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let first_path = temp.path().join("first.jsonl");
        let second_path = temp.path().join("second.jsonl");
        let third_path = temp.path().join("third.jsonl");
        std::fs::write(&first_path, b"same").unwrap();
        let store = Store::open(temp.path().join("ctx.db")).unwrap();
        store
            .reconcile_provider_source_locator(&observation(
                &first_path,
                "first",
                "cursor-first",
                "revision-same",
            ))
            .unwrap();
        std::fs::rename(&first_path, &second_path).unwrap();
        let moved = store
            .reconcile_provider_source_locator(&observation(
                &second_path,
                "second",
                "cursor-second",
                "revision-same",
            ))
            .unwrap();
        assert!(moved.relocated);

        std::fs::write(&first_path, b"same").unwrap();
        let error = store
            .reconcile_provider_source_locator(&observation(
                &first_path,
                "first",
                "cursor-first",
                "revision-same",
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            StoreError::ProviderSourceRelocationAmbiguous { .. }
        ));

        std::fs::write(&third_path, b"same").unwrap();
        let third = store
            .reconcile_provider_source_locator(&observation(
                &third_path,
                "third",
                "cursor-third",
                "revision-same",
            ))
            .unwrap();
        assert!(!third.relocated, "multiple live sources must stay distinct");
        assert_shared_canonical_source_allows_multiple_current_physical_sources();
    }

    #[test]
    fn debug_output_never_contains_the_local_path() {
        let observation = observation(
            Path::new("/private/home/alice/provider/session.jsonl"),
            "private-locator",
            "private-cursor",
            "private-revision",
        );
        let debug = format!("{observation:?}");
        assert!(!debug.contains("/private/home/alice"));
        assert!(debug.contains("<local-path>"));
    }
}
