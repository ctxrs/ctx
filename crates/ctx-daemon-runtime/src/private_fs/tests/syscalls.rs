use super::*;

#[test]
fn private_directory_syscall_child() -> Result<()> {
    let Ok(case) = std::env::var("CTX_DIRECTORY_SYSCALL_CASE") else {
        return Ok(());
    };
    let temp = std::path::PathBuf::from(std::env::var_os("CTX_DIRECTORY_SYSCALL_ROOT").unwrap());
    if case == "paused_creator" {
        return create_private_dir_all_before_ack(&temp.join("a"));
    }
    let target = temp.join("a/b/data");
    if case == "generic_create" || case == "generic_ensure" {
        if case == "generic_create" {
            ctx_history_platform::platform_security::create_private_directory_all(&target)?;
        } else {
            ctx_history_platform::platform_security::ensure_private_directory(&target)?;
        }
        ctx_history_platform::platform_security::verify_private_directory(&target)?;
        return Ok(());
    }
    if matches!(case.as_str(), "sync_error" | "inode_error" | "mkdir_error") {
        assert!(create_private_dir_all_before_ack(&target).is_err());
        assert!(temp.join("a").is_dir());
        assert!(!temp.join("a/b").exists(), "failure must stop descent");
    } else if case == "existing_error" {
        assert!(create_private_dir_all_before_ack(&target).is_err());
        assert!(target.is_dir());
    }
    create_private_dir_all_before_ack(&target)?;
    ctx_history_platform::platform_security::verify_private_directory(&target)?;
    // The ordinary existing-directory path adds no durability work.
    create_private_dir_all(&target)?;
    Ok(())
}

#[test]
fn directory_syscalls_confirm_cold_links_before_descent_and_retry_errors() -> Result<()> {
    for (case, fault) in [
        ("cold", None),
        ("sync_error", Some("fsync:error=EIO:when=4")),
        ("inode_error", Some("fsync:error=EIO:when=3")),
        ("mkdir_error", Some("mkdirat:error=EIO:when=2")),
        ("existing_error", Some("fsync:error=EIO:when=2")),
        ("generic_create", None),
        ("generic_ensure", None),
    ] {
        let temp = isolated_root()?;
        if case == "existing_error" {
            create_private_dir_all_before_ack(&temp.path().join("a/b/data"))?;
        }
        let trace_path = temp.path().join("trace.log");
        let mut command = std::process::Command::new("/usr/bin/strace");
        command
            .args(["-f", "-yy", "-e", "trace=fsync,mkdirat", "-o"])
            .arg(&trace_path);
        if let Some(fault) = fault {
            command.arg(format!("--inject={fault}"));
        }
        let output = command
            .arg(std::env::current_exe()?)
            .args([
                "--exact",
                "private_fs::tests::syscalls::private_directory_syscall_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("CTX_DIRECTORY_SYSCALL_CASE", case)
            .env("CTX_DIRECTORY_SYSCALL_ROOT", temp.path())
            .output()?;
        let trace = fs::read_to_string(&trace_path)?;
        assert!(
            output.status.success(),
            "{case}: {} {}\n{trace}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let calls = trace
            .lines()
            .filter(|line| line.contains("fsync(") || line.contains("mkdirat("))
            .collect::<Vec<_>>();
        if case == "generic_create" || case == "generic_ensure" {
            assert_eq!(calls.len(), 3, "{trace}");
            assert!(
                calls.iter().all(|line| line.contains("mkdirat(")),
                "{trace}"
            );
        } else if case == "existing_error" {
            assert_eq!(calls.len(), 4, "{trace}");
            assert!(calls[0].contains("/a/b/data>"), "{trace}");
            assert!(
                calls[1].contains("/a/b>") && calls[1].contains("EIO"),
                "{trace}"
            );
            assert!(calls[2].contains("/a/b/data>"), "{trace}");
            assert!(
                calls[3].contains("/a/b>") && calls[3].ends_with("= 0"),
                "{trace}"
            );
        } else {
            let a = calls
                .iter()
                .position(|line| line.contains("mkdirat(") && line.contains("\"a\""))
                .unwrap();
            let b = calls
                .iter()
                .rposition(|line| line.contains("mkdirat(") && line.contains("\"b\""))
                .unwrap();
            let data = calls
                .iter()
                .position(|line| line.contains("mkdirat(") && line.contains("\"data\""))
                .unwrap();
            assert!(
                calls[a + 1..b]
                    .iter()
                    .any(|line| line.contains("/a>") && line.ends_with("= 0")),
                "{trace}"
            );
            assert!(
                calls[a + 1..b].iter().any(|line| line
                    .contains(&format!("<{}>", temp.path().display()))
                    && line.ends_with("= 0")),
                "{trace}"
            );
            assert!(
                calls[b + 1..data]
                    .iter()
                    .any(|line| line.contains("/a/b>") && line.ends_with("= 0")),
                "{trace}"
            );
            assert!(
                calls[b + 1..data]
                    .iter()
                    .any(|line| line.contains("/a>") && line.ends_with("= 0")),
                "{trace}"
            );
        }
        assert!(!calls.iter().any(|line| line.contains("</>")), "{trace}");
        eprintln!(
            "directory_durability case={case} calls={} trace={calls:?}",
            calls.len()
        );
    }
    Ok(())
}
