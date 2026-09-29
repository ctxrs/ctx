//! Human-readable resource quantities. Calculations and structured output keep exact bytes.

pub fn format_bytes(bytes: u64) -> String {
    let (value, unit) = scaled_bytes(bytes);
    if unit == "B" {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {unit}")
    }
}

fn scaled_bytes(bytes: u64) -> (f64, &'static str) {
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < BYTE_UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    (value, BYTE_UNITS[unit])
}

const BYTE_UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

/// Describe a disk shortage using exact quantities and an upward-rounded deficit.
/// Callers supply the affected operation and filesystem context.
pub fn format_disk_shortage(required: u64, available: u64) -> String {
    format!(
        "free at least {}; required {}; available {}",
        format_bytes_up(required.saturating_sub(available)),
        format_bytes(required),
        format_bytes(available),
    )
}

fn format_bytes_up(bytes: u64) -> String {
    if bytes < 1024 {
        return format_bytes(bytes);
    }
    let unit = (bytes.ilog(1024) as usize).min(BYTE_UNITS.len() - 1);
    let divisor = 1024_u128.pow(unit as u32);
    // u128 keeps both the multiplication and ceiling exact across all u64 inputs.
    let tenths = (u128::from(bytes) * 10).div_ceil(divisor);
    format!("{}.{} {}", tenths / 10, tenths % 10, BYTE_UNITS[unit])
}

#[cfg(test)]
mod tests;
