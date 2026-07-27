use std::time::{Duration, Instant};

use serde_json::Value;

use crate::progress::{format_byte_progress, format_bytes, format_count};

const DEFAULT_TERMINAL_WIDTH: usize = 80;
const MAX_PROGRESS_BAR_WIDTH: usize = 56;
const FIELD_LABEL_WIDTH: usize = 12;
const RATE_SMOOTHING_WEIGHT: f64 = 0.35;

#[derive(Debug, Clone, Copy)]
struct DashboardSample {
    at: Instant,
    completed_bytes: u64,
    indexed_records: u64,
    semantic_records: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct DashboardRates {
    bytes_per_second: Option<f64>,
    records_per_second: Option<f64>,
    semantic_records_per_second: Option<f64>,
}

#[derive(Debug, Default)]
pub(super) struct IndexDashboard {
    previous: Option<DashboardSample>,
    rates: DashboardRates,
}

impl IndexDashboard {
    pub(super) fn render(&mut self, status: &Value, terminal_width: usize, styled: bool) -> String {
        self.render_at(status, terminal_width, styled, Instant::now())
    }

    fn render_at(
        &mut self,
        status: &Value,
        terminal_width: usize,
        styled: bool,
        now: Instant,
    ) -> String {
        self.observe(status, now);
        render_dashboard(
            status,
            terminal_width.max(1),
            Paint::new(styled),
            self.rates,
        )
    }

    fn observe(&mut self, status: &Value, now: Instant) {
        let sample = DashboardSample {
            at: now,
            completed_bytes: u64_at(status, &["lexical", "completed_source_bytes"]),
            indexed_records: u64_at(status, &["lexical", "indexed_items"]),
            semantic_records: u64_at(status, &["semantic", "coverage", "embedded_items"]),
        };
        if let Some(previous) = self.previous {
            let elapsed = now.saturating_duration_since(previous.at).as_secs_f64();
            if elapsed >= 0.1 {
                update_rate(
                    &mut self.rates.bytes_per_second,
                    sample.completed_bytes,
                    previous.completed_bytes,
                    elapsed,
                );
                update_rate(
                    &mut self.rates.records_per_second,
                    sample.indexed_records,
                    previous.indexed_records,
                    elapsed,
                );
                update_rate(
                    &mut self.rates.semantic_records_per_second,
                    sample.semantic_records,
                    previous.semantic_records,
                    elapsed,
                );
            }
        }
        self.previous = Some(sample);
    }
}

pub(super) fn terminal_width() -> usize {
    platform_terminal_width()
        .or_else(environment_terminal_width)
        .unwrap_or(DEFAULT_TERMINAL_WIDTH)
        .max(1)
}

pub(super) fn color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map_or(true, |term| term != "dumb")
}

fn update_rate(rate: &mut Option<f64>, current: u64, previous: u64, elapsed: f64) {
    if current < previous {
        *rate = None;
        return;
    }
    let observed = current.saturating_sub(previous) as f64 / elapsed;
    *rate = Some(match *rate {
        Some(existing) => {
            existing * (1.0 - RATE_SMOOTHING_WEIGHT) + observed * RATE_SMOOTHING_WEIGHT
        }
        None => observed,
    });
}

fn render_dashboard(
    status: &Value,
    terminal_width: usize,
    paint: Paint,
    rates: DashboardRates,
) -> String {
    let mut lines = Vec::new();
    render_lexical(&mut lines, status, terminal_width, paint, rates);
    render_semantic(&mut lines, status, terminal_width, paint, rates);
    render_health(&mut lines, status, paint);
    lines.join("\n")
}

fn render_lexical(
    lines: &mut Vec<String>,
    status: &Value,
    terminal_width: usize,
    paint: Paint,
    rates: DashboardRates,
) {
    let lexical_status = string_at(status, &["lexical", "status"], "unknown");
    let failed = u64_at(status, &["lexical", "failed_inventory_units"]);
    let completed_bytes = u64_at(status, &["lexical", "completed_source_bytes"]);
    let total_bytes = u64_at(status, &["lexical", "total_source_bytes"]).max(completed_bytes);
    let indexed_records = usize_at(status, &["lexical", "indexed_items"]);
    let indexed_sessions = usize_at(status, &["lexical", "indexed_sessions"]);
    let ready = matches!(lexical_status.as_str(), "ready" | "empty");

    if ready {
        lines.push(paint.green_bold("✓ Your history is searchable"));
        lines.push(String::new());
        push_field(
            lines,
            "Processed",
            &format_bytes(total_bytes),
            terminal_width,
            paint,
        );
        push_field(
            lines,
            "Sessions",
            &format_count(indexed_sessions),
            terminal_width,
            paint,
        );
        push_field(
            lines,
            "Records",
            &format!("{} searchable", format_count(indexed_records)),
            terminal_width,
            paint,
        );
        if failed > 0 {
            push_failed_files(lines, failed, paint);
        }
        return;
    }

    if total_bytes == 0 {
        lines.push(paint.bold("Discovering your history…"));
    } else if completed_bytes >= total_bytes {
        lines.push(paint.bold("Finalizing your search index…"));
    } else {
        let percent = percent(completed_bytes, total_bytes);
        push_heading(
            lines,
            "Indexing your history",
            &format!("{percent}%"),
            terminal_width,
            paint,
        );
        lines.push(progress_bar(percent, terminal_width, paint));
    }

    lines.push(String::new());
    if total_bytes > 0 {
        push_field(
            lines,
            "Processed",
            &format_byte_progress(completed_bytes, total_bytes),
            terminal_width,
            paint,
        );
    }
    push_field(
        lines,
        "Sessions",
        &format!("{} indexed", format_count(indexed_sessions)),
        terminal_width,
        paint,
    );
    push_field(
        lines,
        "Records",
        &format!("{} searchable", format_count(indexed_records)),
        terminal_width,
        paint,
    );
    push_field(
        lines,
        "Throughput",
        &format_rate(rates.records_per_second, "records/sec"),
        terminal_width,
        paint,
    );
    push_field(
        lines,
        "Remaining",
        &format_remaining(completed_bytes, total_bytes, rates.bytes_per_second),
        terminal_width,
        paint,
    );

    if failed > 0 {
        push_failed_files(lines, failed, paint);
    }
}

fn push_failed_files(lines: &mut Vec<String>, failed: u64, paint: Paint) {
    lines.push(String::new());
    lines.push(format!(
        "{} {} need attention",
        paint.warning("!"),
        pluralized_count(failed, "history file", "history files")
    ));
}

fn render_semantic(
    lines: &mut Vec<String>,
    status: &Value,
    terminal_width: usize,
    paint: Paint,
    rates: DashboardRates,
) {
    lines.push(String::new());
    if !bool_at(status, &["semantic", "enabled"]) {
        lines.push(format!("{}  Off", paint.dim("Semantic search")));
        return;
    }

    let semantic_status = string_at(
        status,
        &["daemon", "jobs", "semantic_index", "status"],
        "unknown",
    );
    let embedded = u64_at(status, &["semantic", "coverage", "embedded_items"]);
    let searchable = u64_at(status, &["semantic", "coverage", "searchable_items"]).max(embedded);
    if matches!(semantic_status.as_str(), "ready" | "empty") && embedded >= searchable {
        lines.push(paint.green_bold("✓ Semantic search is ready"));
        if searchable > 0 {
            push_field(
                lines,
                "Embedded",
                &format!("{} records", format_count_u64(embedded)),
                terminal_width,
                paint,
            );
        }
        return;
    }

    if matches!(
        semantic_status.as_str(),
        "failed" | "stale_lock" | "unavailable" | "blocked"
    ) {
        lines.push(format!(
            "{}  {}",
            paint.bold("Semantic search"),
            paint.warning("Needs attention")
        ));
        let reason = string_at(
            status,
            &["daemon", "jobs", "semantic_index", "reason"],
            &semantic_status,
        );
        lines.push(format!("  {}", humanize(&reason)));
        return;
    }

    if searchable == 0 {
        lines.push(paint.bold("Semantic search"));
        lines.push("Preparing…".to_owned());
        return;
    }

    let semantic_percent = percent(embedded, searchable);
    push_heading(
        lines,
        "Semantic search",
        &format!("{semantic_percent}%"),
        terminal_width,
        paint,
    );
    lines.push(progress_bar(semantic_percent, terminal_width, paint));
    lines.push(String::new());
    push_field(
        lines,
        "Embedded",
        &format!(
            "{} / {} records",
            format_count_u64(embedded),
            format_count_u64(searchable)
        ),
        terminal_width,
        paint,
    );
    push_field(
        lines,
        "Throughput",
        &format_rate(rates.semantic_records_per_second, "records/sec"),
        terminal_width,
        paint,
    );
    push_field(
        lines,
        "Remaining",
        &format_remaining(embedded, searchable, rates.semantic_records_per_second),
        terminal_width,
        paint,
    );
}

fn render_health(lines: &mut Vec<String>, status: &Value, paint: Paint) {
    let lexical_pending = u64_at(status, &["lexical", "pending_inventory_units"]) > 0;
    let semantic_status = string_at(
        status,
        &["daemon", "jobs", "semantic_index", "status"],
        "unknown",
    );
    let semantic_pending = bool_at(status, &["semantic", "enabled"])
        && !matches!(semantic_status.as_str(), "ready" | "empty");
    if (lexical_pending || semantic_pending) && !bool_at(status, &["daemon", "running"]) {
        lines.push(String::new());
        lines.push(format!(
            "{} Background indexing stopped",
            paint.warning("!")
        ));
        lines.push("  Run `ctx doctor` for details.".to_owned());
    }
}

fn push_heading(
    lines: &mut Vec<String>,
    title: &str,
    trailing: &str,
    terminal_width: usize,
    paint: Paint,
) {
    let usable_width = terminal_width.saturating_sub(1).max(1);
    let required = title.chars().count() + trailing.chars().count() + 1;
    if required <= usable_width {
        let padding = usable_width - title.chars().count() - trailing.chars().count();
        lines.push(format!(
            "{}{}{}",
            paint.bold(title),
            " ".repeat(padding),
            trailing
        ));
    } else {
        lines.push(paint.bold(title));
        lines.push(trailing.to_owned());
    }
}

fn push_field(
    lines: &mut Vec<String>,
    label: &str,
    value: &str,
    terminal_width: usize,
    paint: Paint,
) {
    let usable_width = terminal_width.saturating_sub(1).max(1);
    let inline_width = FIELD_LABEL_WIDTH + value.chars().count();
    if inline_width <= usable_width {
        lines.push(format!("{} {}", paint.dim(&format!("{label:<11}")), value));
    } else {
        lines.push(paint.dim(label));
        lines.push(format!("  {value}"));
    }
}

fn progress_bar(percent: u64, terminal_width: usize, paint: Paint) -> String {
    let available = terminal_width.saturating_sub(3).max(1);
    let width = available.min(MAX_PROGRESS_BAR_WIDTH);
    let filled = ((width as u128 * percent.min(100) as u128) / 100) as usize;
    let completed = "━".repeat(filled);
    let remaining = "─".repeat(width.saturating_sub(filled));
    format!("{}{}", paint.cyan(&completed), paint.dim(&remaining))
}

fn percent(completed: u64, total: u64) -> u64 {
    if total == 0 {
        0
    } else {
        ((completed.min(total) as u128 * 100) / total as u128) as u64
    }
}

fn format_rate(rate: Option<f64>, unit: &str) -> String {
    match rate.filter(|rate| rate.is_finite() && *rate >= 0.05) {
        Some(rate) if rate < 10.0 => format!("{rate:.1} {unit}"),
        Some(rate) => format!("{} {unit}", format_count_u64(rate.round() as u64)),
        None => "measuring…".to_owned(),
    }
}

fn format_remaining(completed: u64, total: u64, rate: Option<f64>) -> String {
    if completed >= total {
        return "finalizing…".to_owned();
    }
    let Some(rate) = rate.filter(|rate| rate.is_finite() && *rate >= 0.05) else {
        return "estimating…".to_owned();
    };
    let seconds = ((total - completed) as f64 / rate)
        .max(1.0)
        .min(u64::MAX as f64) as u64;
    format_duration(Duration::from_secs(seconds))
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs().max(1);
    if seconds < 60 {
        format!("about {seconds} seconds")
    } else if seconds < 3_600 {
        let minutes = (seconds + 30) / 60;
        format!(
            "about {minutes} {}",
            if minutes == 1 { "minute" } else { "minutes" }
        )
    } else {
        let hours = (seconds + 1_800) / 3_600;
        format!(
            "about {hours} {}",
            if hours == 1 { "hour" } else { "hours" }
        )
    }
}

fn pluralized_count(value: u64, singular: &str, plural: &str) -> String {
    format!(
        "{} {}",
        format_count_u64(value),
        if value == 1 { singular } else { plural }
    )
}

fn format_count_u64(value: u64) -> String {
    usize::try_from(value)
        .map(format_count)
        .unwrap_or_else(|_| value.to_string())
}

fn humanize(value: &str) -> String {
    value.replace('_', " ")
}

#[derive(Debug, Clone, Copy)]
struct Paint {
    enabled: bool,
}

impl Paint {
    const fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    fn bold(self, value: &str) -> String {
        self.wrap("1", value)
    }

    fn green_bold(self, value: &str) -> String {
        self.wrap("1;32", value)
    }

    fn cyan(self, value: &str) -> String {
        self.wrap("36", value)
    }

    fn dim(self, value: &str) -> String {
        self.wrap("2", value)
    }

    fn warning(self, value: &str) -> String {
        self.wrap("33", value)
    }

    fn wrap(self, code: &str, value: &str) -> String {
        if self.enabled && !value.is_empty() {
            format!("\u{1b}[{code}m{value}\u{1b}[0m")
        } else {
            value.to_owned()
        }
    }
}

#[cfg(unix)]
fn platform_terminal_width() -> Option<usize> {
    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    // SAFETY: TIOCGWINSZ initializes `winsize` for the valid stdout descriptor.
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, size.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    // SAFETY: ioctl succeeded and initialized the structure.
    let columns = unsafe { size.assume_init() }.ws_col;
    (columns > 0).then_some(usize::from(columns))
}

#[cfg(windows)]
fn platform_terminal_width() -> Option<usize> {
    use windows_sys::Win32::System::Console::{
        GetConsoleScreenBufferInfo, GetStdHandle, CONSOLE_SCREEN_BUFFER_INFO, STD_OUTPUT_HANDLE,
    };

    // SAFETY: the Windows console APIs initialize the provided structure on success.
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() {
            return None;
        }
        let mut info = std::mem::zeroed::<CONSOLE_SCREEN_BUFFER_INFO>();
        if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
            return None;
        }
        let columns = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
        usize::try_from(columns).ok().filter(|columns| *columns > 0)
    }
}

#[cfg(not(any(unix, windows)))]
fn platform_terminal_width() -> Option<usize> {
    None
}

fn environment_terminal_width() -> Option<usize> {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn string_at(value: &Value, path: &[&str], default: &str) -> String {
    value_at(value, path)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn bool_at(value: &Value, path: &[&str]) -> bool {
    value_at(value, path)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn u64_at(value: &Value, path: &[&str]) -> u64 {
    value_at(value, path).and_then(Value::as_u64).unwrap_or(0)
}

fn usize_at(value: &Value, path: &[&str]) -> usize {
    u64_at(value, path).try_into().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn status(semantic_enabled: bool) -> Value {
        json!({
            "lexical": {
                "status": "partial",
                "indexed_items": 854_466,
                "indexed_sessions": 3_486,
                "completed_source_bytes": 10_700_000_000_u64,
                "total_source_bytes": 13_600_000_000_u64,
                "pending_inventory_units": 947,
                "failed_inventory_units": 0,
            },
            "semantic": {
                "enabled": semantic_enabled,
                "coverage": {
                    "embedded_items": 357_421,
                    "searchable_items": 854_466,
                },
            },
            "daemon": {
                "running": true,
                "jobs": {
                    "semantic_index": {
                        "status": if semantic_enabled { "pending" } else { "disabled" },
                    },
                },
            },
        })
    }

    #[test]
    fn wide_dashboard_uses_aligned_rows_without_internal_statuses() {
        let rendered = render_dashboard(
            &status(false),
            80,
            Paint::new(false),
            DashboardRates {
                bytes_per_second: Some(20_000_000.0),
                records_per_second: Some(5_200.0),
                semantic_records_per_second: None,
            },
        );

        assert!(rendered.contains("Indexing your history"));
        assert!(rendered.lines().next().unwrap().ends_with("78%"));
        assert!(rendered.contains("Processed   10.0 / 12.7 GiB"));
        assert!(rendered.contains("Sessions    3,486 indexed"));
        assert!(rendered.contains("Records     854,466 searchable"));
        assert!(rendered.contains("Throughput  5,200 records/sec"));
        assert!(rendered.contains("Semantic search  Off"));
        assert!(!rendered.contains("partial"));
        assert!(!rendered.contains("daemon"));
        assert!(!rendered.contains('\u{1b}'));
    }

    #[test]
    fn narrow_dashboard_breaks_heading_and_fields_deliberately() {
        let rendered = render_dashboard(
            &status(false),
            24,
            Paint::new(false),
            DashboardRates::default(),
        );
        let lines = rendered.lines().collect::<Vec<_>>();

        assert_eq!(lines[0], "Indexing your history");
        assert_eq!(lines[1], "78%");
        assert!(rendered.contains("Processed\n  10.0 / 12.7 GiB"));
        assert!(rendered.contains("Records\n  854,466 searchable"));
        assert!(lines.iter().all(|line| line.chars().count() < 24));
    }

    #[test]
    fn enabled_semantic_search_has_independent_progress() {
        let rendered = render_dashboard(
            &status(true),
            52,
            Paint::new(false),
            DashboardRates {
                bytes_per_second: Some(20_000_000.0),
                records_per_second: Some(5_200.0),
                semantic_records_per_second: Some(2_100.0),
            },
        );

        assert!(rendered.contains("Semantic search"));
        assert!(rendered.contains("41%"));
        assert!(rendered.contains("Embedded    357,421 / 854,466 records"));
        assert!(rendered.contains("Throughput  2,100 records/sec"));
    }

    #[test]
    fn dashboard_rates_are_derived_from_successive_snapshots() {
        let mut dashboard = IndexDashboard::default();
        let first = status(false);
        let mut second = first.clone();
        second["lexical"]["completed_source_bytes"] = json!(10_740_000_000_u64);
        second["lexical"]["indexed_items"] = json!(864_866);
        let started = Instant::now();

        let first_render = dashboard.render_at(&first, 80, false, started);
        let second_render =
            dashboard.render_at(&second, 80, false, started + Duration::from_secs(2));

        assert!(first_render.contains("Throughput  measuring…"));
        assert!(second_render.contains("Throughput  5,200 records/sec"));
        assert!(second_render.contains("Remaining   about 2 minutes"));
    }

    #[test]
    fn ready_dashboard_is_a_completion_summary() {
        let mut ready = status(false);
        ready["lexical"]["status"] = json!("ready");
        ready["lexical"]["pending_inventory_units"] = json!(0);
        ready["lexical"]["completed_source_bytes"] = json!(13_600_000_000_u64);
        let rendered = render_dashboard(&ready, 80, Paint::new(false), DashboardRates::default());

        assert!(rendered.starts_with("✓ Your history is searchable"));
        assert!(rendered.contains("Processed   12.7 GiB"));
        assert!(rendered.contains("Sessions    3,486"));
        assert!(!rendered.contains("Remaining"));
    }

    #[test]
    fn missing_store_is_not_reported_as_ready() {
        let mut missing = status(false);
        missing["lexical"]["status"] = json!("missing");
        missing["lexical"]["pending_inventory_units"] = json!(0);
        missing["lexical"]["completed_source_bytes"] = json!(0);
        missing["lexical"]["total_source_bytes"] = json!(0);
        let rendered = render_dashboard(&missing, 80, Paint::new(false), DashboardRates::default());

        assert!(rendered.starts_with("Discovering your history…"));
        assert!(!rendered.contains("Your history is searchable"));
    }

    #[test]
    fn style_codes_are_only_emitted_when_requested() {
        let plain = render_dashboard(
            &status(false),
            80,
            Paint::new(false),
            DashboardRates::default(),
        );
        let styled = render_dashboard(
            &status(false),
            80,
            Paint::new(true),
            DashboardRates::default(),
        );

        assert!(!plain.contains('\u{1b}'));
        assert!(styled.contains("\u{1b}[1m"));
        assert!(styled.contains("\u{1b}[36m"));
    }
}
