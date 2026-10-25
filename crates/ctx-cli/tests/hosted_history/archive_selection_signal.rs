//! Linux exposes open file descriptors so the interrupt is synchronized with
//! an active, disk-backed selection, without timing assumptions or product hooks.

use super::{archive_selection::seed_selection_records, support::Sandbox};
use std::{
    fs,
    os::unix::process::ExitStatusExt,
    process::{Child, Stdio},
    time::{Duration, Instant},
};

struct ChildCleanup(Child);

impl Drop for ChildCleanup {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn sigint_during_preview_leaves_no_selection_scratch() {
    let sandbox = Sandbox::new();
    seed_selection_records(
        &sandbox,
        (0..12_000).map(|i| (format!("conversation-{i}"), Some(100), "signal".into())),
    );
    let scratch = sandbox.path("tmp");
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
    let mut child = ChildCleanup(
        sandbox
            .std_command()
            .env("SQLITE_TMPDIR", &scratch)
            .args([
                "archive",
                "export",
                "--origin",
                "synthetic-laptop",
                "--preview",
                "--format=json",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let pid = i32::try_from(child.0.id()).unwrap();
    let descriptors = format!("/proc/{pid}/fd");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "preview exited before interrupt"
        );
        let scratch_open = fs::read_dir(&descriptors)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                fs::read_link(entry.path()).is_ok_and(|path| path.starts_with(&scratch))
                    && fs::metadata(entry.path()).is_ok_and(|metadata| metadata.len() >= 16_384)
            });
        if scratch_open {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "selection never opened disk-backed scratch"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    // Deliver an actual terminal-interrupt signal while the selection owns its file.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "preview did not stop after SIGINT"
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(status.signal(), Some(libc::SIGINT));
    assert_eq!(
        fs::read_dir(&scratch).unwrap().count(),
        0,
        "selection scratch leaked"
    );
}
