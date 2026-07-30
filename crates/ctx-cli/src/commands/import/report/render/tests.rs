use ctx_history_capture::ProviderImportWorkResult;
use serde_json::json;
use unicode_width::UnicodeWidthStr as _;

use super::*;
use crate::commands::import::{ImportReport, ImportTotals};
use crate::ui::{ColorMode, StreamKind, TestContext};

fn context(width: usize) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
}

fn report(totals: ImportTotals, sources: Vec<Value>) -> ImportReport {
    ImportReport {
        resume: false,
        totals,
        sources,
    }
}

fn assert_fits(document: &Document, context: &RenderContext) {
    let maximum = context.content_width().unwrap_or(1);
    assert!(
        document
            .render_plain()
            .lines()
            .all(|line| line.width() <= maximum),
        "document exceeded {maximum} columns:\n{}",
        document.render_plain()
    );
}

fn strip_ansi(rendered: &str) -> String {
    let mut plain = String::with_capacity(rendered.len());
    let mut chars = rendered.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' || chars.peek() != Some(&'[') {
            plain.push(character);
            continue;
        }
        chars.next();
        for code in chars.by_ref() {
            if ('@'..='~').contains(&code) {
                break;
            }
        }
    }
    plain
}

#[test]
fn empty_import_is_concise_and_has_one_next_command() {
    let rendered =
        render_import_completion(&report(ImportTotals::default(), Vec::new()), &context(80))
            .render_plain();

    assert_eq!(
        rendered,
        concat!(
            "No history changes found\n",
            "The configured sources did not add searchable history.\n\n",
            "Next\n",
            "  ctx sources\n",
        )
    );
    assert_eq!(rendered.matches("\nNext\n").count(), 1);
}

#[test]
fn successful_import_leads_with_outcome_and_only_useful_totals() {
    let totals = ImportTotals {
        per_run_counts_available: true,
        source_files: 2,
        source_bytes: 2_660,
        imported_sources: 1,
        imported_sessions: 2,
        imported_events: 7,
        skipped_events: 1,
        work_result: ProviderImportWorkResult::Changed,
        ..ImportTotals::default()
    };
    let rendered =
        render_import_completion(&report(totals, Vec::new()), &context(80)).render_plain();

    assert_eq!(
        rendered,
        concat!(
            "✓ History import completed\n\n",
            "Import\n",
            "Sources         1\n",
            "Sessions        2\n",
            "Events          7\n",
            "Processed       2.6 KiB from 2 files\n",
            "Skipped events  1\n\n",
            "Next\n",
            "  ctx search \"your query\"\n",
        )
    );
    for internal in ["failure_scope", "resume", "generation", "path", "Edges"] {
        assert!(!rendered.contains(internal));
    }
}

#[test]
fn partial_import_separates_failures_from_rejections() {
    let totals = ImportTotals {
        per_run_counts_available: true,
        source_files: 3,
        source_bytes: 2_670,
        imported_sources: 1,
        sources_completed_with_rejections: 1,
        failed_sources: 1,
        imported_sessions: 2,
        imported_events: 7,
        failed: 4,
        work_result: ProviderImportWorkResult::Changed,
        ..ImportTotals::default()
    };
    let sources = vec![
        json!({
            "status": "failed",
            "provider": "opencode",
            "path": "/history/opencode.db",
            "error": "file is not a SQLite database",
            "published_generation": "internal-generation",
        }),
        json!({
            "status": "published",
            "provider": "codex",
            "path": "/history/codex",
        }),
    ];
    let rendered = render_import_completion(&report(totals, sources), &context(120)).render_plain();

    assert!(rendered.starts_with("! History import completed with issues\n"));
    assert!(rendered.contains("\nFailures\n"));
    assert!(
        rendered.contains("opencode: file is not a SQLite database; source /history/opencode.db")
    );
    assert!(rendered.contains("\nRejections\n"));
    assert!(
        rendered.find("\nFailures\n").unwrap_or(usize::MAX)
            < rendered.find("\nRejections\n").unwrap_or(0)
    );
    assert!(rendered.ends_with("\nNext\n  ctx sources\n"));
    assert_eq!(rendered.matches("\nNext\n").count(), 1);
    assert!(!rendered.contains("internal-generation"));
    assert!(!rendered.contains("/history/codex"));
}

#[test]
fn failed_import_is_error_first_and_control_safe() {
    let totals = ImportTotals {
        per_run_counts_available: true,
        source_files: 2,
        source_bytes: 12,
        failed_sources: 2,
        work_result: ProviderImportWorkResult::NoOp,
        ..ImportTotals::default()
    };
    let sources = vec![
        json!({
            "status": "failed",
            "provider": "codex",
            "path": "/history/\tprivate",
            "error": "permission denied\u{1b}[2J\nretry after fixing access",
        }),
        json!({
            "status": "failed",
            "provider": "claude",
            "error": "source could not be opened",
        }),
    ];
    let rendered = render_import_completion(&report(totals, sources), &context(80)).render_plain();

    assert!(rendered.starts_with("✗ History import failed\n"));
    assert!(rendered.contains("\nFailures\n"));
    assert!(rendered.contains("\\x1b[2J\\nretry after fixing access"));
    assert!(rendered.contains("/history/\\tprivate"));
    assert!(!rendered.contains('\u{1b}'));
    assert!(!rendered.contains('\t'));
    assert!(rendered.ends_with("\nNext\n  ctx sources\n"));
    assert!(!rendered.contains("\nRejections\n"));
}

#[test]
fn large_totals_fit_supported_widths_and_preserve_plain_ansi_parity() {
    let totals = ImportTotals {
        per_run_counts_available: true,
        source_files: 1_234_567,
        source_bytes: 5 * 1024_u64.pow(4) + 200 * 1024_u64.pow(3),
        imported_sources: 98_765,
        imported_sessions: 9_876_543,
        imported_events: 987_654_321,
        skipped_sessions: 12_345,
        skipped_events: 67_890,
        work_result: ProviderImportWorkResult::Changed,
        ..ImportTotals::default()
    };
    let report = report(totals, Vec::new());

    for width in [32, 48, 80, 120] {
        let context = context(width);
        let document = render_import_completion(&report, &context);
        assert_fits(&document, &context);
        let rendered = document.render_plain();
        assert!(rendered.contains("98,765"));
        assert!(rendered.contains("987,654,321"));
        assert!(rendered.contains("5.2 TiB"));
    }

    let plain_context = context(80);
    let styled_context =
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Always));
    let document = render_import_completion(&report, &plain_context);
    let styled = document.render(&styled_context);
    assert!(styled.contains("\u{1b}["));
    assert_eq!(strip_ansi(&styled), document.render_plain());

    let ascii_context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, 80)
            .color(ColorMode::Never)
            .unicode(false),
    );
    assert!(render_import_completion(&report, &ascii_context)
        .render_plain()
        .starts_with("OK History import completed\n"));
}
