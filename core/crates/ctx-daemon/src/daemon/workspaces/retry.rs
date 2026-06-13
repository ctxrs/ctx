use std::time::Duration;

const GLOBAL_INDEX_WRITE_RETRY_LIMIT: usize = 3;
const GLOBAL_INDEX_WRITE_RETRY_BASE_MS: u64 = 40;

fn is_transient_store_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("database is locked")
        || msg.contains("sqlite_busy")
        || msg.contains("database is busy")
}

pub async fn retry_global_index_write<Fut>(mut op: impl FnMut() -> Fut) -> Result<(), anyhow::Error>
where
    Fut: std::future::Future<Output = Result<(), anyhow::Error>>,
{
    let mut attempt = 0usize;
    loop {
        match op().await {
            Ok(()) => return Ok(()),
            Err(err) => {
                if !is_transient_store_error(&err) || attempt >= GLOBAL_INDEX_WRITE_RETRY_LIMIT {
                    return Err(err);
                }
                attempt += 1;
                let backoff_ms = GLOBAL_INDEX_WRITE_RETRY_BASE_MS.saturating_mul(attempt as u64);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            }
        }
    }
}
