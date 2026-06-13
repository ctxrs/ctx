impl Store {
    pub async fn get_session_head(
        &self,
        session_id: SessionId,
        limit: u32,
        include_events: bool,
    ) -> Result<Option<SessionHead>> {
        self.get_session_head_with_kind(session_id, limit, include_events, None)
            .await
    }

    pub async fn get_session_head_snapshot(
        &self,
        session_id: SessionId,
        limit: u32,
        include_events: bool,
    ) -> Result<Option<SessionHeadSnapshot>> {
        crate::fault_injection::maybe_fail("ctx_store.get_session_head_snapshot")?;
        let head = self
            .get_session_head(session_id, limit, include_events)
            .await?;
        Ok(head.map(session_head_to_snapshot))
    }

    pub async fn get_active_snapshot_head(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionHeadSnapshot>> {
        crate::fault_injection::maybe_fail("ctx_store.get_active_snapshot_head")?;
        let session = match self.get_session(session_id).await? {
            Some(session) => session,
            None => return Ok(None),
        };
        if !matches!(
            self.session_head_kind_for_task(session.task_id).await?,
            SessionHeadKind::Active
        ) {
            return Ok(None);
        }
        let last_event_seq = self.session_last_event_seq(session_id).await?;
        let projection_rev = self.get_session_projection_rev(session_id).await?;
        if !self
            .is_task_primary_session(session.task_id, session.id)
            .await?
        {
            let limits = session_head_limits(SessionHeadKind::Active, ACTIVE_SNAPSHOT_HEAD_LIMIT);
            let head = self
                .build_session_head(&session, limits, false, last_event_seq, projection_rev)
                .await?;
            return Ok(Some(session_head_to_snapshot(head)));
        }
        let projection = match self
            .load_active_snapshot_head_projection(session_id)
            .await?
        {
            Some(projection)
                if projection.last_event_seq == last_event_seq
                    && projection.head_rev == projection_rev =>
            {
                projection
            }
            _ => {
                self.refresh_active_snapshot_head(session_id, Some(last_event_seq))
                    .await?;
                match self
                    .load_active_snapshot_head_projection(session_id)
                    .await?
                {
                    Some(projection) => projection,
                    None => {
                        let limits = session_head_limits(
                            SessionHeadKind::Active,
                            ACTIVE_SNAPSHOT_HEAD_LIMIT,
                        );
                        let head = self
                            .build_session_head(
                                &session,
                                limits,
                                false,
                                last_event_seq,
                                projection_rev,
                            )
                            .await?;
                        return Ok(Some(session_head_to_snapshot(head)));
                    }
                }
            }
        };
        Ok(Some(session_head_to_snapshot(
            projection.into_session_head(session, projection_rev),
        )))
    }

    pub(super) async fn get_session_head_with_kind(
        &self,
        session_id: SessionId,
        limit: u32,
        include_events: bool,
        head_kind_override: Option<SessionHeadKind>,
    ) -> Result<Option<SessionHead>> {
        let session = match self.get_session(session_id).await? {
            Some(session) => session,
            None => return Ok(None),
        };
        let head_kind = match head_kind_override {
            Some(kind) => kind,
            None => self.session_head_kind_for_task(session.task_id).await?,
        };
        let last_event_seq = self.session_last_event_seq(session_id).await?;
        let projection_rev = self.get_session_projection_rev(session_id).await?;

        if let Some(materialized) = self
            .load_session_head_materialization(session_id, head_kind)
            .await?
        {
            if materialized.last_event_seq == last_event_seq
                && materialized.head_rev == projection_rev
            {
                let summary_checkpoint = self.get_session_summary_checkpoint(session_id).await?;
                let head =
                    materialized.into_session_head(session, projection_rev, summary_checkpoint);
                let limits = session_head_limits(head_kind, limit);
                return Ok(Some(apply_session_head_limits(
                    head,
                    limits,
                    include_events,
                )));
            }
        }

        let turn_limit = if disable_head_materialization_writes_for(head_kind) {
            limit
        } else {
            match head_kind {
                SessionHeadKind::Active => SESSION_HEAD_MAX_TURNS,
                SessionHeadKind::Archived => SESSION_HEAD_ARCHIVED_TURN_LIMIT,
            }
        };
        let materialize_limits = session_head_limits(head_kind, turn_limit);
        let head = self
            .build_session_head(
                &session,
                materialize_limits,
                true,
                last_event_seq,
                projection_rev,
            )
            .await?;
        if !disable_head_materialization_writes_for(head_kind) {
            let store = self.clone();
            let session_id = session.id;
            let materialized = SessionHeadMaterialization::from_head(&head);
            tokio::spawn(async move {
                if let Err(err) = store
                    .upsert_session_head_materialization(session_id, head_kind, &materialized)
                    .await
                {
                    tracing::warn!(
                        session_id = %session_id.0,
                        "failed to persist session head materialization: {err:#}"
                    );
                }
            });
        }
        let limits = session_head_limits(head_kind, limit);
        Ok(Some(apply_session_head_limits(
            head,
            limits,
            include_events,
        )))
    }

    pub async fn refresh_active_session_head_projection(
        &self,
        session_id: SessionId,
    ) -> Result<bool> {
        let session = match self.get_session(session_id).await? {
            Some(session) => session,
            None => return Ok(false),
        };
        if !matches!(
            self.session_head_kind_for_task(session.task_id).await?,
            SessionHeadKind::Active
        ) {
            return Ok(false);
        }
        let last_event_seq = self.session_last_event_seq(session_id).await?;
        let projection_rev = self.get_session_projection_rev(session_id).await?;
        if let Some(materialized) = self
            .load_active_snapshot_head_projection(session_id)
            .await?
        {
            if materialized.last_event_seq == last_event_seq
                && materialized.head_rev == projection_rev
            {
                return Ok(false);
            }
        }
        self.refresh_active_snapshot_head(session_id, Some(last_event_seq))
            .await?;
        Ok(true)
    }
}
