use std::{
    ffi::OsString,
    io::{self, Write},
    sync::{Arc, Mutex},
};

use unicode_width::UnicodeWidthStr as _;

use super::{
    bootstrap::{scan_color_mode, scan_machine_output_hint},
    callout, canonical_human_output_bytes, diagnostic, empty_state, evidence_list, fields, hint,
    outcome, progress, sanitize_untrusted_history_body_for_terminal, section, table, Action,
    Callout, ColorMode, Diagnostic, DiagnosticLevel, Document, EmptyState, Evidence, Field, Hint,
    Line, Outcome, OutcomeState, Progress, RenderContext, Span, StreamKind, Table, TestContext,
    Token, Ui,
};

fn tty(width: usize) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width))
}

fn strip_ansi(rendered: &str) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    stream.write_all(rendered.as_bytes()).unwrap();
    String::from_utf8(stream.into_inner()).unwrap()
}

fn assert_within_terminal(document: &Document, context: &RenderContext) {
    let plain = document.render_plain();
    let available = context.content_width().unwrap();
    for line in plain.lines() {
        assert!(
            line.width() <= available,
            "{line:?} is {} columns in a {available}-column content area",
            line.width()
        );
    }
}

#[test]
fn render_context_resolves_color_width_and_unicode_explicitly() {
    let auto =
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Auto));
    assert!(auto.color_enabled());
    assert_eq!(auto.terminal_width(), Some(80));
    assert_eq!(auto.content_width(), Some(79));
    assert!(auto.unicode());

    let no_color = RenderContext::for_test(
        TestContext::tty(StreamKind::Stderr, 48)
            .color(ColorMode::Auto)
            .no_color(true),
    );
    assert!(!no_color.color_enabled());

    let dumb = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, 48)
            .color(ColorMode::Auto)
            .term_dumb(true),
    );
    assert!(!dumb.color_enabled());

    let injected_auto = RenderContext::for_test(
        TestContext::pipe(StreamKind::Stdout)
            .color(ColorMode::Auto)
            .auto_color(true),
    );
    assert!(
        injected_auto.color_enabled(),
        "injected contexts preserve anstream-equivalent auto decisions"
    );

    let forced_pipe = RenderContext::for_test(
        TestContext::pipe(StreamKind::Stdout)
            .color(ColorMode::Always)
            .unicode(false),
    );
    assert!(forced_pipe.color_enabled());
    assert!(!forced_pipe.is_terminal());
    assert_eq!(forced_pipe.terminal_width(), None);
    assert_eq!(forced_pipe.content_width(), None);
    assert!(!forced_pipe.unicode());

    let never =
        RenderContext::for_test(TestContext::tty(StreamKind::Stderr, 120).color(ColorMode::Never));
    assert!(!never.color_enabled());

    let unknown = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 32).unknown_width());
    assert_eq!(unknown.terminal_width(), Some(80));

    let zero = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 0));
    assert_eq!(zero.terminal_width(), Some(80));
}

#[test]
fn canonical_human_measurement_is_plain_unbounded_and_deterministic() {
    let context = RenderContext::canonical_human_measurement();
    assert_eq!(context.stream(), StreamKind::Stdout);
    assert_eq!(context.color_mode(), ColorMode::Never);
    assert!(!context.is_terminal());
    assert!(!context.color_enabled());
    assert_eq!(context.terminal_width(), None);
    assert_eq!(context.content_width(), None);
    assert!(context.unicode());

    let bytes = canonical_human_output_bytes(|measurement| {
        assert_eq!(*measurement, context);
        Document::from_line(Line::text("stable"))
    });
    assert_eq!(bytes, "stable\n".len());
}

#[test]
fn human_timestamps_default_to_deterministic_utc_in_test_contexts() {
    let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    assert_eq!(
        context.human_timestamp("2026-07-30T12:00:00.123Z"),
        "2026-07-30 12:00:00 UTC"
    );
}

#[test]
fn human_timestamps_follow_named_historical_dst_without_global_state() {
    let context = RenderContext::for_test(
        TestContext::pipe(StreamKind::Stdout).time_zone("America/New_York"),
    );
    assert_eq!(
        context.human_timestamp("2026-07-30T12:00:00.123Z"),
        "2026-07-30 08:00:00 EDT"
    );
    assert_eq!(
        context.human_timestamp("2026-01-30T12:00:00.123Z"),
        "2026-01-30 07:00:00 EST"
    );
    assert_eq!(
        context.human_timestamp("2026-03-08T06:59:59Z"),
        "2026-03-08 01:59:59 EST"
    );
    assert_eq!(
        context.human_timestamp("2026-03-08T07:00:00Z"),
        "2026-03-08 03:00:00 EDT"
    );
    assert_eq!(
        context.human_timestamp("2026-11-01T05:30:00Z"),
        "2026-11-01 01:30:00 EDT"
    );
    assert_eq!(
        context.human_timestamp("2026-11-01T06:30:00Z"),
        "2026-11-01 01:30:00 EST"
    );
}

#[test]
fn human_timestamps_preserve_malformed_input_and_fall_back_to_utc() {
    let missing_zone = RenderContext::for_test(
        TestContext::pipe(StreamKind::Stdout).time_zone("Etc/Definitely-Missing"),
    );
    assert_eq!(
        missing_zone.human_timestamp("2026-07-30T12:00:00.123Z"),
        "2026-07-30 12:00:00 UTC"
    );
    assert_eq!(
        missing_zone.human_timestamp("not-a-timestamp"),
        "not-a-timestamp"
    );
}

#[test]
fn bootstrap_scans_only_supported_global_color_spellings() {
    let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();

    assert_eq!(
        scan_color_mode(args(&["ctx", "--color", "always", "status"])),
        Some(ColorMode::Always)
    );
    assert_eq!(
        scan_color_mode(args(&["ctx", "status", "--color=never"])),
        Some(ColorMode::Never)
    );
    assert_eq!(
        scan_color_mode(args(&[
            "ctx",
            "--color=always",
            "status",
            "--color",
            "auto"
        ])),
        Some(ColorMode::Auto)
    );
    assert_eq!(
        scan_color_mode(args(&["ctx", "search", "--", "--color=always"])),
        None
    );
    assert_eq!(
        scan_color_mode(args(&["ctx", "--color", "sometimes", "status"])),
        None
    );
    assert_eq!(
        scan_color_mode(args(&["--color=always", "status"])),
        None,
        "argv[0] is never parsed as an option"
    );
}

#[test]
fn bootstrap_conservatively_recognizes_explicit_machine_output() {
    let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();

    assert!(scan_machine_output_hint(&args(&[
        "ctx", "show", "event", "bad", "--format", "jsonl"
    ])));
    assert!(scan_machine_output_hint(&args(&[
        "ctx",
        "setup",
        "--progress",
        "json"
    ])));
    assert!(scan_machine_output_hint(&args(&["ctx", "mcp", "serve"])));
    assert!(scan_machine_output_hint(&args(&[
        "ctx", "mcp", "--quiet", "serve"
    ])));
    assert!(!scan_machine_output_hint(&args(&[
        "ctx", "search", "failure"
    ])));
    assert!(!scan_machine_output_hint(&args(&[
        "ctx",
        "search",
        "--",
        "--format=json"
    ])));
}

#[test]
fn responsive_components_cover_32_48_80_and_120_columns() {
    let table_model = Table::new(["Provider", "State", "Evidence"])
        .row(["codex", "ready", "42 indexed events"])
        .row(["claude", "pending", "history catalog refresh"]);

    for width in [32, 48] {
        let context = tty(width);
        let rendered = table(&context, &table_model);
        let plain = rendered.render_plain();
        assert!(plain.starts_with("Provider\n  codex\nState\n  ready\n"));
        assert_within_terminal(&rendered, &context);
    }

    for width in [80, 120] {
        let context = tty(width);
        let rendered = table(&context, &table_model);
        let plain = rendered.render_plain();
        assert!(plain.starts_with("Provider  State    Evidence\n"));
        assert!(!plain.contains("\n  codex\n"));
        assert_within_terminal(&rendered, &context);
    }

    let narrow = tty(32);
    let field_document = fields(
        &narrow,
        &[
            Field::new("Repository", "ctxrs/ctx"),
            Field::new(
                "Evidence",
                "A deliberately long explanation that wraps without terminal help",
            ),
        ],
    );
    assert_within_terminal(&field_document, &narrow);

    let progress_document = progress(
        &narrow,
        Progress {
            label: "Indexing local agent history",
            current: 72,
            total: Some(100),
            detail: Some("854,466 records searchable"),
        },
    );
    assert_within_terminal(&progress_document, &narrow);
}

#[test]
fn decorative_glyphs_have_only_the_approved_ascii_fallbacks() {
    let unicode = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, 80)
            .color(ColorMode::Never)
            .unicode(true),
    );
    let ascii = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, 80)
            .color(ColorMode::Never)
            .unicode(false),
    );

    assert!(outcome(
        &unicode,
        Outcome {
            state: OutcomeState::Success,
            title: "Ready",
            detail: None,
        }
    )
    .render_plain()
    .starts_with("✓ Ready"));
    assert!(outcome(
        &ascii,
        Outcome {
            state: OutcomeState::Success,
            title: "Ready",
            detail: None,
        }
    )
    .render_plain()
    .starts_with("OK Ready"));
    assert!(outcome(
        &unicode,
        Outcome {
            state: OutcomeState::Error,
            title: "Failed",
            detail: None,
        }
    )
    .render_plain()
    .starts_with("✗ Failed"));
    assert!(outcome(
        &ascii,
        Outcome {
            state: OutcomeState::Error,
            title: "Failed",
            detail: None,
        }
    )
    .render_plain()
    .starts_with("X Failed"));

    let unicode_progress = progress(
        &unicode,
        Progress {
            label: "Indexing",
            current: 1,
            total: Some(2),
            detail: None,
        },
    )
    .render_plain();
    assert!(unicode_progress.contains('━'));
    assert!(unicode_progress.contains('─'));

    let ascii_progress = progress(
        &ascii,
        Progress {
            label: "Indexing",
            current: 1,
            total: Some(2),
            detail: None,
        },
    )
    .render_plain();
    assert!(ascii_progress.contains('='));
    assert!(ascii_progress.contains('-'));

    let indeterminate = progress(
        &ascii,
        Progress {
            label: "Discovering",
            current: 0,
            total: None,
            detail: None,
        },
    )
    .render_plain();
    assert!(indeterminate.contains("========"));
    assert!(indeterminate.contains('-'));

    let callout_body = Document::from_line(Line::text("Complete human output."));
    let unicode_callout = callout(
        &unicode,
        Callout {
            title: "Note",
            body: &callout_body,
        },
    )
    .render_plain();
    let ascii_callout = callout(
        &ascii,
        Callout {
            title: "Note",
            body: &callout_body,
        },
    )
    .render_plain();
    assert!(unicode_callout.starts_with('╭'));
    assert!(unicode_callout.trim_end().ends_with('╯'));
    assert!(ascii_callout.starts_with('+'));
    assert!(ascii_callout.trim_end().ends_with('+'));
}

#[test]
fn every_component_has_byte_identical_ansi_stripped_and_plain_output() {
    let context =
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Always));
    let field_values = [
        Field::new("Repository", "ctxrs/ctx"),
        Field::new("State", "ready"),
    ];
    let diagnostic_fields = [Field::new("Path", "/tmp/history.jsonl")];
    let table_value = Table::new(["Provider", "State"])
        .row(["codex", "ready"])
        .row(["claude", "pending"]);
    let callout_body = Document::from_line(
        Line::new()
            .with(Span::text("Local history remains "))
            .with(Span::new("on this device", Token::Accent)),
    );
    let documents = vec![
        callout(
            &context,
            Callout {
                title: "History stays local",
                body: &callout_body,
            },
        ),
        outcome(
            &context,
            Outcome {
                state: OutcomeState::Success,
                title: "History is searchable",
                detail: Some("The local index is current."),
            },
        ),
        section("Index", fields(&context, &field_values)),
        table(&context, &table_value),
        progress(
            &context,
            Progress {
                label: "Indexing",
                current: 3,
                total: Some(4),
                detail: Some("3 of 4 sources complete"),
            },
        ),
        empty_state(
            &context,
            EmptyState {
                title: "No sessions found",
                detail: "Import history before searching.",
                action: Some(Action {
                    command: "ctx import --all",
                }),
            },
        ),
        diagnostic(
            &context,
            Diagnostic {
                level: DiagnosticLevel::Error,
                summary: "History import failed",
                detail: Some("The source file could not be read."),
                fields: &diagnostic_fields,
                action: Some(Action {
                    command: "ctx doctor",
                }),
            },
        ),
        hint(
            &context,
            Hint {
                text: "Use a narrower provider filter.",
            },
            Some(Action {
                command: "ctx search query --provider codex",
            }),
        ),
        evidence_list(
            &context,
            &[Evidence {
                reference: "1",
                summary: "Session 018f",
                detail: Some("Provider: codex"),
            }],
        ),
    ];

    for document in documents {
        let ansi = document.render(&context);
        assert!(ansi.contains("\u{1b}["));
        assert_eq!(strip_ansi(&ansi), document.render_plain());
        assert!(!ansi.contains("\u{1b}[2J"));
        assert!(!ansi.contains("\u{1b}[?"));
        assert!(!ansi.contains('\r'));
    }
}

#[test]
fn component_values_cannot_inject_ansi_or_terminal_controls() {
    let attack = "\u{1b}[31mowned\u{1b}[0m\rrewrite\u{0000}\u{0085}\u{009b}2J\nnext\tcell";
    let context = tty(120);
    let document = fields(&context, &[Field::new("Value\u{1b}[2J", attack)]);
    let plain = document.render_plain();

    assert!(!plain.contains('\u{1b}'));
    assert!(!plain.contains('\r'));
    assert!(!plain.contains('\u{0000}'));
    assert!(!plain.contains('\u{0085}'));
    assert!(!plain.contains('\u{009b}'));
    assert!(plain.contains("\\x1b[31mowned\\x1b[0m"));
    assert!(plain.contains("\\u{0000}"));
    assert!(plain.contains("\\u{0085}"));
    assert!(plain.contains("\\u{009b}2J"));
    assert!(plain.contains("\\nnext\\tcell"));

    let direct = Document::from_line(
        Line::new()
            .with(Span::new(attack, Token::Text))
            .with(Span::new("\u{1b}[2A", Token::Heading)),
    )
    .render_plain();
    assert!(!direct.contains('\u{1b}'));
    assert!(direct.contains("\\x1b[2A"));
}

#[test]
fn shared_production_wrapping_preserves_legitimate_unicode_at_all_widths() {
    const FAMILY: &str = "👨‍👩‍👧‍👦";
    const PROFESSION: &str = "👩🏽‍💻";
    const PERSIAN_ZWNJ: &str = "می‌روم";
    const ARABIC_COMBINING: &str = "اَلْعَرَبِيَّةُ";
    const DECOMPOSED_LATIN: &str = "e\u{0301}";
    const RTL_LETTERS: &str = "مرحبا";
    const VARIATION_SELECTOR: &str = "✈\u{fe0f}";
    let body = format!(
        "Family {FAMILY} profession {PROFESSION} Persian {PERSIAN_ZWNJ} Arabic \
         {ARABIC_COMBINING} decomposed {DECOMPOSED_LATIN} RTL {RTL_LETTERS} variant \
         {VARIATION_SELECTOR}. This intentionally long history body traverses the shared \
         production wrapping path without changing legitimate Unicode grapheme content at \
         narrow or wide terminal sizes."
    );

    for width in [32, 48, 80, 120] {
        let context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Always),
        );
        let document = fields(&context, &[Field::new("Body", &body)]);
        let plain = document.render_plain();
        let styled = document.render(&context);

        for expected in [
            FAMILY,
            PROFESSION,
            PERSIAN_ZWNJ,
            ARABIC_COMBINING,
            DECOMPOSED_LATIN,
            RTL_LETTERS,
            VARIATION_SELECTOR,
        ] {
            assert!(
                plain.contains(expected),
                "width {width} changed {expected:?}"
            );
        }
        assert!(!plain.contains("\\u{200c}"), "width {width}");
        assert!(!plain.contains("\\u{200d}"), "width {width}");
        assert_eq!(strip_ansi(&styled), plain, "width {width}");
        assert_within_terminal(&document, &context);
    }
}

#[test]
fn global_spans_preserve_ordinary_unicode_exactly() {
    let ordinary = concat!("👨‍👩‍👧‍👦 👩🏽‍💻 می‌روم اَلْعَرَبِيَّةُ e\u{0301} ", "مرحبا ✈\u{fe0f}");

    assert_eq!(Span::text(ordinary).content(), ordinary);
    assert_eq!(
        Document::from_line(Line::styled(ordinary, Token::Heading)).render_plain(),
        format!("{ordinary}\n")
    );
}

#[test]
fn strict_history_sanitizer_escapes_disallowed_format_controls_exactly() {
    const CASES: &[(char, &str)] = &[
        ('\u{00ad}', "\\u{00ad}"),
        ('\u{0600}', "\\u{0600}"),
        ('\u{061c}', "\\u{061c}"),
        ('\u{115f}', "\\u{115f}"),
        ('\u{180e}', "\\u{180e}"),
        ('\u{2028}', "\\u{2028}"),
        ('\u{2029}', "\\u{2029}"),
        ('\u{2061}', "\\u{2061}"),
        ('\u{2062}', "\\u{2062}"),
        ('\u{2063}', "\\u{2063}"),
        ('\u{2064}', "\\u{2064}"),
        ('\u{2065}', "\\u{2065}"),
        ('\u{3164}', "\\u{3164}"),
        ('\u{ffa0}', "\\u{ffa0}"),
        ('\u{fff0}', "\\u{fff0}"),
        ('\u{fff9}', "\\u{fff9}"),
        ('\u{110bd}', "\\u{110bd}"),
        ('\u{13430}', "\\u{13430}"),
        ('\u{1bca0}', "\\u{1bca0}"),
        ('\u{1d173}', "\\u{1d173}"),
        ('\u{e0001}', "\\u{e0001}"),
        ('\u{e0020}', "\\u{e0020}"),
        ('\u{e007f}', "\\u{e007f}"),
        ('\u{e0080}', "\\u{e0080}"),
        ('\u{e01f0}', "\\u{e01f0}"),
    ];

    for (character, expected) in CASES {
        assert_eq!(
            sanitize_untrusted_history_body_for_terminal(&character.to_string()),
            *expected,
            "U+{:04X}",
            u32::from(*character)
        );
    }
}

#[test]
fn strict_history_sanitizer_preserves_text_shaping_and_generic_ui_behavior() {
    const PRESERVED: &str = concat!(
        "می\u{200c}روم ",
        "👩\u{200d}💻 ",
        "✈\u{fe0f} 字\u{e0100} ᠠ\u{180b} ",
        "e\u{0301} x\u{034f} ក\u{17b4} ",
        "مرحبا שלום"
    );
    const STRICT_ONLY: &str = "\u{2028}\u{2029}\u{2061}\u{2062}\u{2063}\u{2064}";

    assert_eq!(
        sanitize_untrusted_history_body_for_terminal(PRESERVED),
        PRESERVED
    );
    assert_eq!(
        Span::text(STRICT_ONLY).content(),
        STRICT_ONLY,
        "the generic UI sanitizer must remain unchanged"
    );
}

#[test]
fn untrusted_history_body_controls_are_visibly_escaped_before_layout() {
    const FORBIDDEN: &[char] = &[
        '\u{061c}', '\u{200b}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}',
        '\u{202d}', '\u{202e}', '\u{2060}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        '\u{206a}', '\u{206b}', '\u{206c}', '\u{206d}', '\u{206e}', '\u{206f}', '\u{feff}',
    ];
    const VISIBLE: &[&str] = &[
        "\\u{061c}",
        "\\u{200b}",
        "\\u{200e}",
        "\\u{200f}",
        "\\u{202a}",
        "\\u{202b}",
        "\\u{202c}",
        "\\u{202d}",
        "\\u{202e}",
        "\\u{2060}",
        "\\u{2066}",
        "\\u{2067}",
        "\\u{2068}",
        "\\u{2069}",
        "\\u{206a}",
        "\\u{206b}",
        "\\u{206c}",
        "\\u{206d}",
        "\\u{206e}",
        "\\u{206f}",
        "\\u{feff}",
    ];
    const LEGITIMATE: &str = "می‌روم 👩🏽‍💻 اَلْعَرَبِيَّةُ e\u{0301} مرحبا ✈\u{fe0f}";
    let controls = FORBIDDEN
        .iter()
        .map(char::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let visible_controls = VISIBLE.join(" ");
    let input = format!(
        "before \n \r \t \u{1b} \u{0000} \u{001f} \u{007f} \u{0085} \u{009b} \
         {controls} after {LEGITIMATE}"
    );
    let expected = format!(
        "before \\n \\r \\t \\x1b \\u{{0000}} \\u{{001f}} \\u{{007f}} \\u{{0085}} \\u{{009b}} \
         {visible_controls} after {LEGITIMATE}"
    );

    let sanitized = sanitize_untrusted_history_body_for_terminal(&input);
    assert_eq!(sanitized, expected);
    assert!(VISIBLE.iter().all(|escape| escape.is_ascii()));
    let dangerous_only = format!("\n\r\t\u{1b}\u{0000}\u{001f}\u{007f}\u{0085}\u{009b}{controls}");
    assert!(sanitize_untrusted_history_body_for_terminal(&dangerous_only).is_ascii());
    assert!(sanitized.contains(LEGITIMATE));
    for forbidden in FORBIDDEN {
        assert!(!sanitized.contains(*forbidden), "retained {forbidden:?}");
    }

    let context =
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Always));
    let document = fields(&context, &[Field::new("Body", &sanitized)]);
    let plain = document.render_plain();
    let styled = document.render(&context);
    assert_eq!(document.render_plain(), plain);
    assert_eq!(document.render(&context), styled);
    assert_eq!(strip_ansi(&styled), plain);
    for legitimate in ["می‌روم", "👩🏽‍💻", "اَلْعَرَبِيَّةُ", "e\u{0301}", "مرحبا", "✈\u{fe0f}"]
    {
        assert!(plain.contains(legitimate), "changed {legitimate:?}");
    }
    assert_within_terminal(&document, &context);
    for escape in VISIBLE {
        assert!(plain.contains(escape), "missing {escape}");
    }
    for forbidden in FORBIDDEN {
        assert!(!plain.contains(*forbidden), "plain retained {forbidden:?}");
        assert!(
            !styled.contains(*forbidden),
            "styled retained {forbidden:?}"
        );
    }
}

#[derive(Clone, Default)]
struct SharedWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl SharedWriter {
    fn bytes(&self) -> Vec<u8> {
        self.bytes.lock().unwrap().clone()
    }

    fn text(&self) -> String {
        String::from_utf8(self.bytes()).unwrap()
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .map_err(|_| io::Error::other("shared test writer was poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn ui_owns_independent_injectable_streams_and_capabilities() {
    let stdout = SharedWriter::default();
    let stdout_copy = stdout.clone();
    let stderr = SharedWriter::default();
    let stderr_copy = stderr.clone();
    let stdout_context =
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Always));
    let stderr_context = RenderContext::for_test(
        TestContext::pipe(StreamKind::Stderr)
            .color(ColorMode::Never)
            .unicode(false),
    );
    let mut ui = Ui::with_writers(stdout, stdout_context, stderr, stderr_context);
    let document = outcome(
        ui.stdout_context(),
        Outcome {
            state: OutcomeState::Warning,
            title: "Partial history",
            detail: None,
        },
    );

    ui.write_stdout(&document).unwrap();
    ui.write_stderr(&document).unwrap();
    ui.flush().unwrap();

    assert!(stdout_copy.text().contains("\u{1b}["));
    assert!(!stderr_copy.text().contains('\u{1b}'));
    assert_eq!(strip_ansi(&stdout_copy.text()), stderr_copy.text());
    assert_eq!(ui.context(StreamKind::Stdout).content_width(), Some(79));
    assert_eq!(ui.context(StreamKind::Stderr).content_width(), None);
}

#[test]
fn ui_writes_exact_framed_machine_bytes_to_the_selected_stream() {
    let stdout = SharedWriter::default();
    let stdout_copy = stdout.clone();
    let stderr = SharedWriter::default();
    let stderr_copy = stderr.clone();
    let stdout_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    let stderr_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    let mut ui = Ui::with_writers(stdout, stdout_context, stderr, stderr_context);

    ui.write_stdout_bytes(b"{\"stream\":\"stdout\"}\n").unwrap();
    ui.write_stderr_bytes(b"{\"stream\":\"stderr\"}\n").unwrap();
    ui.flush().unwrap();

    assert_eq!(stdout_copy.bytes(), b"{\"stream\":\"stdout\"}\n");
    assert_eq!(stderr_copy.bytes(), b"{\"stream\":\"stderr\"}\n");
}

#[test]
fn ui_measurement_matches_final_cross_width_color_and_stream_bytes() {
    let mut rendered_sizes = Vec::new();
    for (width, color) in [
        (32, ColorMode::Never),
        (80, ColorMode::Never),
        (80, ColorMode::Always),
    ] {
        let measurement = crate::output::OutputMeasurement::start();
        let stdout = SharedWriter::default();
        let stdout_copy = stdout.clone();
        let stderr = SharedWriter::default();
        let stderr_copy = stderr.clone();
        let stdout_context =
            RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color));
        let stderr_context =
            RenderContext::for_test(TestContext::tty(StreamKind::Stderr, width).color(color));
        let stdout_document = outcome(
            &stdout_context,
            Outcome {
                state: OutcomeState::Success,
                title: "History is ready",
                detail: Some(
                    "The final terminal-width decision controls these exact delivered bytes.",
                ),
            },
        );
        let stderr_document = diagnostic(
            &stderr_context,
            Diagnostic {
                level: DiagnosticLevel::Warning,
                summary: "One source needs attention",
                detail: Some("Run the recovery command after reviewing the source."),
                fields: &[],
                action: Some(Action {
                    command: "ctx sources --all",
                }),
            },
        );
        let mut ui = Ui::with_writers(stdout, stdout_context, stderr, stderr_context);

        ui.write_stdout(&stdout_document).unwrap();
        ui.write_stderr(&stderr_document).unwrap();
        ui.flush().unwrap();

        let stdout_bytes = stdout_copy.bytes();
        let stderr_bytes = stderr_copy.bytes();
        assert_eq!(
            measurement.stream_bytes(StreamKind::Stdout),
            u64::try_from(stdout_bytes.len()).unwrap()
        );
        assert_eq!(
            measurement.stream_bytes(StreamKind::Stderr),
            u64::try_from(stderr_bytes.len()).unwrap()
        );
        assert_eq!(
            measurement.total_bytes(),
            u64::try_from(stdout_bytes.len() + stderr_bytes.len()).unwrap()
        );
        assert_eq!(stdout_bytes.contains(&0x1b), color == ColorMode::Always);
        assert_eq!(stderr_bytes.contains(&0x1b), color == ColorMode::Always);
        rendered_sizes.push(measurement.total_bytes());
    }

    assert_ne!(
        rendered_sizes[0], rendered_sizes[1],
        "wrapping must affect the measured delivered byte count"
    );
    assert!(
        rendered_sizes[2] > rendered_sizes[1],
        "selected ANSI bytes must be included"
    );
}
