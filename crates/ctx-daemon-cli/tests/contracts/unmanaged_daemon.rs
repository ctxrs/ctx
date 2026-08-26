mod support;

#[cfg(all(
    unix,
    any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            target_env = "gnu"
        ),
        all(target_os = "macos", any(target_arch = "x86_64", target_arch = "aarch64")),
        all(target_os = "freebsd", target_arch = "x86_64")
    )
))]
mod unix {
    use std::{
        fs,
        io::Read,
        os::unix::fs::PermissionsExt as _,
        path::Path,
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    use serde_json::Value;

    use super::support::*;

    struct DaemonGuard {
        child: Option<Child>,
    }

    impl Drop for DaemonGuard {
        fn drop(&mut self) {
            if let Err(error) = terminate_and_reap_test_child(&mut self.child, "unmanaged daemon")
            {
                if std::thread::panicking() {
                    eprintln!("unmanaged daemon teardown also failed: {error}");
                } else {
                    panic!("unmanaged daemon teardown failed: {error}");
                }
            }
        }
    }

    /// Restores write permission so TempDir cleanup can remove the fixture
    /// even when the test itself fails.
    struct WritableBinDirRestore<'a>(&'a Path);

    impl Drop for WritableBinDirRestore<'_> {
        fn drop(&mut self) {
            let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(0o755));
        }
    }

    #[test]
    fn unmanaged_read_only_installation_daemon_runs_without_coordination() {
        let temp = tempdir();
        let root = data_root(&temp);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("config.toml"),
            "[analytics]\nenabled = false\n\n[upgrade]\nauto = \"off\"\n\n[daemon]\nenabled = true\nmode = \"full\"\n\n[search]\nsemantic = false\n",
        )
        .unwrap();
        fs::create_dir_all(temp.path().join(".codex").join("sessions")).unwrap();

        // A third-party packaged install: a copied executable with no
        // hosted-installer marker inside a read-only directory.
        let copied = copied_ctx_binary(&temp);
        let bin_dir = temp.path().join("readonly-bin");
        fs::create_dir(&bin_dir).unwrap();
        let binary = bin_dir.join("ctx");
        fs::rename(&copied, &binary).unwrap();
        assert!(
            !hosted_install_marker_path(&binary).exists(),
            "test setup must not create an install marker"
        );
        fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o555)).unwrap();
        let _restore = WritableBinDirRestore(&bin_dir);

        let prepared = ctx_from_binary(&temp, &binary);
        let mut command = Command::new(prepared.get_program());
        for (name, value) in prepared.get_envs() {
            match value {
                Some(value) => {
                    command.env(name, value);
                }
                None => {
                    command.env_remove(name);
                }
            }
        }
        command
            .args(["daemon", "run", "--loop-interval-seconds", "600"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut guard = DaemonGuard {
            child: Some(command.spawn().expect("start unmanaged read-only daemon")),
        };
        let child = guard.child.as_mut().expect("running daemon child");

        let deadline = Instant::now() + Duration::from_secs(20);
        let lifecycle = loop {
            if let Some(exit) = child.try_wait().expect("observe daemon child") {
                let mut stderr = String::new();
                child
                    .stderr
                    .as_mut()
                    .expect("piped daemon stderr")
                    .read_to_string(&mut stderr)
                    .expect("read daemon stderr");
                panic!("unmanaged read-only daemon exited before readiness ({exit}): {stderr}");
            }
            let lifecycle = fs::read(root.join("daemon/status.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
            if lifecycle
                .as_ref()
                .is_some_and(|lifecycle| lifecycle["status"] == "running")
            {
                break lifecycle;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for unmanaged read-only daemon readiness; lifecycle={lifecycle:#?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        };

        let lifecycle = lifecycle.expect("observed running lifecycle");
        assert_eq!(
            lifecycle["last_error"], Value::Null,
            "running unmanaged daemon must not report an error: {lifecycle:#}"
        );
        assert_eq!(lifecycle["pid"], Value::from(child.id()));
    }
}

