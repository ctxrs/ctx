const RETRY_BASE_MS: u64 = 10_000;
const RETRY_MAX_MS: u64 = 5 * 60 * 1_000;

pub(super) fn retry_delay_ms(consecutive_failures: u32) -> u64 {
    let exponent = consecutive_failures.saturating_sub(1).min(63);
    RETRY_BASE_MS
        .checked_mul(1_u64 << exponent)
        .unwrap_or(RETRY_MAX_MS)
        .min(RETRY_MAX_MS)
}
