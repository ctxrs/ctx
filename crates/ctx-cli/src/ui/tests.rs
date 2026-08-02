use std::{
    ffi::OsString,
    io::{self, Write},
    sync::{Arc, Mutex},
};

use clap::Parser as _;
use unicode_width::UnicodeWidthStr as _;

use super::{
    bootstrap::{scan_color_mode, scan_machine_output_hint},
    canonical_human_output_bytes, diagnostic, empty_state, evidence_list, fields, hint, outcome,
    progress, section, table, Action, ColorMode, Diagnostic, DiagnosticLevel, Document, EmptyState,
    Evidence, Field, Hint, Line, Outcome, OutcomeState, Progress, RenderContext, Span, StreamKind,
    Table, TestContext, Token, Ui,
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
fn clap_global_color_option_parses_before_or_after_subcommands() {
    let before = crate::Cli::try_parse_from(["ctx", "--color", "always", "status"]).unwrap();
    assert_eq!(before.color, ColorMode::Always);

    let after = crate::Cli::try_parse_from(["ctx", "status", "--color=never"]).unwrap();
    assert_eq!(after.color, ColorMode::Never);

    let default = crate::Cli::try_parse_from(["ctx", "status"]).unwrap();
    assert_eq!(default.color, ColorMode::Auto);
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
    assert!(indeterminate.contains("..."));
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
    let documents = vec![
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
