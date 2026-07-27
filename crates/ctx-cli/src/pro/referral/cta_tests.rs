use std::{
    fs, io,
    sync::{Arc, Barrier},
};

use super::{show_cta_once, show_cta_once_when_available, REFERRAL_CTA, REFERRAL_CTA_MARKER};

#[test]
fn cta_marker_is_private_atomic_and_shown_once() {
    let root = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();
    let mut first = Vec::new();
    let mut second = Vec::new();
    assert!(show_cta_once_when_available(
        root.path(),
        true,
        true,
        &mut first
    ));
    assert!(!show_cta_once_when_available(
        root.path(),
        true,
        true,
        &mut second
    ));
    assert_eq!(
        String::from_utf8(first).unwrap(),
        format!("\n{REFERRAL_CTA}\n")
    );
    assert!(second.is_empty());
    let marker = root.path().join(REFERRAL_CTA_MARKER);
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

    assert!(!show_cta_once_when_available(
        root.path(),
        true,
        true,
        &mut FailingWriter
    ));
    assert!(!root.path().join(REFERRAL_CTA_MARKER).exists());

    let mut retry = Vec::new();
    assert!(show_cta_once_when_available(
        root.path(),
        true,
        true,
        &mut retry
    ));
    assert_eq!(
        String::from_utf8(retry).unwrap(),
        format!("\n{REFERRAL_CTA}\n")
    );
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
            let shown = show_cta_once_when_available(root.path(), true, true, &mut output);
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
    assert!(results.iter().all(|(shown, output)| {
        *shown == (output.as_slice() == format!("\n{REFERRAL_CTA}\n").as_bytes())
    }));
}

#[test]
fn ineligible_cta_does_not_write_or_create_state() {
    let root = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();
    let mut output = Vec::new();
    assert!(!show_cta_once_when_available(
        root.path(),
        false,
        true,
        &mut output
    ));
    assert!(output.is_empty());
    assert!(!root.path().join(REFERRAL_CTA_MARKER).exists());
}

#[test]
fn disabled_cta_does_not_write_or_consume_the_marker() {
    let root = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::restrict_private_directory(root.path()).unwrap();
    let mut output = Vec::new();
    assert!(!show_cta_once(root.path(), true, &mut output));
    assert!(output.is_empty());
    assert!(!root.path().join(REFERRAL_CTA_MARKER).exists());
}
