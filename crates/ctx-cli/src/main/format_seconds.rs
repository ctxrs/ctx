#[allow(unused_imports)]
use super::*;

pub(crate) fn format_seconds(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        let minutes = seconds / 60;
        let rem = seconds % 60;
        format!("{minutes}m{rem:02}s")
    }
}
