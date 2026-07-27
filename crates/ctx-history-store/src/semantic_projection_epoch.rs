use uuid::Uuid;

use crate::{Result, Store};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalSemanticProjectionVersion {
    pub store_identity: Uuid,
    pub mutation_epoch: u64,
}

impl Store {
    /// Durable identity and mutation version of the canonical semantic-lite-turn
    /// projection. Exact Store backups retain the identity; independently
    /// initialized Stores receive different identities.
    pub fn canonical_semantic_projection_version(
        &self,
    ) -> Result<CanonicalSemanticProjectionVersion> {
        let (store_identity, mutation_epoch) = self.conn.query_row(
            "SELECT store_identity, mutation_epoch
             FROM canonical_semantic_projection_state
             WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
        )?;
        Ok(CanonicalSemanticProjectionVersion {
            store_identity: Uuid::parse_str(&store_identity)?,
            mutation_epoch,
        })
    }

    pub fn canonical_semantic_projection_epoch(&self) -> Result<u64> {
        self.canonical_semantic_projection_version()
            .map(|version| version.mutation_epoch)
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::params;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn store_identity_survives_exact_backup_but_differs_for_independent_store() {
        let temp = tempdir().unwrap();
        let original_path = temp.path().join("original.db");
        let backup_path = temp.path().join("backup.db");
        let independent_path = temp.path().join("independent.db");
        let original = Store::open(&original_path).unwrap();
        let original_version = original.canonical_semantic_projection_version().unwrap();
        original
            .conn
            .execute("VACUUM INTO ?1", [backup_path.to_string_lossy().as_ref()])
            .unwrap();
        drop(original);

        let backup = Store::open(&backup_path).unwrap();
        assert_eq!(
            backup.canonical_semantic_projection_version().unwrap(),
            original_version
        );
        let independent = Store::open(&independent_path).unwrap();
        let independent_version = independent.canonical_semantic_projection_version().unwrap();
        assert_eq!(
            independent_version.mutation_epoch,
            original_version.mutation_epoch
        );
        assert_ne!(
            independent_version.store_identity,
            original_version.store_identity
        );
    }

    #[test]
    fn epoch_is_cross_connection_atomic_and_ignores_noop_updates() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("ctx.db");
        let observer = Store::open(&path).unwrap();
        let writer = Store::open(&path).unwrap();
        let initial_epoch = observer.canonical_semantic_projection_epoch().unwrap();
        let event_id = Uuid::new_v4();

        writer.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer
            .conn
            .execute(
                "INSERT INTO events
                 (id, seq, event_type, role, occurred_at_ms, payload_json)
                 VALUES (?1, 1, 'message', 'user', 1, ?2)",
                params![event_id.to_string(), r#"{"text":"uncommitted"}"#],
            )
            .unwrap();
        assert_eq!(
            observer.canonical_semantic_projection_epoch().unwrap(),
            initial_epoch
        );
        writer.conn.execute_batch("ROLLBACK").unwrap();

        writer
            .conn
            .execute(
                "INSERT INTO events
                 (id, seq, event_type, role, occurred_at_ms, payload_json)
                 VALUES (?1, 1, 'message', 'user', 1, ?2)",
                params![event_id.to_string(), r#"{"text":"committed"}"#],
            )
            .unwrap();
        let committed_epoch = observer.canonical_semantic_projection_epoch().unwrap();
        assert!(committed_epoch > initial_epoch);

        writer
            .conn
            .execute(
                "UPDATE events SET payload_json = payload_json WHERE id = ?1",
                [event_id.to_string()],
            )
            .unwrap();
        writer
            .conn
            .execute(
                "UPDATE events SET metadata_json = '{\"irrelevant\":true}' WHERE id = ?1",
                [event_id.to_string()],
            )
            .unwrap();
        assert_eq!(
            observer.canonical_semantic_projection_epoch().unwrap(),
            committed_epoch
        );

        writer
            .conn
            .execute(
                "UPDATE events SET visibility = 'reportable' WHERE id = ?1",
                [event_id.to_string()],
            )
            .unwrap();
        assert!(observer.canonical_semantic_projection_epoch().unwrap() > committed_epoch);
    }

    #[test]
    fn update_trigger_columns_match_executable_semantic_projection_reads() {
        use std::{
            collections::{BTreeMap, BTreeSet, HashMap},
            sync::{Arc, Mutex},
        };

        use rusqlite::hooks::{AuthAction, Authorization};

        let temp = tempdir().unwrap();
        let store = Store::open(temp.path().join("ctx.db")).unwrap();
        let reads = Arc::new(Mutex::new(BTreeSet::new()));
        let observed_reads = Arc::clone(&reads);
        store
            .conn
            .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if let AuthAction::Read {
                    table_name,
                    column_name,
                } = context.action
                {
                    observed_reads
                        .lock()
                        .unwrap()
                        .insert((table_name.to_owned(), column_name.to_owned()));
                }
                Authorization::Allow
            }));
        store.recent_event_embedding_documents(None, 1).unwrap();
        store
            .semantic_event_hits_by_id(&HashMap::from([(Uuid::new_v4(), (0, 1))]))
            .unwrap();
        store
            .conn
            .authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>);

        let immutable_keys = BTreeSet::from([
            ("events", "id"),
            ("event_search_lookup", "event_id"),
            ("sessions", "id"),
            ("runs", "id"),
            ("capture_sources", "id"),
            ("history_records", "id"),
        ]);
        let tracked_tables = BTreeSet::from([
            "events",
            "event_search_lookup",
            "sessions",
            "runs",
            "capture_sources",
            "history_records",
        ]);
        let projected_reads = reads
            .lock()
            .unwrap()
            .iter()
            .filter(|(table, column)| {
                tracked_tables.contains(table.as_str())
                    && !immutable_keys.contains(&(table.as_str(), column.as_str()))
            })
            .cloned()
            .collect::<BTreeSet<_>>();

        let mut trigger_columns = BTreeSet::new();
        let mut statement = store
            .conn
            .prepare(
                "SELECT tbl_name, sql FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name LIKE 'canonical_semantic_projection_%_update'
                 ORDER BY name",
            )
            .unwrap();
        let triggers = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let mut trigger_matrix = BTreeMap::<String, BTreeSet<String>>::new();
        for (table, sql) in triggers {
            let tail = sql
                .split_once("AFTER UPDATE OF")
                .map(|(_, tail)| tail)
                .unwrap_or_else(|| {
                    panic!("semantic update trigger for {table} is not parseable: {sql}")
                });
            let on_table = format!("ON {table}");
            let columns_end = tail.find(&on_table).unwrap_or_else(|| {
                panic!("semantic update trigger for {table} is not parseable: {sql}")
            });
            let columns = tail[..columns_end]
                .split(',')
                .map(str::trim)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>();
            for column in &columns {
                trigger_columns.insert((table.clone(), column.clone()));
            }
            trigger_matrix.insert(table, columns);
        }

        assert_eq!(
            trigger_columns, projected_reads,
            "every mutable relational column read by semantic document/hash or hydration SQL \
             must advance the epoch, and no excluded column may spuriously advance it"
        );
        assert!(!trigger_matrix["events"].contains("metadata_json"));
        assert!(trigger_matrix["sessions"].contains("metadata_json"));
        assert!(trigger_matrix["capture_sources"].contains("metadata_json"));
    }

    #[test]
    fn final_v5_migrates_epoch_schema_and_advances_physical_identity() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("ctx.db");
        let store = Store::open(&path).unwrap();
        let trigger_names = {
            let mut statement = store
                .conn
                .prepare(
                    "SELECT name FROM sqlite_master
                     WHERE type = 'trigger'
                       AND name LIKE 'canonical_semantic_projection_%'",
                )
                .unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(trigger_names.len(), 18);
        for trigger_name in trigger_names {
            store
                .conn
                .execute_batch(&format!("DROP TRIGGER \"{trigger_name}\";"))
                .unwrap();
        }
        store
            .conn
            .execute_batch(
                "DROP TABLE canonical_semantic_projection_state;
                 UPDATE ctx_store_schema_identity
                 SET schema_identity = 'ctx-store-schema-47-final-v5'
                 WHERE singleton = 1 AND schema_version = 47;",
            )
            .unwrap();
        drop(store);

        let reopened = Store::open(&path).unwrap();
        let physical_identity = reopened
            .conn
            .query_row(
                "SELECT schema_identity FROM ctx_store_schema_identity WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(physical_identity, crate::FINAL_SCHEMA_IDENTITY);
        assert_eq!(crate::FINAL_SCHEMA_IDENTITY, "ctx-store-schema-47-final-v7");
        assert_eq!(
            crate::CANONICAL_PROJECTION_SCHEMA_IDENTITY,
            "ctx-store-schema-47-final-v3"
        );
        assert_eq!(reopened.canonical_semantic_projection_epoch().unwrap(), 0);
    }
}
