use anyhow::{bail, Context as _, Result};

pub(super) fn validate_portal_url(value: &str) -> Result<()> {
    if value.len() > 4096 {
        bail!("invalid_response: Pro management URL exceeds the maximum length");
    }
    let parsed = url::Url::parse(value).context("invalid_response: invalid Pro management URL")?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() || parsed.username() != "" {
        bail!("invalid_response: Pro management URL must be an HTTPS origin");
    }
    Ok(())
}

pub(super) fn validate_access_status(
    state: &str,
    refresh_after_unix: Option<i64>,
    access_deadline_unix: Option<i64>,
    grace_deadline_unix: Option<i64>,
) -> Result<()> {
    if !matches!(
        state,
        "trial" | "active" | "canceling_paid" | "offline_grace" | "locked"
    ) {
        bail!("invalid_response: Pro access state is invalid");
    }
    if [
        refresh_after_unix,
        access_deadline_unix,
        grace_deadline_unix,
    ]
    .into_iter()
    .flatten()
    .any(|value| value <= 0)
    {
        bail!("invalid_response: Pro access deadline is invalid");
    }
    if matches!((access_deadline_unix, grace_deadline_unix), (Some(access), Some(grace)) if access > grace)
    {
        bail!("invalid_response: Pro access deadlines are inconsistent");
    }
    if matches!(state, "trial" | "active" | "canceling_paid") && access_deadline_unix.is_none() {
        bail!("invalid_response: Pro access deadline is missing");
    }
    if state == "offline_grace" && (access_deadline_unix.is_none() || grace_deadline_unix.is_none())
    {
        bail!("invalid_response: Pro offline-grace deadlines are missing");
    }
    Ok(())
}
