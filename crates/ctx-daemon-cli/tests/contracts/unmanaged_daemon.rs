mod support;

#[cfg(all(
    unix,
    any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            target_env = "gnu"
        ),
        all(
            target_os = "macos",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "freebsd", target_arch = "x86_64")
    )
))]
mod unix {
    use std::{
        fs,
        io::Read,
        os::unix::fs::PermissionsExt as _,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    use serde_json::Value;

    use super::support::*;

    struct DaemonGuard {
        root: PathBuf,
        child: Option<Child>,
    }

    impl Drop for DaemonGuard {
        fn drop(&mut self) {
            let description = format!("unmanaged daemon for {}", self.root.display());
            if let Err(error) = terminate_and_reap_test_child(&mut self.child, &description) {
                if std::thread::panicking() {
                    eprintln!("unmanaged daemon teardown also failed: {error}");
                } else {
                    panic!("unmanaged daemon teardown failed: {error}");
                }
            }
        }
    }

    fn write_daemon_config(root: &Path) {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("config.toml"),
            "[analytics]\nenabled = false\n\n[upgrade]\nauto = \"off\"\n\n[daemon]\nenabled = true\nmode = \"full\"\n\n[search]\nsemantic = false\n",
        )
        .unwrap();
    }

    fn start_daemon(temp: &tempfile::TempDir, binary: &Path, root: &Path) -> DaemonGuard {
        let prepared = ctx_from_binary(temp, binary);
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
            .arg("--data-root")
            .arg(root)
            .args(["daemon", "run", "--loop-interval-seconds", "600"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        DaemonGuard {
            root: root.to_path_buf(),
            child: Some(command.spawn().expect("start unmanaged read-only daemon")),
        }
    }

    fn wait_for_running(guard: &mut DaemonGuard) {
        let child = guard.child.as_mut().expect("running daemon child");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(exit) = child.try_wait().expect("observe daemon child") {
                let mut stderr = String::new();
                child
                    .stderr
                    .as_mut()
                    .expect("piped daemon stderr")
                    .read_to_string(&mut stderr)
                    .expect("read daemon stderr");
                panic!(
                    "unmanaged read-only daemon for {} exited before readiness ({exit}): {stderr}",
                    guard.root.display()
                );
            }
            let lifecycle = fs::read(guard.root.join("daemon/status.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
            if lifecycle
                .as_ref()
                .is_some_and(|lifecycle| lifecycle["status"] == "running")
            {
                let lifecycle = lifecycle.expect("observed running lifecycle");
                assert_eq!(
                    lifecycle["last_error"],
                    Value::Null,
                    "running unmanaged daemon must not report an error: {lifecycle:#}"
                );
                assert_eq!(lifecycle["pid"], Value::from(child.id()));
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for unmanaged read-only daemon readiness for {}; lifecycle={lifecycle:#?}",
                guard.root.display()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_for_stopped(guard: &mut DaemonGuard) {
        let mut child = guard.child.take().expect("running daemon child");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(exit) = child.try_wait().expect("observe stopped daemon child") {
                let mut stderr = String::new();
                child
                    .stderr
                    .as_mut()
                    .expect("piped daemon stderr")
                    .read_to_string(&mut stderr)
                    .expect("read daemon stderr");
                assert!(
                    exit.success(),
                    "unmanaged daemon for {} failed during prepare-uninstall ({exit}): {stderr}",
                    guard.root.display()
                );
                return;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "unmanaged daemon for {} did not stop during prepare-uninstall",
                    guard.root.display()
                );
            }
            std::thread::sleep(Duration::from_millis(25));
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
    fn unmanaged_read_only_installation_quiesces_every_registered_root() {
        let temp = tempdir();
        let first_root = data_root(&temp).join("first");
        let second_root = data_root(&temp).join("second");
        write_daemon_config(&first_root);
        write_daemon_config(&second_root);
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

        let mut first = start_daemon(&temp, &binary, &first_root);
        let mut second = start_daemon(&temp, &binary, &second_root);
        wait_for_running(&mut first);
        wait_for_running(&mut second);

        let coordination_root = temp.path().join(".ctx").join("daemon-installations");
        let namespaces = fs::read_dir(&coordination_root)
            .expect("read user-state installation coordination")
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(
            namespaces.len(),
            1,
            "one executable must use one coordination namespace"
        );
        let namespace = &namespaces[0];
        assert_eq!(
            fs::metadata(namespace).unwrap().permissions().mode() & 0o777,
            0o700,
            "installation coordination namespace must be owner-private"
        );
        let registrations = namespace.join("daemon-quiescence-acks");
        let registration_roots = fs::read_dir(&registrations)
            .expect("read installation daemon registrations")
            .map(|entry| {
                let value: Value =
                    serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
                value["data_root"].as_str().unwrap().to_owned()
            })
            .collect::<Vec<_>>();
        for root in [&first_root, &second_root] {
            assert!(
                registration_roots
                    .iter()
                    .any(|registered| registered == root.to_string_lossy().as_ref()),
                "installation registration omitted {}: {registration_roots:#?}",
                root.display()
            );
        }

        let mut prepare = ctx_from_binary(&temp, &binary);
        let output = prepare
            .arg("--data-root")
            .arg(&first_root)
            .args(["daemon", "disable", "--prepare-uninstall", "--format=json"])
            .output()
            .expect("prepare unmanaged installation uninstall");
        assert!(
            output.status.success(),
            "prepare-uninstall failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let receipt: Value =
            serde_json::from_slice(&output.stdout).expect("parse uninstall receipt");
        assert_eq!(receipt["ok"], true, "{receipt:#}");
        assert_eq!(receipt["scope"], "installation", "{receipt:#}");
        assert_eq!(receipt["installation_quiescent"], true, "{receipt:#}");
        assert_eq!(receipt["daemon_running"], false, "{receipt:#}");
        let quiesced_roots = receipt["quiesced_roots"]
            .as_array()
            .expect("quiesced roots array");
        for root in [&first_root, &second_root] {
            assert!(
                quiesced_roots
                    .iter()
                    .any(|candidate| candidate.as_str() == Some(root.to_string_lossy().as_ref())),
                "successful installation receipt omitted {}: {receipt:#}",
                root.display()
            );
        }

        wait_for_stopped(&mut first);
        wait_for_stopped(&mut second);

        let bin_entries = fs::read_dir(&bin_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(
            bin_entries,
            vec![binary.clone()],
            "daemon coordination created files beside the executable"
        );
        assert!(
            !registrations.exists(),
            "prepare-uninstall retained installation daemon registrations"
        );
    }
}
