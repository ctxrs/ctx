use super::*;
use serde::de::DeserializeOwned;

pub(super) fn map_daemon_enrollment(row: SqliteRow) -> Result<DaemonEnrollment> {
    Ok(DaemonEnrollment {
        id: parse_uuid_id(&row.try_get::<String, _>("id")?, DaemonEnrollmentId)?,
        account_id: parse_uuid_id(&row.try_get::<String, _>("account_id")?, AccountId)?,
        org_id: parse_uuid_id(&row.try_get::<String, _>("org_id")?, OrgId)?,
        org_membership_id: parse_uuid_id(
            &row.try_get::<String, _>("org_membership_id")?,
            OrgMembershipId,
        )?,
        membership_role: parse_string_enum(&row.try_get::<String, _>("membership_role")?)?,
        plan_type: parse_string_enum(&row.try_get::<String, _>("plan_type")?)?,
        status: parse_string_enum(&row.try_get::<String, _>("status")?)?,
        policy_signature_algorithm: parse_string_enum(
            &row.try_get::<String, _>("policy_signature_algorithm")?,
        )?,
        policy_signing_key: row.try_get("policy_signing_key")?,
        active_policy_snapshot_id: parse_optional_uuid_id(
            row.try_get::<Option<String>, _>("active_policy_snapshot_id")?,
            OrgPolicySnapshotId,
        )?,
        enrolled_at: parse_dt(&row.try_get::<String, _>("enrolled_at")?)?,
        updated_at: parse_dt(&row.try_get::<String, _>("updated_at")?)?,
        revoked_at: row
            .try_get::<Option<String>, _>("revoked_at")?
            .map(|value| parse_dt(&value))
            .transpose()?,
    })
}

pub(super) fn map_workspace_policy_overlay(row: SqliteRow) -> Result<WorkspacePolicyOverlay> {
    let allowed_providers_json: Option<String> = row.try_get("allowed_providers_json")?;
    let required_execution_environment: Option<String> =
        row.try_get("required_execution_environment")?;
    let allowed_network_profiles_json: Option<String> =
        row.try_get("allowed_network_profiles_json")?;
    let allowed_route_types_json: Option<String> = row.try_get("allowed_route_types_json")?;

    Ok(WorkspacePolicyOverlay {
        workspace_id: parse_uuid_id(&row.try_get::<String, _>("workspace_id")?, WorkspaceId)?,
        org_id: parse_uuid_id(&row.try_get::<String, _>("org_id")?, OrgId)?,
        allowed_providers: allowed_providers_json
            .as_deref()
            .map(parse_json)
            .transpose()?,
        allowed_models: parse_json(&row.try_get::<String, _>("allowed_models_json")?)?,
        required_execution_environment: required_execution_environment
            .as_deref()
            .map(parse_string_enum)
            .transpose()?,
        allowed_network_profiles: allowed_network_profiles_json
            .as_deref()
            .map(parse_json)
            .transpose()?,
        allowed_route_types: allowed_route_types_json
            .as_deref()
            .map(parse_json)
            .transpose()?,
        features: parse_json(&row.try_get::<String, _>("features_json")?)?,
    })
}

pub(super) fn map_org_policy_snapshot(row: SqliteRow) -> Result<OrgPolicySnapshot> {
    let allowed_providers_json: Option<String> = row.try_get("allowed_providers_json")?;
    let required_execution_environment: Option<String> =
        row.try_get("required_execution_environment")?;

    Ok(OrgPolicySnapshot {
        id: parse_uuid_id(&row.try_get::<String, _>("id")?, OrgPolicySnapshotId)?,
        org_id: parse_uuid_id(&row.try_get::<String, _>("org_id")?, OrgId)?,
        policy_version: row.try_get("policy_version")?,
        issued_at: parse_dt(&row.try_get::<String, _>("issued_at")?)?,
        expires_at: parse_dt(&row.try_get::<String, _>("expires_at")?)?,
        grace_expires_at: parse_dt(&row.try_get::<String, _>("grace_expires_at")?)?,
        allowed_providers: allowed_providers_json
            .as_deref()
            .map(parse_json)
            .transpose()?,
        allowed_models: parse_json(&row.try_get::<String, _>("allowed_models_json")?)?,
        required_execution_environment: required_execution_environment
            .as_deref()
            .map(parse_string_enum)
            .transpose()?,
        allowed_network_profiles: parse_json(
            &row.try_get::<String, _>("allowed_network_profiles_json")?,
        )?,
        route_policy: parse_json(&row.try_get::<String, _>("route_policy_json")?)?,
        archive_policy: parse_json(&row.try_get::<String, _>("archive_policy_json")?)?,
        features: parse_json(&row.try_get::<String, _>("features_json")?)?,
        signature: row.try_get("signature")?,
    })
}

pub(super) fn map_run_grant(row: SqliteRow) -> Result<RunGrant> {
    Ok(RunGrant {
        id: parse_uuid_id(&row.try_get::<String, _>("id")?, RunGrantId)?,
        run_id: parse_uuid_id(&row.try_get::<String, _>("run_id")?, RunId)?,
        session_id: parse_uuid_id(&row.try_get::<String, _>("session_id")?, SessionId)?,
        workspace_id: parse_uuid_id(&row.try_get::<String, _>("workspace_id")?, WorkspaceId)?,
        account_id: parse_uuid_id(&row.try_get::<String, _>("account_id")?, AccountId)?,
        org_id: parse_uuid_id(&row.try_get::<String, _>("org_id")?, OrgId)?,
        membership_role: row
            .try_get::<Option<String>, _>("membership_role")?
            .as_deref()
            .map(parse_string_enum)
            .transpose()?,
        policy_version: row.try_get("policy_version")?,
        provider_id: row.try_get("provider_id")?,
        model_id: row.try_get("model_id")?,
        execution_environment: parse_string_enum(
            &row.try_get::<String, _>("execution_environment")?,
        )?,
        network_profile: parse_string_enum(&row.try_get::<String, _>("network_profile")?)?,
        route_type: row
            .try_get::<Option<String>, _>("route_type")?
            .as_deref()
            .map(parse_string_enum)
            .transpose()?,
        archive_mode: parse_string_enum(&row.try_get::<String, _>("archive_mode")?)?,
        issued_at: parse_dt(&row.try_get::<String, _>("issued_at")?)?,
        expires_at: row
            .try_get::<Option<String>, _>("expires_at")?
            .map(|value| parse_dt(&value))
            .transpose()?,
        decision_source: parse_string_enum(&row.try_get::<String, _>("decision_source")?)?,
    })
}

pub(super) fn map_policy_decision_event(row: SqliteRow) -> Result<PolicyDecisionEvent> {
    Ok(PolicyDecisionEvent {
        id: parse_uuid_id(&row.try_get::<String, _>("id")?, PolicyDecisionEventId)?,
        run_grant_id: parse_optional_uuid_id(
            row.try_get::<Option<String>, _>("run_grant_id")?,
            RunGrantId,
        )?,
        run_id: parse_optional_uuid_id(row.try_get::<Option<String>, _>("run_id")?, RunId)?,
        session_id: parse_optional_uuid_id(
            row.try_get::<Option<String>, _>("session_id")?,
            SessionId,
        )?,
        workspace_id: parse_optional_uuid_id(
            row.try_get::<Option<String>, _>("workspace_id")?,
            WorkspaceId,
        )?,
        account_id: parse_optional_uuid_id(
            row.try_get::<Option<String>, _>("account_id")?,
            AccountId,
        )?,
        org_id: parse_optional_uuid_id(row.try_get::<Option<String>, _>("org_id")?, OrgId)?,
        policy_snapshot_id: parse_optional_uuid_id(
            row.try_get::<Option<String>, _>("policy_snapshot_id")?,
            OrgPolicySnapshotId,
        )?,
        policy_version: row.try_get("policy_version")?,
        decision_source: parse_string_enum(&row.try_get::<String, _>("decision_source")?)?,
        outcome: parse_string_enum(&row.try_get::<String, _>("outcome")?)?,
        deny_reason: row
            .try_get::<Option<String>, _>("deny_reason")?
            .as_deref()
            .map(parse_string_enum)
            .transpose()?,
        requested_provider_id: row.try_get("requested_provider_id")?,
        requested_model_id: row.try_get("requested_model_id")?,
        requested_execution_environment: row
            .try_get::<Option<String>, _>("requested_execution_environment")?
            .as_deref()
            .map(parse_string_enum)
            .transpose()?,
        requested_network_profile: row
            .try_get::<Option<String>, _>("requested_network_profile")?
            .as_deref()
            .map(parse_string_enum)
            .transpose()?,
        requested_route_type: row
            .try_get::<Option<String>, _>("requested_route_type")?
            .as_deref()
            .map(parse_string_enum)
            .transpose()?,
        detail: row.try_get("detail")?,
        created_at: parse_dt(&row.try_get::<String, _>("created_at")?)?,
    })
}

pub(super) fn serialize_json<T>(value: &T) -> Result<String>
where
    T: Serialize,
{
    serde_json::to_string(value).context("serialize json column")
}

pub(super) fn serialize_optional_json<T>(value: Option<&T>) -> Result<Option<String>>
where
    T: Serialize,
{
    value.map(serialize_json).transpose()
}

fn parse_json<T>(value: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(value).context("parse json column")
}

pub(super) fn enum_str<T>(value: &T) -> Result<String>
where
    T: Serialize,
{
    match serde_json::to_value(value).context("serialize enum")? {
        serde_json::Value::String(value) => Ok(value),
        _ => anyhow::bail!("expected enum to serialize as string"),
    }
}

fn parse_string_enum<T>(value: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .context("parse enum from string column")
}

fn parse_uuid_id<T>(value: &str, build: fn(uuid::Uuid) -> T) -> Result<T> {
    Ok(build(uuid::Uuid::parse_str(value)?))
}

fn parse_optional_uuid_id<T>(
    value: Option<String>,
    build: fn(uuid::Uuid) -> T,
) -> Result<Option<T>> {
    value
        .as_deref()
        .map(|value| parse_uuid_id(value, build))
        .transpose()
}
