use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use uuid::Uuid;

pub(super) fn new_idempotency_key(domain: &str) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("invalid_response: system clock is before Unix epoch")?
        .as_secs();
    Ok(format!("ctx-{domain}-v1-{now}-{}", Uuid::new_v4()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_identity_carries_one_bounded_server_verifiable_time() {
        let key = new_idempotency_key("cli").unwrap();
        let fields = key.split('-').collect::<Vec<_>>();
        assert_eq!(fields[0..3], ["ctx", "cli", "v1"]);
        assert!(fields[3].parse::<u64>().is_ok());
        assert_eq!(key.len(), "ctx-cli-v1--".len() + fields[3].len() + 36);
        assert!(key.len() <= 128);
    }
}
