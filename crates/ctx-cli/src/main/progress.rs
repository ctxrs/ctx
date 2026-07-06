#[allow(unused_imports)]
use super::*;

#[derive(Debug)]
pub(crate) struct ProgressState {
    pub(crate) started: Instant,
    pub(crate) last_emit: Option<Instant>,
    pub(crate) last_line_len: usize,
}

#[derive(Clone)]
pub(crate) struct ProgressReporter {
    pub(crate) mode: ProgressRenderMode,
    pub(crate) operation: &'static str,
    pub(crate) total_bytes: u64,
    pub(crate) state: Arc<Mutex<ProgressState>>,
}

pub(crate) struct ProgressLine {
    pub(crate) phase: &'static str,
    pub(crate) message: String,
    pub(crate) completed_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) completed_files: Option<usize>,
    pub(crate) total_files: Option<usize>,
    pub(crate) imported_events: Option<usize>,
    pub(crate) done: bool,
    pub(crate) force: bool,
}

pub(crate) fn progress_percent(completed: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    ((completed as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
}

pub(crate) fn progress_bar(percent: f64, width: usize) -> String {
    let filled = ((percent / 100.0) * width as f64).round() as usize;
    format!(
        "{}{}",
        "#".repeat(filled.min(width)),
        "-".repeat(width.saturating_sub(filled))
    )
}
