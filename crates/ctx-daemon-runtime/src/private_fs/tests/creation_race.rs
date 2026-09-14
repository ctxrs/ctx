use super::*;
use std::{
    process::Command,
    time::{Duration, Instant},
};

const CHILD: &str = "private_fs::tests::syscalls::private_directory_syscall_child";

#[test]
fn visible_unsynced_directory_is_confirmed_by_second_creator_before_descent() -> Result<()> {
    let temp = isolated_root()?;
    let trace_a = temp.path().join("creator-a.trace");
    let trace_b = temp.path().join("creator-b.trace");
    // Pause at the actual new-directory inode fsync after mkdir(a) has
    // returned, without a production hook. The second owner must see a via
    // its initial open, not mkdir/EEXIST, and repair its link before mkdir(b).
    let mut first = Command::new("/usr/bin/strace")
        .args([
            "-f",
            "-yy",
            "-e",
            "trace=fsync,mkdirat",
            "--inject=fsync:delay_enter=5s:when=3",
            "-o",
        ])
        .arg(&trace_a)
        .arg(std::env::current_exe()?)
        .args(["--exact", CHILD, "--test-threads=1"])
        .env("CTX_DIRECTORY_SYSCALL_CASE", "paused_creator")
        .env("CTX_DIRECTORY_SYSCALL_ROOT", temp.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let result = (|| -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !temp.path().join("a").exists() {
            anyhow::ensure!(Instant::now() < deadline, "creator a did not reach mkdir");
            anyhow::ensure!(
                first.try_wait()?.is_none(),
                "creator a exited before visibility"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        anyhow::ensure!(first.try_wait()?.is_none(), "creator a was not paused");
        let second = Command::new("/usr/bin/strace")
            .args(["-f", "-yy", "-e", "trace=fsync,mkdirat", "-o"])
            .arg(&trace_b)
            .arg(std::env::current_exe()?)
            .args(["--exact", CHILD, "--test-threads=1"])
            .env("CTX_DIRECTORY_SYSCALL_CASE", "cold")
            .env("CTX_DIRECTORY_SYSCALL_ROOT", temp.path())
            .output()?;
        anyhow::ensure!(
            second.status.success(),
            "second creator: {}",
            String::from_utf8_lossy(&second.stderr)
        );
        anyhow::ensure!(
            first.try_wait()?.is_none(),
            "pause ended before second creator finished"
        );
        Ok(fs::read_to_string(&trace_b)?)
    })();
    let first_output = first.wait_with_output()?;
    let trace = result?;
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    let calls = trace
        .lines()
        .filter(|line| line.contains("fsync(") || line.contains("mkdirat("))
        .collect::<Vec<_>>();
    assert!(
        calls[0].contains("fsync(") && calls[0].contains("/a>") && calls[0].ends_with("= 0"),
        "{trace}"
    );
    assert!(
        calls[1].contains("fsync(")
            && calls[1].contains(&format!("<{}>", temp.path().display()))
            && calls[1].ends_with("= 0"),
        "{trace}"
    );
    assert!(
        calls[2].contains("mkdirat(") && calls[2].contains("\"b\""),
        "{trace}"
    );
    assert!(
        !calls
            .iter()
            .any(|line| line.contains("mkdirat(") && line.contains("\"a\"")),
        "{trace}"
    );
    for directory in [
        temp.path().join("a"),
        temp.path().join("a/b"),
        temp.path().join("a/b/data"),
    ] {
        ctx_history_platform::platform_security::verify_private_directory(&directory)?;
    }
    let first_trace = fs::read_to_string(trace_a)?;
    assert!(first_trace.contains("DELAYED"), "{first_trace}");
    eprintln!(
        "directory_durability forced_visible_before_sync first={first_trace:?} second={trace:?}"
    );
    Ok(())
}
