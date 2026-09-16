use std::{io::Write as _, os::unix::fs::PermissionsExt as _, sync::Mutex};

use super::*;
use crate::{
    ensure_native_supervisor_with, resume_native_supervisor_with, DaemonLock,
    NativeSupervisorBackend, SupervisorEnsureOutcome, SupervisorResumeOutcome,
    SupervisorUpgradeFence,
};

const SERVICE: &str = "ctx-start-limit-test.service";

struct SystemdFixture {
    owner: Mutex<Option<DaemonLock>>,
    temp: tempfile::TempDir,
    spec: SupervisorSpec,
    environment: SupervisorManagerEnvironment,
}

impl SystemdFixture {
    fn new(scenario: &str, result: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let systemctl = temp.path().join("systemctl");
        fs::write(
            &systemctl,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$FIXTURE_ROOT/commands"
case "$*" in
  '--user show --property=Version --value') printf '257\n' ;;
  '--user daemon-reload') ;;
  '--user enable ctx-start-limit-test.service') : > "$FIXTURE_ROOT/enabled" ;;
  '--user is-enabled ctx-start-limit-test.service')
    [ -f "$FIXTURE_ROOT/enabled" ] || exit 1
    printf 'enabled\n' ;;
  '--user restart ctx-start-limit-test.service')
    printf 'initial restart refused\n' >&2; exit 1 ;;
  '--user start ctx-start-limit-test.service')
    if [ "$SCENARIO" = healthy ]; then
      : > "$FIXTURE_ROOT/started"
    elif [ -f "$FIXTURE_ROOT/reset" ]; then
      if [ "$SCENARIO" = retry-fails ]; then
        printf 'retry refused\n' >&2; exit 1
      fi
      : > "$FIXTURE_ROOT/started"
    else
      if [ "$SCENARIO" = misleading-error ]; then
        printf 'initial start refused: start-limit-hit (untrusted diagnostic)\n' >&2
      else
        printf 'initial start refused: démarrage refusé\n' >&2
      fi
      exit 1
    fi ;;
  '--user show ctx-start-limit-test.service --property=Result --value')
    if [ "$SCENARIO" = invalid-query ]; then
      printf '\377\n'; exit 0
    fi
    printf '%s\n' "$RESULT"
    [ "$SCENARIO" != query-fails ] || exit 1 ;;
  '--user reset-failed ctx-start-limit-test.service')
    if [ "$SCENARIO" = reset-fails ]; then
      printf 'reset refused\n' >&2; exit 1
    fi
    : > "$FIXTURE_ROOT/reset" ;;
  '--user is-active ctx-start-limit-test.service')
    [ -f "$FIXTURE_ROOT/started" ] || exit 3
    printf 'active\n' ;;
  '--user show ctx-start-limit-test.service --property=MainPID --value')
    printf '%s\n' "$MANAGER_PID" ;;
  *) printf 'unexpected systemctl command: %s\n' "$*" >&2; exit 90 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o700)).unwrap();
        let environment = SupervisorManagerEnvironment::new(BTreeMap::from([
            (OsString::from("PATH"), temp.path().as_os_str().to_owned()),
            (
                OsString::from("FIXTURE_ROOT"),
                temp.path().as_os_str().to_owned(),
            ),
            (OsString::from("SCENARIO"), OsString::from(scenario)),
            (OsString::from("RESULT"), OsString::from(result)),
            (
                OsString::from("MANAGER_PID"),
                OsString::from(if scenario == "wrong-owner" {
                    std::process::id().saturating_add(1).to_string()
                } else {
                    std::process::id().to_string()
                }),
            ),
        ]));
        let spec = SupervisorSpec::new(
            SupervisorIdentity::new(SERVICE, temp.path().join(SERVICE)).unwrap(),
            "test daemon",
            crate::supervisor_environment_path(temp.path()),
            crate::NormalizedLaunch::new(
                std::env::current_exe().unwrap(),
                vec![OsString::from("daemon"), OsString::from("run")],
                BTreeMap::from([(OsString::from("HOME"), temp.path().as_os_str().to_owned())]),
            ),
        )
        .unwrap();
        Self {
            owner: Mutex::new(None),
            temp,
            spec,
            environment,
        }
    }

    fn commands(&self) -> Vec<String> {
        fs::read_to_string(self.temp.path().join("commands"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn lifecycle_commands(&self) -> Vec<String> {
        self.commands()
            .into_iter()
            .filter(|line| {
                line == "fence-release"
                    || ["--user start ", "--user restart ", "--user reset-failed "]
                        .iter()
                        .any(|prefix| line.starts_with(prefix))
            })
            .collect()
    }

    fn register(&self) {
        crate::write_supervisor_environment(&self.spec).unwrap();
        fs::write(
            self.spec.identity().artifact_path(),
            crate::linux_systemd_unit(&self.spec).unwrap(),
        )
        .unwrap();
        fs::write(self.temp.path().join("enabled"), "").unwrap();
    }
}

#[test]
fn systemd_start_only_resets_an_exact_successfully_queried_start_limit() {
    for result in [
        "exit-code",
        "success",
        "",
        "start-limit",
        "start-limit-hit\nsuccess",
    ] {
        let fixture = SystemdFixture::new("misleading-error", result);
        let error =
            start_systemd_supervisor(fixture.spec.identity(), &fixture.environment).unwrap_err();
        assert!(error.to_string().contains("initial start refused"));
        assert_eq!(
            fixture.commands(),
            [
                format!("--user start {SERVICE}"),
                format!("--user show {SERVICE} --property=Result --value"),
            ]
        );
    }
    for (scenario, expected_commands, expected_error) in [
        ("healthy", 1, None),
        ("recover", 4, None),
        ("query-fails", 2, Some("initial start refused")),
        ("invalid-query", 2, Some("initial start refused")),
        ("reset-fails", 3, Some("reset refused")),
        ("retry-fails", 4, Some("retry refused")),
    ] {
        let fixture = SystemdFixture::new(scenario, "start-limit-hit");
        let result = start_systemd_supervisor(fixture.spec.identity(), &fixture.environment);
        if let Some(message) = expected_error {
            let error = format!("{:#}", result.unwrap_err());
            assert!(error.contains(message), "{scenario}: {error}");
        } else {
            result.unwrap();
        }
        let expected = [
            format!("--user start {SERVICE}"),
            format!("--user show {SERVICE} --property=Result --value"),
            format!("--user reset-failed {SERVICE}"),
            format!("--user start {SERVICE}"),
        ];
        assert_eq!(
            fixture.commands(),
            expected[..expected_commands],
            "{scenario}"
        );
    }
}

#[test]
fn missing_systemctl_preserves_the_original_spawn_failure() {
    let fixture = SystemdFixture::new("recover", "start-limit-hit");
    fs::remove_file(fixture.temp.path().join("systemctl")).unwrap();
    let error =
        start_systemd_supervisor(fixture.spec.identity(), &fixture.environment).unwrap_err();
    assert_eq!(error.to_string(), "run systemctl --user");
    assert!(!fixture.temp.path().join("commands").exists());
}

#[test]
fn systemd_verification_does_not_recover_a_stopped_service() {
    let fixture = SystemdFixture::new("recover", "start-limit-hit");
    fixture.register();
    verify_systemd_registration(&fixture.spec, &fixture.environment).unwrap();
    assert!(systemd_live_owner_pid(&fixture.spec, &fixture.environment).is_err());
    assert_eq!(
        fixture.commands(),
        [
            format!("--user is-enabled {SERVICE}"),
            format!("--user is-enabled {SERVICE}"),
            format!("--user is-active {SERVICE}"),
        ]
    );
    assert!(!fixture.temp.path().join("reset").exists());
}

impl NativeSupervisorBackend<()> for SystemdFixture {
    fn probe_manager(&self, _: &Path) -> Result<SupervisorManagerOperability> {
        probe_systemd_user_manager(&self.environment)
    }

    fn prepare_mutation(&self, _: &Path, _: &Path) -> Result<()> {
        assert!(self.owner.lock().unwrap().is_none());
        Ok(())
    }

    fn artifact_path(&self, _: &Path) -> Result<Option<PathBuf>> {
        Ok(Some(self.spec.identity().artifact_path().to_owned()))
    }

    fn install(&self, data_root: &Path, _: &Path, _: &()) -> Result<PathBuf> {
        install_systemd_supervisor(data_root, &self.spec, &self.environment, &|_| Ok(()))
    }

    fn disable(&self, _: &Path) -> Result<Option<PathBuf>> {
        panic!("a surviving registration must not be disabled")
    }

    fn verify_registration(&self, _: &Path, _: &Path) -> Result<()> {
        verify_systemd_registration(&self.spec, &self.environment)
    }

    fn verify_live_owner(&self, data_root: &Path, executable: &Path) -> Result<u32> {
        let pid = systemd_live_owner_pid(&self.spec, &self.environment)?;
        verify_daemon_owner_identity(data_root, executable, Some(pid))
    }

    fn prepare_start(&self, _: &Path, _: &Path) -> Result<Option<u32>> {
        assert!(self.owner.lock().unwrap().is_none());
        Ok(None)
    }

    fn start(&self, data_root: &Path) -> Result<()> {
        start_systemd_supervisor(self.spec.identity(), &self.environment)?;
        *self.owner.lock().unwrap() = DaemonLock::acquire(data_root)?;
        Ok(())
    }
}

#[test]
fn ensure_bounds_recovery_across_registration_restart_and_start() {
    for scenario in ["recover", "retry-fails", "wrong-owner"] {
        let fixture = SystemdFixture::new(scenario, "start-limit-hit");
        let outcome = ensure_native_supervisor_with(
            fixture.temp.path(),
            fixture.spec.launch().program(),
            &(),
            &fixture,
        )
        .unwrap();
        match outcome {
            SupervisorEnsureOutcome::Native { owner_pid, .. } => {
                assert_eq!(scenario, "recover");
                assert_eq!(owner_pid, std::process::id());
            }
            SupervisorEnsureOutcome::RegisteredNotRunning { recovery_error, .. } => {
                assert_ne!(
                    scenario, "recover",
                    "recovery must establish a verified owner"
                );
                let error = format!("{recovery_error:#}");
                let expected = if scenario == "wrong-owner" {
                    "does not own the ctx daemon lock"
                } else {
                    "retry refused"
                };
                assert!(error.contains(expected), "{scenario}: {error}");
            }
            other => panic!("unexpected ensure outcome: {other:?}"),
        }
        assert_eq!(
            fixture.lifecycle_commands(),
            [
                format!("--user restart {SERVICE}"),
                format!("--user start {SERVICE}"),
                format!("--user reset-failed {SERVICE}"),
                format!("--user start {SERVICE}"),
            ],
            "{scenario}"
        );
    }
}

struct FixtureFence<'a>(&'a SystemdFixture);

impl SupervisorUpgradeFence for FixtureFence<'_> {
    fn release(&mut self) -> Result<()> {
        let mut log = fs::OpenOptions::new()
            .append(true)
            .open(self.0.temp.path().join("commands"))?;
        writeln!(log, "fence-release")?;
        Ok(())
    }
}

#[test]
fn resume_recovers_once_after_fence_release_and_requires_verified_ownership() {
    for scenario in ["recover", "retry-fails", "wrong-owner"] {
        let fixture = SystemdFixture::new(scenario, "start-limit-hit");
        fixture.register();
        let outcome = resume_native_supervisor_with(
            fixture.temp.path(),
            fixture.spec.launch().program(),
            &fixture,
            &mut FixtureFence(&fixture),
        )
        .unwrap();
        match outcome {
            SupervisorResumeOutcome::Native { owner_pid, .. } => {
                assert_eq!(scenario, "recover");
                assert_eq!(owner_pid, std::process::id());
            }
            SupervisorResumeOutcome::RegisteredNotRunning { error, .. } => {
                assert_ne!(
                    scenario, "recover",
                    "recovery must establish a verified owner"
                );
                let error = format!("{error:#}");
                let expected = if scenario == "wrong-owner" {
                    "does not own the ctx daemon lock"
                } else {
                    "retry refused"
                };
                assert!(error.contains(expected), "{scenario}: {error}");
            }
            other => panic!("unexpected resume outcome: {other:?}"),
        }
        assert_eq!(
            fixture.lifecycle_commands(),
            [
                "fence-release".to_owned(),
                format!("--user start {SERVICE}"),
                format!("--user reset-failed {SERVICE}"),
                format!("--user start {SERVICE}"),
            ],
            "{scenario}"
        );
    }
}
