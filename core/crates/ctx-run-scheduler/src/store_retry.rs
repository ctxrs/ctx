use std::time::Duration;

pub const STORE_WRITE_RETRY_LIMIT: usize = 3;
const STORE_WRITE_RETRY_BASE_MS: u64 = 40;

pub fn is_transient_store_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("database is locked")
        || msg.contains("sqlite_busy")
        || msg.contains("database is busy")
}

fn store_write_retry_backoff_ms(attempt: usize) -> u64 {
    STORE_WRITE_RETRY_BASE_MS.saturating_mul(attempt as u64)
}

pub async fn sleep_store_write_retry(attempt: usize) {
    tokio::time::sleep(Duration::from_millis(store_write_retry_backoff_ms(attempt))).await;
}

#[cfg(test)]
mod tests {
    use super::{is_transient_store_error, store_write_retry_backoff_ms};

    #[test]
    fn classifies_sqlite_lock_errors_as_transient() {
        for message in [
            "database is locked",
            "SQLITE_BUSY",
            "database is busy",
            "wrapped: Database Is Locked",
        ] {
            assert!(is_transient_store_error(&anyhow::anyhow!(message)));
        }
    }

    #[test]
    fn leaves_non_lock_errors_fatal() {
        assert!(!is_transient_store_error(&anyhow::anyhow!(
            "unique constraint failed"
        )));
    }

    #[test]
    fn backoff_scales_with_attempt_number() {
        assert_eq!(store_write_retry_backoff_ms(1), 40);
        assert_eq!(store_write_retry_backoff_ms(2), 80);
        assert_eq!(store_write_retry_backoff_ms(3), 120);
    }
}
