use std::{
    fs, io,
    sync::{Arc, Barrier, Mutex},
};

use super::{cta_marker, render, show_cta_once_for_channel};
use crate::pro::lifecycle::lifecycle_manifest::ReleaseChannel;
use crate::ui::{ColorMode, RenderContext, StreamKind, TestContext, Ui};

fn expected_cta() -> Vec<u8> {
    let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    render::cta(&context).render_plain().into_bytes()
}

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
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

#[test]
fn cta_marker_is_private_atomic_and_shown_once() {
    let root = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();
    let mut first = Vec::new();
    let mut second = Vec::new();
    assert!(show_cta_once_for_channel(
        root.path(),
        true,
        ReleaseChannel::Stable,
        &mut first
    ));
    assert!(!show_cta_once_for_channel(
        root.path(),
        true,
        ReleaseChannel::Stable,
        &mut second
    ));
    assert_eq!(first, expected_cta());
    assert!(second.is_empty());
    let marker = cta_marker(root.path(), ReleaseChannel::Stable);
    assert_eq!(fs::read(&marker).unwrap(), b"shown\n");
    ctx_history_core::platform_security::verify_private_file(&marker).unwrap();
    assert!(fs::read_dir(root.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

struct FailingWriter;

impl io::Write for FailingWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "simulated output failure",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn cta_output_failure_rolls_back_the_marker_for_a_later_retry() {
    let root = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();

    assert!(!show_cta_once_for_channel(
        root.path(),
        true,
        ReleaseChannel::Stable,
        &mut FailingWriter
    ));
    assert!(!cta_marker(root.path(), ReleaseChannel::Stable).exists());

    let mut retry = Vec::new();
    assert!(show_cta_once_for_channel(
        root.path(),
        true,
        ReleaseChannel::Stable,
        &mut retry
    ));
    assert_eq!(retry, expected_cta());
}

#[test]
fn concurrent_cta_attempts_publish_exactly_once() {
    const WORKERS: usize = 16;
    let root = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();
    let root = Arc::new(root);
    let barrier = Arc::new(Barrier::new(WORKERS));
    let mut workers = Vec::new();
    for _ in 0..WORKERS {
        let root = Arc::clone(&root);
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            let mut output = Vec::new();
            barrier.wait();
            let shown =
                show_cta_once_for_channel(root.path(), true, ReleaseChannel::Stable, &mut output);
            (shown, output)
        }));
    }

    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|(shown, _)| *shown).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|(_, output)| !output.is_empty())
            .count(),
        1
    );
    let expected = expected_cta();
    assert!(results
        .iter()
        .all(|(shown, output)| *shown == (output.as_slice() == expected)));
}

#[test]
fn ineligible_cta_does_not_write_or_create_state() {
    let root = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();
    let mut output = Vec::new();
    assert!(!show_cta_once_for_channel(
        root.path(),
        false,
        ReleaseChannel::Stable,
        &mut output
    ));
    assert!(output.is_empty());
    assert!(!cta_marker(root.path(), ReleaseChannel::Stable).exists());
}

#[test]
fn cta_marker_is_namespaced_by_commercial_channel() {
    let root = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();
    let mut stable = Vec::new();
    let mut staging = Vec::new();
    assert!(show_cta_once_for_channel(
        root.path(),
        true,
        ReleaseChannel::Stable,
        &mut stable
    ));
    assert!(show_cta_once_for_channel(
        root.path(),
        true,
        ReleaseChannel::Staging,
        &mut staging
    ));
    assert_eq!(stable, staging);
    assert!(cta_marker(root.path(), ReleaseChannel::Stable).is_file());
    assert!(cta_marker(root.path(), ReleaseChannel::Staging).is_file());
    assert_ne!(
        cta_marker(root.path(), ReleaseChannel::Stable),
        cta_marker(root.path(), ReleaseChannel::Staging)
    );
}

#[test]
fn cta_uses_ui_stderr_capabilities_without_touching_stdout() {
    let root = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();
    let stdout = SharedWriter::default();
    let stdout_copy = stdout.clone();
    let stderr = SharedWriter::default();
    let stderr_copy = stderr.clone();
    let stdout_context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 48));
    let stderr_context =
        RenderContext::for_test(TestContext::tty(StreamKind::Stderr, 48).color(ColorMode::Always));
    let mut ui = Ui::with_writers(stdout, stdout_context, stderr, stderr_context);

    assert!(show_cta_once_for_channel(
        root.path(),
        true,
        ReleaseChannel::Stable,
        &mut ui,
    ));
    assert!(stdout_copy.bytes().is_empty());
    let rendered = String::from_utf8(stderr_copy.bytes()).unwrap();
    assert!(rendered.contains("\u{1b}["));
    assert!(rendered.contains("Refer a developer."));
    assert!(rendered.contains("$10/month"));
    assert!(rendered.contains("agent bill."));
    assert!(rendered.contains("Up to $120 per friend."));
    assert!(rendered.contains("ctx referral create <codename>"));
    assert!(!rendered.contains("Ordinary customer referrals"));

    let plain_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    let plain = render::cta(&plain_context).render_plain();
    assert!(plain.contains("Refer a developer. Earn $10/month toward your agent bill."));
    assert!(plain.contains("Up to $120 per friend."));
}
