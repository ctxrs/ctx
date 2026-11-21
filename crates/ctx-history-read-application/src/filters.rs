use anyhow::{anyhow, Context, Result};
use chrono::{Duration, NaiveDate, Utc};
use thiserror::Error;

use ctx_history_core::utc_now;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SourceIdentityFilterError {
    #[error("history_source expects plugin/source or provider_key/source_id")]
    InvalidHistorySource,
    #[error("custom history source filters require the custom provider")]
    CustomProviderRequired,
}

#[derive(Debug, Clone, Default)]
pub struct SourceIdentityFilterArgs {
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SourceIdentityFilters {
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
}

impl SourceIdentityFilters {
    pub fn is_empty(&self) -> bool {
        self.history_source.is_none()
            && self.provider_key.is_none()
            && self.source_id.is_none()
            && self.source_format.is_none()
    }
}

pub fn normalize_source_identity_filters(
    input: SourceIdentityFilterArgs,
) -> Result<SourceIdentityFilters> {
    let history_source = normalize_source_identity_filter("history_source", input.history_source)?;
    if history_source
        .as_deref()
        .is_some_and(|value| !value.contains('/'))
    {
        return Err(SourceIdentityFilterError::InvalidHistorySource.into());
    }
    Ok(SourceIdentityFilters {
        history_source,
        provider_key: normalize_source_identity_filter("provider_key", input.provider_key)?,
        source_id: normalize_source_identity_filter("source_id", input.source_id)?,
        source_format: normalize_source_identity_filter("source_format", input.source_format)?,
    })
}

pub fn normalize_source_identity_filter(
    label: &str,
    value: Option<String>,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("{label} cannot be empty"));
    }
    if value.chars().any(char::is_control) {
        return Err(anyhow!("{label} cannot contain control characters"));
    }
    Ok(Some(value.to_owned()))
}

pub fn parse_since_filter(value: &str) -> Result<chrono::DateTime<Utc>> {
    parse_since_filter_at(value, utc_now())
}

pub fn parse_since_filter_at(
    value: &str,
    now: chrono::DateTime<Utc>,
) -> Result<chrono::DateTime<Utc>> {
    let trimmed = value.trim();
    if let Some(days) = trimmed.strip_suffix('d') {
        let days: i64 = days
            .parse()
            .with_context(|| format!("invalid since day window: {value}"))?;
        if days < 0 {
            return Err(anyhow!(
                "invalid since day window: {value}: value must be non-negative"
            ));
        }
        let duration = Duration::try_days(days)
            .ok_or_else(|| anyhow!("invalid since day window: {value}: value too large"))?;
        let since = now
            .checked_sub_signed(duration)
            .ok_or_else(|| anyhow!("invalid since day window: {value}: value too large"))?;
        return Ok(since);
    }
    if let Some((amount, unit)) = split_relative_since_window(trimmed) {
        let amount: i64 = amount
            .parse()
            .with_context(|| format!("invalid since window: {value}"))?;
        if amount < 0 {
            return Err(anyhow!(
                "invalid since window: {value}: value must be non-negative"
            ));
        }
        let duration = match unit {
            's' => Duration::try_seconds(amount),
            'm' => Duration::try_minutes(amount),
            'h' => Duration::try_hours(amount),
            'w' => Duration::try_weeks(amount),
            _ => None,
        }
        .ok_or_else(|| anyhow!("invalid since window: {value}: value too large"))?;
        let since = now
            .checked_sub_signed(duration)
            .ok_or_else(|| anyhow!("invalid since window: {value}: value too large"))?;
        return Ok(since);
    }
    if is_calendar_date(trimmed) {
        let date = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
            .with_context(|| format!("invalid since date: {value}"))?;
        let datetime = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| anyhow!("invalid since date: {value}"))?;
        return Ok(datetime.and_utc());
    }
    Ok(chrono::DateTime::parse_from_rfc3339(trimmed)
        .with_context(|| format!("invalid since value: {value}"))?
        .with_timezone(&Utc))
}

fn split_relative_since_window(value: &str) -> Option<(&str, char)> {
    let unit = value.chars().next_back()?;
    if !matches!(unit, 's' | 'm' | 'h' | 'w') {
        return None;
    }
    let amount = value.strip_suffix(unit)?;
    (!amount.is_empty()).then_some((amount, unit))
}

fn is_calendar_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    fn fixed_now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 20, 18, 0, 0).unwrap()
    }

    #[test]
    fn parse_since_filter_supports_relative_windows_dates_and_rfc3339() {
        let now = fixed_now();
        assert_eq!(
            parse_since_filter_at("30s", now).unwrap(),
            now - Duration::seconds(30)
        );
        assert_eq!(
            parse_since_filter_at("15m", now).unwrap(),
            now - Duration::minutes(15)
        );
        assert_eq!(
            parse_since_filter_at("2h", now).unwrap(),
            now - Duration::hours(2)
        );
        assert_eq!(
            parse_since_filter_at("30d", now).unwrap(),
            now - Duration::days(30)
        );
        assert_eq!(
            parse_since_filter_at("2w", now).unwrap(),
            now - Duration::weeks(2)
        );
        assert_eq!(
            parse_since_filter_at("2026-11-15", now).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 15, 0, 0, 0).unwrap()
        );
        assert_eq!(
            parse_since_filter_at("2026-11-15T12:30:00Z", now).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 15, 12, 30, 0).unwrap()
        );
    }

    #[test]
    fn parse_since_filter_rejects_negative_overflow_and_invalid_dates() {
        let now = fixed_now();
        for invalid in [
            "",
            "-2h",
            "-1d",
            "500000000d",
            "999999999999999999h",
            "2026-02-30",
            "2026-13-01",
            "h",
            "1.5h",
        ] {
            assert!(
                parse_since_filter_at(invalid, now).is_err(),
                "expected error for {invalid:?}"
            );
        }
    }
}
