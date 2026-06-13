use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;

const RESOURCE_LOG_PREFIX: &str = "resource-util-";
const RESOURCE_LOG_SUFFIX: &str = ".jsonl";

pub fn resource_telemetry_cleanup_key(occurred_at: DateTime<Utc>) -> String {
    occurred_at.format("%Y-%m-%d").to_string()
}

pub fn resource_telemetry_log_path(logs_dir: &Path, occurred_at: DateTime<Utc>) -> PathBuf {
    logs_dir.join(resource_telemetry_log_file_name(occurred_at.date_naive()))
}

pub async fn append_resource_telemetry_log<T: Serialize + ?Sized>(
    logs_dir: &Path,
    occurred_at: DateTime<Utc>,
    event: &T,
    max_bytes: u64,
) -> Result<()> {
    tokio::fs::create_dir_all(logs_dir).await?;
    let path = resource_telemetry_log_path(logs_dir, occurred_at);

    if max_bytes > 0 {
        if let Ok(metadata) = tokio::fs::metadata(&path).await {
            if metadata.len() >= max_bytes {
                return Ok(());
            }
        }
    }

    let line = serde_json::to_string(event)?;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    use tokio::io::AsyncWriteExt;
    file.write_all(line.as_bytes()).await?;
    file.write_all(b"\n").await?;
    file.flush().await?;
    Ok(())
}

pub async fn cleanup_old_resource_telemetry_logs(
    logs_dir: &Path,
    now: DateTime<Utc>,
    retention_days: u64,
) -> Result<usize> {
    let mut entries = tokio::fs::read_dir(logs_dir).await?;
    let retention_days = i64::try_from(retention_days).unwrap_or(i64::MAX);
    let cutoff = now - chrono::Duration::days(retention_days);
    let mut removed = 0usize;
    while let Some(entry) = entries.next_entry().await? {
        let file_name = entry.file_name().to_string_lossy().to_string();
        let Some(date) = parse_resource_telemetry_log_date(&file_name) else {
            continue;
        };
        if date < cutoff {
            tokio::fs::remove_file(entry.path()).await?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn resource_telemetry_log_file_name(date: NaiveDate) -> String {
    format!(
        "{RESOURCE_LOG_PREFIX}{}{RESOURCE_LOG_SUFFIX}",
        date.format("%Y-%m-%d")
    )
}

fn parse_resource_telemetry_log_date(file_name: &str) -> Option<DateTime<Utc>> {
    if !file_name.starts_with(RESOURCE_LOG_PREFIX) || !file_name.ends_with(RESOURCE_LOG_SUFFIX) {
        return None;
    }
    let date = file_name
        .trim_start_matches(RESOURCE_LOG_PREFIX)
        .trim_end_matches(RESOURCE_LOG_SUFFIX);
    let date = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let naive = date.and_hms_opt(0, 0, 0)?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::TimeZone;
    use serde::Serialize;

    use super::*;

    #[derive(Serialize)]
    struct TestEvent {
        value: &'static str,
    }

    #[test]
    fn resource_telemetry_log_path_uses_utc_day() {
        let logs_dir = Path::new("/var/log/ctx");
        let occurred_at = utc_time(2026, 5, 8);

        assert_eq!(
            resource_telemetry_log_path(logs_dir, occurred_at),
            PathBuf::from("/var/log/ctx/resource-util-2026-05-08.jsonl")
        );
        assert_eq!(resource_telemetry_cleanup_key(occurred_at), "2026-05-08");
    }

    #[tokio::test]
    async fn append_resource_telemetry_log_respects_existing_max_bytes() {
        let logs_dir = temp_logs_dir("append_cap");
        let occurred_at = utc_time(2026, 5, 8);
        let event = TestEvent { value: "first" };

        append_resource_telemetry_log(&logs_dir, occurred_at, &event, 1024)
            .await
            .expect("write first resource log event");
        let path = resource_telemetry_log_path(&logs_dir, occurred_at);
        let first = tokio::fs::read_to_string(&path)
            .await
            .expect("read first resource log event");
        append_resource_telemetry_log(&logs_dir, occurred_at, &TestEvent { value: "second" }, 1)
            .await
            .expect("skip capped resource log event");
        let second = tokio::fs::read_to_string(&path)
            .await
            .expect("read capped resource log event");

        assert_eq!(second, first);
        let _ = std::fs::remove_dir_all(&logs_dir);
    }

    #[tokio::test]
    async fn cleanup_old_resource_telemetry_logs_removes_only_expired_owned_logs() {
        let logs_dir = temp_logs_dir("cleanup");
        tokio::fs::create_dir_all(&logs_dir)
            .await
            .expect("create log dir");
        let now = utc_time(2026, 5, 8);
        tokio::fs::write(logs_dir.join("resource-util-2026-05-01.jsonl"), b"old\n")
            .await
            .expect("write old resource log");
        tokio::fs::write(logs_dir.join("resource-util-2026-05-07.jsonl"), b"fresh\n")
            .await
            .expect("write fresh resource log");
        tokio::fs::write(logs_dir.join("other-2026-05-01.jsonl"), b"other\n")
            .await
            .expect("write unrelated log");

        let removed = cleanup_old_resource_telemetry_logs(&logs_dir, now, 3)
            .await
            .expect("cleanup resource logs");

        assert_eq!(removed, 1);
        assert!(
            tokio::fs::metadata(logs_dir.join("resource-util-2026-05-01.jsonl"))
                .await
                .is_err()
        );
        assert!(
            tokio::fs::metadata(logs_dir.join("resource-util-2026-05-07.jsonl"))
                .await
                .is_ok()
        );
        assert!(tokio::fs::metadata(logs_dir.join("other-2026-05-01.jsonl"))
            .await
            .is_ok());
        let _ = std::fs::remove_dir_all(&logs_dir);
    }

    fn utc_time(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .expect("valid UTC date")
    }

    fn temp_logs_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ctx-resource-telemetry-log-{label}-{}-{nanos}",
            std::process::id()
        ))
    }
}
