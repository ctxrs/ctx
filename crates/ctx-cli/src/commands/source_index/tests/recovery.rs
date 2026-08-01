use std::{
    fs, io,
    sync::{Arc, Mutex},
};

use anyhow::anyhow;

use crate::ui::{ColorMode, RenderContext, StreamKind, TestContext, Ui};

use super::super::{
    search::{render_search_error, MISSING_INDEX_ERROR},
    shared::{resolve_core_event, resolve_lookup_for_output, resolve_session},
};
use super::*;

const MISSING_EVENT_ID: &str = "019fa000-0000-7000-8000-0000000000e1";
const MISSING_SESSION_ID: &str = "019fa000-0000-7000-8000-0000000000f1";

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn test_ui(width: usize) -> (Ui, SharedWriter) {
    let stderr = SharedWriter::default();
    let captured = stderr.clone();
    let stdout_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    let stderr_context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stderr, width).color(ColorMode::Never),
    );
    (
        Ui::with_writers(io::sink(), stdout_context, stderr, stderr_context),
        captured,
    )
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn missing_lookup_machine_errors_keep_exact_bytes_and_no_human_output() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let index = open_index(temp.path()).unwrap();

    for (error, expected) in [
        (
            resolve_core_event(&index, MISSING_EVENT_ID).unwrap_err(),
            format!("event {MISSING_EVENT_ID} was not found in the Core generation"),
        ),
        (
            resolve_session(&index, MISSING_SESSION_ID).unwrap_err(),
            format!(
                "session {MISSING_SESSION_ID} was not found in the Core generation"
            ),
        ),
    ] {
        assert_eq!(error.to_string(), expected);
        assert_eq!(format!("{error:?}"), expected);
        let (mut ui, captured) = test_ui(32);
        let machine_error =
            resolve_lookup_for_output::<()>(Err(error), false, "unused", &mut ui).unwrap_err();
        assert_eq!(machine_error.to_string(), expected);
        assert_eq!(format!("{machine_error:?}"), expected);
        assert_eq!(captured.text(), "");
    }
}

#[test]
fn show_missing_session_is_titled_actionable_and_preserves_the_requested_id() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let index = open_index(temp.path()).unwrap();

    for width in [32, 48, 80, 100] {
        let (mut ui, captured) = test_ui(width);
        let error = resolve_lookup_for_output(
            resolve_session(&index, MISSING_SESSION_ID),
            true,
            r#"ctx search "<query>" --verbose"#,
            &mut ui,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "CLI error was already rendered");
        let rendered = captured.text();
        assert!(rendered.starts_with("✗ Session not found\n"), "{rendered}");
        assert!(
            normalized(&rendered).contains("current searchable generation"),
            "{rendered}"
        );
        assert_eq!(
            rendered.matches(MISSING_SESSION_ID).count(),
            1,
            "{rendered}"
        );
        assert!(
            rendered.contains("Next\n  ctx search \"<query>\" --verbose\n"),
            "{rendered}"
        );
        assert!(!rendered.contains("source-backed Core"), "{rendered}");
        assert!(!rendered.starts_with("Error:"), "{rendered}");
        assert!(!rendered.contains('\u{1b}'), "{rendered}");
    }
}

#[test]
fn show_missing_event_is_titled_actionable_and_preserves_the_requested_id() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let index = open_index(temp.path()).unwrap();

    for width in [32, 48, 80, 100] {
        let (mut ui, captured) = test_ui(width);
        let error = resolve_lookup_for_output(
            resolve_core_event(&index, MISSING_EVENT_ID),
            true,
            r#"ctx search "<query>" --verbose"#,
            &mut ui,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "CLI error was already rendered");
        let rendered = captured.text();
        assert!(rendered.starts_with("✗ Event not found\n"), "{rendered}");
        assert!(
            normalized(&rendered).contains("current searchable generation"),
            "{rendered}"
        );
        assert_eq!(rendered.matches(MISSING_EVENT_ID).count(), 1, "{rendered}");
        assert!(
            rendered.contains("Next\n  ctx search \"<query>\" --verbose\n"),
            "{rendered}"
        );
        assert!(!rendered.contains("source-backed Core"), "{rendered}");
        assert!(!rendered.starts_with("Error:"), "{rendered}");
        assert!(!rendered.contains('\u{1b}'), "{rendered}");
    }
}

#[test]
fn search_not_ready_offers_setup_and_import_without_changing_machine_error() {
    let temp = tempdir().unwrap();
    let (mut ui, captured) = test_ui(32);
    let machine_error = render_search_error::<()>(
        Err(anyhow!(MISSING_INDEX_ERROR)),
        false,
        temp.path(),
        &mut ui,
    )
    .unwrap_err();
    assert_eq!(machine_error.to_string(), MISSING_INDEX_ERROR);
    assert_eq!(format!("{machine_error:?}"), MISSING_INDEX_ERROR);
    assert_eq!(captured.text(), "");

    let stale_root = index_root(temp.path());
    let stale_candidate = stale_root.join("index-generations/stale");
    fs::create_dir_all(&stale_candidate).unwrap();
    fs::write(stale_candidate.join("meta.json"), "{}").unwrap();

    for width in [32, 48, 80, 100] {
        let (mut ui, captured) = test_ui(width);
        let error = render_search_error::<()>(
            Err(anyhow!(MISSING_INDEX_ERROR)),
            true,
            temp.path(),
            &mut ui,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "CLI error was already rendered");
        let rendered = captured.text();
        assert!(
            rendered.starts_with("✗ History search is not ready\n"),
            "{rendered}"
        );
        assert!(
            normalized(&rendered).contains("no current searchable generation"),
            "{rendered}"
        );
        assert!(rendered.contains("Next\n  ctx setup\n"), "{rendered}");
        assert!(
            rendered.contains("Already set up?\n  ctx import --all\n"),
            "{rendered}"
        );
        assert!(!rendered.contains("daemon"), "{rendered}");
        assert!(!rendered.contains("source-backed index"), "{rendered}");
        assert!(!rendered.contains('\u{1b}'), "{rendered}");
    }

    let (mut ui, captured) = test_ui(80);
    let unrelated = render_search_error::<()>(
        Err(anyhow!("unrelated search failure")),
        true,
        temp.path(),
        &mut ui,
    )
    .unwrap_err();
    assert_eq!(unrelated.to_string(), "unrelated search failure");
    assert_eq!(captured.text(), "");
}
