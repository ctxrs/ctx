use anyhow::{anyhow, Result};
use ctx_history_store::CanonicalSemanticProjectionVersion;
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use super::vector_store::SemanticVectorStore;

const RECONCILED_VERSION_STATE: &str = "canonical_semantic_projection_reconciled_version";
const RECONCILIATION_TARGET_VERSION_STATE: &str =
    "canonical_semantic_projection_reconciliation_target_version";
const COMMITTED_RECONCILIATION_CURSOR_STATE: &str = "committed_store_reconcile_cursor_before";
const PRUNE_CURSOR_STATE: &str = "prune_anchor_cursor_before";

impl SemanticVectorStore {
    fn reconciliation_projection_version(
        &self,
        state_name: &str,
    ) -> Result<Option<CanonicalSemanticProjectionVersion>> {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
                [state_name],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(value) = value else {
            return Ok(None);
        };
        let Some((store_identity, mutation_epoch)) = value.split_once(':') else {
            return Err(anyhow!(
                "semantic vector store has invalid Store version state {state_name}={value:?}"
            ));
        };
        let store_identity = Uuid::parse_str(store_identity).map_err(|_| {
            anyhow!("semantic vector store has invalid Store version state {state_name}={value:?}")
        })?;
        let mutation_epoch = mutation_epoch.parse::<u64>().map_err(|_| {
            anyhow!("semantic vector store has invalid Store version state {state_name}={value:?}")
        })?;
        Ok(Some(CanonicalSemanticProjectionVersion {
            store_identity,
            mutation_epoch,
        }))
    }

    pub(super) fn reconciled_store_version(
        &self,
    ) -> Result<Option<CanonicalSemanticProjectionVersion>> {
        self.reconciliation_projection_version(RECONCILED_VERSION_STATE)
    }

    pub(super) fn reconciliation_target_store_version(
        &self,
    ) -> Result<Option<CanonicalSemanticProjectionVersion>> {
        self.reconciliation_projection_version(RECONCILIATION_TARGET_VERSION_STATE)
    }

    pub(super) fn begin_reconciliation_version(
        &mut self,
        version: CanonicalSemanticProjectionVersion,
    ) -> Result<()> {
        let value = format!("{}:{}", version.store_identity, version.mutation_epoch);
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM semantic_maintenance_state
             WHERE key IN (?1, ?2)",
            params![COMMITTED_RECONCILIATION_CURSOR_STATE, PRUNE_CURSOR_STATE],
        )?;
        tx.execute(
            r#"
            INSERT INTO semantic_maintenance_state (key, value)
            VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value
            "#,
            params![RECONCILIATION_TARGET_VERSION_STATE, value],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn acknowledge_reconciliation_version(
        &mut self,
        version: CanonicalSemanticProjectionVersion,
    ) -> Result<()> {
        let value = format!("{}:{}", version.store_identity, version.mutation_epoch);
        let tx = self.conn.transaction()?;
        tx.execute(
            r#"
            INSERT INTO semantic_maintenance_state (key, value)
            VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value
            "#,
            params![RECONCILED_VERSION_STATE, value],
        )?;
        tx.execute(
            "DELETE FROM semantic_maintenance_state WHERE key = ?1",
            [RECONCILIATION_TARGET_VERSION_STATE],
        )?;
        tx.commit()?;
        Ok(())
    }
}
