use anyhow::Result;
use ctx_history_index::VerifiedIndex;

use super::{
    advance_frontier_progress, SemanticBatchEmbedder, SemanticDocumentBuilder, SemanticVectorStore,
    SourceBackedSemanticGeneration, SourceBackedSemanticOutcome, SourceProjectionFrontier,
    FULL_REBUILD_STATE,
};

impl SemanticVectorStore {
    /// Reconciles one pinned Core generation and reports each durable semantic
    /// boundary before continuing with later source pages. The sequence is
    /// owned by the durable frontier/acknowledgement, not by this callback.
    pub fn reconcile_source_backed_index_with_checkpoint_and_progress(
        &mut self,
        index: &VerifiedIndex,
        builder: &mut dyn SemanticDocumentBuilder,
        embedder: &mut dyn SemanticBatchEmbedder,
        checkpoint: &mut dyn FnMut() -> Result<()>,
        progress: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<SourceBackedSemanticOutcome> {
        let work_before = self.flat.work_stats();
        let generation =
            SourceBackedSemanticGeneration::from_verified_index(index, self.contract())?;
        let mut outcome = self.reconcile_source_backed_generation_with_checkpoint(
            index,
            &generation,
            builder,
            embedder,
            checkpoint,
            progress,
        )?;
        let work = self.flat.work_since(work_before);
        outcome.vectors_touched = work.vectors_touched;
        outcome.vector_bytes_touched = work.vector_bytes_touched;
        outcome.metadata_records_touched = work.metadata_records_touched;
        Ok(outcome)
    }

    pub(super) fn full_rebuild_pending(&self) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM semantic_maintenance_state WHERE key = ?1
                 )",
                [FULL_REBUILD_STATE],
                |row| row.get::<_, bool>(0),
            )
            .map_err(Into::into)
    }

    pub(super) fn reconcile_pending_full_rebuild(
        &mut self,
        frontier: &mut SourceProjectionFrontier,
        progress: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Option<SourceBackedSemanticOutcome>> {
        if !self.full_rebuild_pending()? {
            return Ok(None);
        }
        self.flat
            .begin_reconciliation_view(FULL_REBUILD_STATE)
            .map_err(anyhow::Error::new)?;
        let event_ids = self
            .flat
            .reconciliation_event_ids(FULL_REBUILD_STATE, super::MAX_SOURCE_EVENT_PAGE_ITEMS)
            .map_err(anyhow::Error::new)?;
        if !event_ids.is_empty() {
            let deleted_chunks = self.delete_events_coordinated(&event_ids)?;
            let sequence = advance_frontier_progress(self, frontier)?;
            progress(sequence)?;
            return Ok(Some(SourceBackedSemanticOutcome {
                deleted_chunks,
                work_remaining: true,
                semantic_progress_sequence: Some(sequence),
                full_rebuild_boundary: true,
                ..SourceBackedSemanticOutcome::default()
            }));
        }
        self.flat
            .finish_reconciliation_view_coordinated()
            .map_err(anyhow::Error::new)?;
        self.conn.execute(
            "DELETE FROM semantic_maintenance_state WHERE key = ?1",
            [FULL_REBUILD_STATE],
        )?;
        frontier.flat_publication = self
            .flat
            .active_publication_token()
            .map_err(anyhow::Error::new)?;
        frontier.flat_staging = None;
        self.store_source_frontier(frontier)?;
        Ok(Some(SourceBackedSemanticOutcome {
            work_remaining: true,
            full_rebuild_boundary: true,
            ..SourceBackedSemanticOutcome::default()
        }))
    }

    pub(super) fn refresh_idle_frontier_publication_after_full_rebuild(&self) -> Result<()> {
        let Some(mut frontier) = self.source_frontier()? else {
            return Ok(());
        };
        if frontier.active_source_identity_digest.is_some() {
            return Err(super::SemanticVectorStoreError::reset_required(
                "full rebuild completed with an active semantic source frontier",
            )
            .into());
        }
        frontier.flat_publication = self
            .flat
            .active_publication_token()
            .map_err(anyhow::Error::new)?;
        frontier.flat_staging = None;
        self.store_source_frontier(&frontier)
    }
}
