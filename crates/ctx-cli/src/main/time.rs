#[allow(unused_imports)]
use super::*;

pub(crate) fn system_time_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

pub(crate) fn parse_since_filter(value: &str) -> Result<chrono::DateTime<Utc>> {
    let trimmed = value.trim();
    if let Some(days) = trimmed.strip_suffix('d') {
        let days: i64 = days
            .parse()
            .with_context(|| format!("invalid --since day window: {value}"))?;
        let duration = Duration::try_days(days)
            .ok_or_else(|| anyhow!("invalid --since day window: {value}: value too large"))?;
        let since = utc_now()
            .checked_sub_signed(duration)
            .ok_or_else(|| anyhow!("invalid --since day window: {value}: value too large"))?;
        return Ok(since);
    }
    Ok(chrono::DateTime::parse_from_rfc3339(trimmed)
        .with_context(|| format!("invalid --since value: {value}"))?
        .with_timezone(&Utc))
}
