use super::*;

pub(super) const TYPED_STATUS_FIELDS: &[&str] = &[
    "logical_request_id",
    "physical_attempt_id",
    "physical_attempt_state",
    "progress_owner_request_id",
    "progress_owner_attempt_state",
    "structured_outcome",
    "maintenance_wake",
];

pub(super) fn parse_maintenance_wake(
    fields: &Value,
    request_state: RefreshRequestState,
) -> Result<RefreshStatusKind> {
    if fields.get("maintenance_wake").and_then(Value::as_bool) != Some(true)
        || request_state != RefreshRequestState::Queued
        || fields.get("logical_phase").and_then(Value::as_str) != Some("waiting")
        || fields
            .get("progress")
            .and_then(|progress| progress.get("phase"))
            .and_then(Value::as_str)
            != Some("maintenance_wake")
        || [
            "physical_attempt_id",
            "physical_attempt_state",
            "progress_owner_request_id",
            "progress_owner_attempt_state",
            "structured_outcome",
        ]
        .iter()
        .any(|field| fields.get(*field).is_some())
    {
        bail!("source refresh response has invalid background maintenance wake status");
    }
    let request_id = required_status_string(fields, "request_id")?.to_owned();
    if required_status_string(fields, "logical_request_id")? != request_id {
        bail!("source refresh maintenance wake authority does not match its request ID");
    }
    let previous_generation = optional_status_string(fields, "previous_generation")?;
    let published_generation = optional_status_string(fields, "published_generation")?;
    if previous_generation != published_generation {
        bail!("source refresh maintenance wake generation authority is inconsistent");
    }
    Ok(RefreshStatusKind::BackgroundMaintenanceWake(
        RefreshMaintenanceWakeStatus {
            request_id,
            previous_generation,
            published_generation,
        },
    ))
}

pub(super) fn parse_terminal_outcome(value: &Value) -> Result<RefreshTerminalOutcome> {
    let fields = value
        .as_object()
        .ok_or_else(|| anyhow!("source refresh structured outcome is not an object"))?;
    let code: RefreshOutcomeCode = required_outcome_string(fields, "code")?.parse()?;
    let class: RefreshOutcomeClass = required_outcome_string(fields, "class")?.parse()?;
    let retryable = fields
        .get("retryable")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("source refresh structured outcome has invalid retryability"))?;
    let affected_routes = outcome_routes(fields, "affected_routes")?;
    let retryable_routes = outcome_routes(fields, "retryable_routes")?;
    let blocked_routes = outcome_routes(fields, "blocked_routes")?;
    let physical_attempt_id = required_outcome_string(fields, "physical_attempt_id")?.to_owned();
    let retry_advice = match optional_outcome_string(fields, "retry_advice")? {
        Some(value) => Some(value.parse()?),
        None => None,
    };
    let outcome = RefreshTerminalOutcome::new(
        code,
        retryable,
        affected_routes,
        retryable_routes,
        blocked_routes,
        physical_attempt_id,
        optional_outcome_string(fields, "retained_generation")?,
        optional_outcome_string(fields, "published_generation")?,
        retry_advice,
        optional_outcome_string(fields, "detail")?,
    )?;
    outcome.validate_declared_class(class)?;
    Ok(outcome)
}

pub(super) fn required_status_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("source refresh response has invalid `{field}`"))
}

pub(super) fn optional_status_string(value: &Value, field: &str) -> Result<Option<String>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        _ => bail!("source refresh response has invalid `{field}`"),
    }
}

fn required_outcome_string<'a>(
    fields: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a str> {
    fields
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("source refresh structured outcome has invalid `{field}`"))
}

fn optional_outcome_string(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<String>> {
    match fields.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        _ => bail!("source refresh structured outcome has invalid `{field}`"),
    }
}

fn outcome_routes(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<BTreeSet<SourceRouteIdentity>> {
    let values = fields
        .get(field)
        .and_then(Value::as_array)
        .filter(|routes| routes.len() <= SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT)
        .ok_or_else(|| anyhow!("source refresh structured outcome has invalid `{field}`"))?;
    let routes = values
        .iter()
        .map(|route| {
            route
                .as_str()
                .ok_or_else(|| anyhow!("source refresh outcome route is not a string"))
                .and_then(|route| {
                    SourceRouteIdentity::from_sha256(route.to_owned()).map_err(Into::into)
                })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if routes.len() != values.len() {
        bail!("source refresh structured outcome has duplicate `{field}` routes");
    }
    Ok(routes)
}
