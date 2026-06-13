use super::*;

mod mappers;
#[cfg(test)]
mod tests;

use self::mappers::*;

impl Store {
    pub async fn upsert_daemon_enrollment(
        &self,
        enrollment: DaemonEnrollment,
    ) -> Result<DaemonEnrollment> {
        self.query(
            r#"INSERT INTO daemon_enrollments (
                   id,
                   account_id,
                   org_id,
                   org_membership_id,
                   membership_role,
                   plan_type,
                   status,
                   policy_signature_algorithm,
                   policy_signing_key,
                   active_policy_snapshot_id,
                   enrolled_at,
                   updated_at,
                   revoked_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(org_id) DO UPDATE SET
                   id = excluded.id,
                   account_id = excluded.account_id,
                   org_membership_id = excluded.org_membership_id,
                   membership_role = excluded.membership_role,
                   plan_type = excluded.plan_type,
                   status = excluded.status,
                   policy_signature_algorithm = excluded.policy_signature_algorithm,
                   policy_signing_key = excluded.policy_signing_key,
                   active_policy_snapshot_id = excluded.active_policy_snapshot_id,
                   enrolled_at = excluded.enrolled_at,
                   updated_at = excluded.updated_at,
                   revoked_at = excluded.revoked_at"#,
        )
        .bind(enrollment.id.0.to_string())
        .bind(enrollment.account_id.0.to_string())
        .bind(enrollment.org_id.0.to_string())
        .bind(enrollment.org_membership_id.0.to_string())
        .bind(enum_str(&enrollment.membership_role)?)
        .bind(enum_str(&enrollment.plan_type)?)
        .bind(enum_str(&enrollment.status)?)
        .bind(enum_str(&enrollment.policy_signature_algorithm)?)
        .bind(&enrollment.policy_signing_key)
        .bind(
            enrollment
                .active_policy_snapshot_id
                .map(|value| value.0.to_string()),
        )
        .bind(enrollment.enrolled_at.to_rfc3339())
        .bind(enrollment.updated_at.to_rfc3339())
        .bind(enrollment.revoked_at.map(|value| value.to_rfc3339()))
        .execute(&self.pool)
        .await?;

        Ok(enrollment)
    }

    pub async fn get_daemon_enrollment_by_org_id(
        &self,
        org_id: OrgId,
    ) -> Result<Option<DaemonEnrollment>> {
        let row = self
            .query(
                r#"SELECT id, account_id, org_id, org_membership_id, membership_role, plan_type,
                          status, policy_signature_algorithm, policy_signing_key,
                          active_policy_snapshot_id, enrolled_at, updated_at, revoked_at
                   FROM daemon_enrollments
                   WHERE org_id = ?"#,
            )
            .bind(org_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(map_daemon_enrollment).transpose()
    }

    pub async fn list_daemon_enrollments(&self) -> Result<Vec<DaemonEnrollment>> {
        let rows = self
            .query(
                r#"SELECT id, account_id, org_id, org_membership_id, membership_role, plan_type,
                          status, policy_signature_algorithm, policy_signing_key,
                          active_policy_snapshot_id, enrolled_at, updated_at, revoked_at
                   FROM daemon_enrollments
                   ORDER BY updated_at DESC"#,
            )
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter().map(map_daemon_enrollment).collect()
    }

    pub async fn upsert_org_policy_snapshot(
        &self,
        snapshot: OrgPolicySnapshot,
    ) -> Result<OrgPolicySnapshot> {
        self.query(
            r#"INSERT INTO org_policy_snapshots (
                   id,
                   org_id,
                   policy_version,
                   issued_at,
                   expires_at,
                   grace_expires_at,
                   allowed_providers_json,
                   allowed_models_json,
                   required_execution_environment,
                   allowed_network_profiles_json,
                   route_policy_json,
                   archive_policy_json,
                   features_json,
                   signature,
                   cached_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(org_id, policy_version) DO UPDATE SET
                   id = excluded.id,
                   issued_at = excluded.issued_at,
                   expires_at = excluded.expires_at,
                   grace_expires_at = excluded.grace_expires_at,
                   allowed_providers_json = excluded.allowed_providers_json,
                   allowed_models_json = excluded.allowed_models_json,
                   required_execution_environment = excluded.required_execution_environment,
                   allowed_network_profiles_json = excluded.allowed_network_profiles_json,
                   route_policy_json = excluded.route_policy_json,
                   archive_policy_json = excluded.archive_policy_json,
                   features_json = excluded.features_json,
                   signature = excluded.signature,
                   cached_at = excluded.cached_at"#,
        )
        .bind(snapshot.id.0.to_string())
        .bind(snapshot.org_id.0.to_string())
        .bind(&snapshot.policy_version)
        .bind(snapshot.issued_at.to_rfc3339())
        .bind(snapshot.expires_at.to_rfc3339())
        .bind(snapshot.grace_expires_at.to_rfc3339())
        .bind(serialize_optional_json(
            snapshot.allowed_providers.as_ref(),
        )?)
        .bind(serialize_json(&snapshot.allowed_models)?)
        .bind(
            snapshot
                .required_execution_environment
                .map(|value| enum_str(&value))
                .transpose()?,
        )
        .bind(serialize_json(&snapshot.allowed_network_profiles)?)
        .bind(serialize_json(&snapshot.route_policy)?)
        .bind(serialize_json(&snapshot.archive_policy)?)
        .bind(serialize_json(&snapshot.features)?)
        .bind(&snapshot.signature)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(snapshot)
    }

    pub async fn upsert_workspace_policy_overlay(
        &self,
        overlay: WorkspacePolicyOverlay,
    ) -> Result<WorkspacePolicyOverlay> {
        self.query(
            r#"INSERT INTO workspace_policy_overlays (
                   workspace_id,
                   org_id,
                   allowed_providers_json,
                   allowed_models_json,
                   required_execution_environment,
                   allowed_network_profiles_json,
                   allowed_route_types_json,
                   features_json,
                   updated_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(workspace_id) DO UPDATE SET
                   org_id = excluded.org_id,
                   allowed_providers_json = excluded.allowed_providers_json,
                   allowed_models_json = excluded.allowed_models_json,
                   required_execution_environment = excluded.required_execution_environment,
                   allowed_network_profiles_json = excluded.allowed_network_profiles_json,
                   allowed_route_types_json = excluded.allowed_route_types_json,
                   features_json = excluded.features_json,
                   updated_at = excluded.updated_at"#,
        )
        .bind(overlay.workspace_id.0.to_string())
        .bind(overlay.org_id.0.to_string())
        .bind(serialize_optional_json(overlay.allowed_providers.as_ref())?)
        .bind(serialize_json(&overlay.allowed_models)?)
        .bind(
            overlay
                .required_execution_environment
                .map(|value| enum_str(&value))
                .transpose()?,
        )
        .bind(serialize_optional_json(
            overlay.allowed_network_profiles.as_ref(),
        )?)
        .bind(serialize_optional_json(
            overlay.allowed_route_types.as_ref(),
        )?)
        .bind(serialize_json(&overlay.features)?)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(overlay)
    }

    pub async fn get_workspace_policy_overlay(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<WorkspacePolicyOverlay>> {
        let row = self
            .query(
                r#"SELECT workspace_id, org_id, allowed_providers_json, allowed_models_json,
                          required_execution_environment, allowed_network_profiles_json,
                          allowed_route_types_json, features_json
                   FROM workspace_policy_overlays
                   WHERE workspace_id = ?"#,
            )
            .bind(workspace_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(map_workspace_policy_overlay).transpose()
    }

    pub async fn get_org_policy_snapshot(
        &self,
        id: OrgPolicySnapshotId,
    ) -> Result<Option<OrgPolicySnapshot>> {
        let row = self
            .query(
                r#"SELECT id, org_id, policy_version, issued_at, expires_at, grace_expires_at,
                          allowed_providers_json, allowed_models_json,
                          required_execution_environment, allowed_network_profiles_json,
                          route_policy_json, archive_policy_json, features_json, signature
                   FROM org_policy_snapshots
                   WHERE id = ?"#,
            )
            .bind(id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(map_org_policy_snapshot).transpose()
    }

    pub async fn get_latest_org_policy_snapshot(
        &self,
        org_id: OrgId,
    ) -> Result<Option<OrgPolicySnapshot>> {
        let row = self
            .query(
                r#"SELECT id, org_id, policy_version, issued_at, expires_at, grace_expires_at,
                          allowed_providers_json, allowed_models_json,
                          required_execution_environment, allowed_network_profiles_json,
                          route_policy_json, archive_policy_json, features_json, signature
                   FROM org_policy_snapshots
                   WHERE org_id = ?
                   ORDER BY issued_at DESC, cached_at DESC
                   LIMIT 1"#,
            )
            .bind(org_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(map_org_policy_snapshot).transpose()
    }

    pub async fn create_run_grant(&self, run_grant: RunGrant) -> Result<RunGrant> {
        self.query(
            r#"INSERT INTO run_grants (
                   id,
                   run_id,
                   session_id,
                   workspace_id,
                   account_id,
                   org_id,
                   membership_role,
                   policy_version,
                   provider_id,
                   model_id,
                   execution_environment,
                   network_profile,
                   route_type,
                   archive_mode,
                   issued_at,
                   expires_at,
                   decision_source
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(run_grant.id.0.to_string())
        .bind(run_grant.run_id.0.to_string())
        .bind(run_grant.session_id.0.to_string())
        .bind(run_grant.workspace_id.0.to_string())
        .bind(run_grant.account_id.0.to_string())
        .bind(run_grant.org_id.0.to_string())
        .bind(
            run_grant
                .membership_role
                .map(|value| enum_str(&value))
                .transpose()?,
        )
        .bind(&run_grant.policy_version)
        .bind(&run_grant.provider_id)
        .bind(&run_grant.model_id)
        .bind(enum_str(&run_grant.execution_environment)?)
        .bind(enum_str(&run_grant.network_profile)?)
        .bind(
            run_grant
                .route_type
                .map(|value| enum_str(&value))
                .transpose()?,
        )
        .bind(enum_str(&run_grant.archive_mode)?)
        .bind(run_grant.issued_at.to_rfc3339())
        .bind(run_grant.expires_at.map(|value| value.to_rfc3339()))
        .bind(enum_str(&run_grant.decision_source)?)
        .execute(&self.pool)
        .await?;

        Ok(run_grant)
    }

    pub async fn get_run_grant(&self, id: RunGrantId) -> Result<Option<RunGrant>> {
        let row = self
            .query(
                r#"SELECT id, run_id, session_id, workspace_id, account_id, org_id,
                          membership_role, policy_version, provider_id, model_id,
                          execution_environment, network_profile, route_type, archive_mode,
                          issued_at, expires_at, decision_source
                   FROM run_grants
                   WHERE id = ?"#,
            )
            .bind(id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(map_run_grant).transpose()
    }

    pub async fn get_run_grant_by_run_id(&self, run_id: RunId) -> Result<Option<RunGrant>> {
        let row = self
            .query(
                r#"SELECT id, run_id, session_id, workspace_id, account_id, org_id,
                          membership_role, policy_version, provider_id, model_id,
                          execution_environment, network_profile, route_type, archive_mode,
                          issued_at, expires_at, decision_source
                   FROM run_grants
                   WHERE run_id = ?"#,
            )
            .bind(run_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(map_run_grant).transpose()
    }

    pub async fn append_policy_decision_event(
        &self,
        event: PolicyDecisionEvent,
    ) -> Result<PolicyDecisionEvent> {
        self.query(
            r#"INSERT INTO policy_decision_events (
                   id,
                   run_grant_id,
                   run_id,
                   session_id,
                   workspace_id,
                   account_id,
                   org_id,
                   policy_snapshot_id,
                   policy_version,
                   decision_source,
                   outcome,
                   deny_reason,
                   requested_provider_id,
                   requested_model_id,
                   requested_execution_environment,
                   requested_network_profile,
                   requested_route_type,
                   detail,
                   created_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(event.id.0.to_string())
        .bind(event.run_grant_id.map(|value| value.0.to_string()))
        .bind(event.run_id.map(|value| value.0.to_string()))
        .bind(event.session_id.map(|value| value.0.to_string()))
        .bind(event.workspace_id.map(|value| value.0.to_string()))
        .bind(event.account_id.map(|value| value.0.to_string()))
        .bind(event.org_id.map(|value| value.0.to_string()))
        .bind(event.policy_snapshot_id.map(|value| value.0.to_string()))
        .bind(&event.policy_version)
        .bind(enum_str(&event.decision_source)?)
        .bind(enum_str(&event.outcome)?)
        .bind(
            event
                .deny_reason
                .map(|value| enum_str(&value))
                .transpose()?,
        )
        .bind(&event.requested_provider_id)
        .bind(&event.requested_model_id)
        .bind(
            event
                .requested_execution_environment
                .map(|value| enum_str(&value))
                .transpose()?,
        )
        .bind(
            event
                .requested_network_profile
                .map(|value| enum_str(&value))
                .transpose()?,
        )
        .bind(
            event
                .requested_route_type
                .map(|value| enum_str(&value))
                .transpose()?,
        )
        .bind(&event.detail)
        .bind(event.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(event)
    }

    pub async fn list_policy_decision_events_for_run(
        &self,
        run_id: RunId,
    ) -> Result<Vec<PolicyDecisionEvent>> {
        let rows = self
            .query(
                r#"SELECT id, run_grant_id, run_id, session_id, workspace_id, account_id,
                          org_id, policy_snapshot_id, policy_version, decision_source, outcome,
                          deny_reason, requested_provider_id, requested_model_id,
                          requested_execution_environment, requested_network_profile,
                          requested_route_type, detail, created_at
                   FROM policy_decision_events
                   WHERE run_id = ?
                   ORDER BY created_at ASC, id ASC"#,
            )
            .bind(run_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter().map(map_policy_decision_event).collect()
    }
}
