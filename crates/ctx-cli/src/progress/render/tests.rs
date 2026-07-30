use unicode_width::UnicodeWidthStr as _;

use super::*;
use crate::ui::{ColorMode, StreamKind, TestContext};

fn tty(width: usize, unicode: bool) -> RenderContext {
    RenderContext::for_test(
        TestContext::tty(StreamKind::Stderr, width)
            .color(ColorMode::Never)
            .unicode(unicode),
    )
}

fn assert_fits(frame: &ProgressFrame, context: &RenderContext) {
    let maximum = context.content_width().unwrap_or(1);
    assert!(
        frame
            .document()
            .render_plain()
            .lines()
            .all(|line| line.width() <= maximum),
        "progress exceeded {maximum} columns:\n{}",
        frame.document().render_plain()
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

fn determinate_snapshot() -> ProgressSnapshot<'static> {
    ProgressSnapshot::new(
        "import",
        "cataloging",
        "Cataloging provider history.",
        Duration::from_secs(12),
    )
    .with_bytes(2 * 1024 * 1024, Some(4 * 1024 * 1024))
    .with_files(3, Some(10))
    .with_imported_events(1_234)
}

#[test]
fn determinate_progress_is_a_deterministic_tty_transient_document() {
    let context = tty(80, true);
    let first = render_progress_snapshot(&determinate_snapshot(), &context);
    let second = render_progress_snapshot(&determinate_snapshot(), &context);

    assert_eq!(first, second);
    assert_eq!(first.kind(), ProgressFrameKind::Transient);
    let rendered = first.document().render_plain();
    assert!(rendered.starts_with("Importing history"));
    assert!(rendered.contains("50%\n"));
    assert!(rendered.contains("Cataloging provider history."));
    assert!(rendered.contains("Phase      Cataloging\n"));
    assert!(rendered.contains("Processed  2.0 / 4.0 MiB\n"));
    assert!(rendered.contains("Files      3 / 10\n"));
    assert!(rendered.contains("Events     1,234\n"));
    assert!(rendered.contains("Elapsed    12 seconds\n"));

    for width in [32, 48, 80, 120] {
        let context = tty(width, true);
        assert_fits(
            &render_progress_snapshot(&determinate_snapshot(), &context),
            &context,
        );
    }
}

#[test]
fn indeterminate_progress_has_no_fake_percentage_and_has_ascii_fallback() {
    let snapshot = ProgressSnapshot::new(
        "import",
        "discovering_sources",
        "Discovering configured history sources.",
        Duration::ZERO,
    );

    for width in [32, 48, 80, 120] {
        let context = tty(width, false);
        let frame = render_progress_snapshot(&snapshot, &context);
        assert_eq!(frame.kind(), ProgressFrameKind::Transient);
        assert_fits(&frame, &context);
        let rendered = frame.document().render_plain();
        assert!(!rendered.contains('%'));
        assert!(rendered.contains("...\n"));
        assert!(rendered.contains("measuring..."));
        assert!(rendered.contains("Discovering sources"));
    }
}

#[test]
fn forced_color_pipe_is_styled_but_append_only_and_cursor_free() {
    let forced_context =
        RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(ColorMode::Always));
    let frame = render_progress_snapshot(&determinate_snapshot(), &forced_context);
    let styled = frame.document().render(&forced_context);

    assert_eq!(frame.kind(), ProgressFrameKind::Snapshot);
    assert!(styled.contains("\u{1b}["));
    assert!(!styled.contains('\r'));
    for cursor_sequence in ["\u{1b}[1A", "\u{1b}[2K", "\u{1b}[H", "\u{1b}[?25"] {
        assert!(!styled.contains(cursor_sequence));
    }
    assert_eq!(strip_ansi(&styled), frame.document().render_plain());

    let auto_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    let auto = render_progress_snapshot(&determinate_snapshot(), &auto_context);
    assert_eq!(auto.kind(), ProgressFrameKind::Snapshot);
    assert!(!auto.document().render(&auto_context).contains('\u{1b}'));
}

#[test]
fn completed_progress_is_a_final_snapshot_and_control_safe() {
    let snapshot = ProgressSnapshot::new(
        "import",
        "published",
        "Published history\u{1b}[2J\nwithout changing JSON.",
        Duration::from_secs(65),
    )
    .with_bytes(4 * 1024, Some(4 * 1024))
    .finished();
    let context = tty(48, true);
    let frame = render_progress_snapshot(&snapshot, &context);
    let rendered = frame.document().render_plain();

    assert_eq!(frame.kind(), ProgressFrameKind::Snapshot);
    assert!(rendered.starts_with("✓ History import complete\n"));
    assert!(rendered.contains("\\x1b[2J"));
    assert!(rendered.contains("\\n"));
    assert!(rendered.contains("JSON"));
    assert!(rendered.contains("Processed  4.0 / 4.0 KiB"));
    assert!(rendered.contains("Elapsed    1 minute, 5 seconds"));
    assert!(!rendered.contains('\u{1b}'));
    assert_fits(&frame, &context);
}
