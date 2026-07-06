#[allow(unused_imports)]
use super::*;

pub(crate) fn render_progress_line(line: &ProgressLine, elapsed: StdDuration) -> String {
    let percent = progress_percent(line.completed_bytes, line.total_bytes);
    let bar = progress_bar(percent, 20);
    let eta = eta_seconds(line.completed_bytes, line.total_bytes, elapsed)
        .map(format_seconds)
        .unwrap_or_else(|| "estimating".to_owned());
    let files = match (line.completed_files, line.total_files) {
        (Some(done), Some(total)) if total > 0 => format!(" {done}/{total} files"),
        _ => String::new(),
    };
    let events = line
        .imported_events
        .map(|events| format!(" {events} events"))
        .unwrap_or_default();
    let remaining = if line.done {
        "done".to_owned()
    } else {
        format!("{eta} left")
    };
    format!(
        "{:<10} [{}] {:>5.1}% {}/{}{}{} {} - {}",
        line.phase,
        bar,
        percent,
        format_bytes(line.completed_bytes),
        format_bytes(line.total_bytes),
        files,
        events,
        remaining,
        line.message
    )
}
