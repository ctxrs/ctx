use std::{cell::Cell, os::unix::fs::PermissionsExt as _};

use super::*;

const DOMAIN: &str = "gui/501";
const LABEL: &str = "rs.ctx.daemon";

struct LaunchdFixture {
    temp: tempfile::TempDir,
    spec: SupervisorSpec,
    environment: SupervisorManagerEnvironment,
}

impl LaunchdFixture {
    fn new(scenario: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let launchctl = temp.path().join("launchctl");
        fs::write(
            &launchctl,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$FIXTURE_ROOT/commands"
case "$1" in
  bootout)
    rm -f "$FIXTURE_ROOT/registered" "$FIXTURE_ROOT/pid"
    exit 0 ;;
  bootstrap)
    : > "$FIXTURE_ROOT/registered"
    if [ "$SCENARIO" = immediate-spawn ]; then
      printf '14581\n' > "$FIXTURE_ROOT/pid"
    elif [ "$SCENARIO" = delayed-spawn ]; then
      printf '0\n' > "$FIXTURE_ROOT/print_count"
    fi
    exit 0 ;;
  print)
    [ -f "$FIXTURE_ROOT/registered" ] || exit 113
    if [ "$SCENARIO" = delayed-spawn ] && [ -f "$FIXTURE_ROOT/print_count" ]; then
      count="$(cat "$FIXTURE_ROOT/print_count")"
      count="$((count + 1))"
      printf '%s\n' "$count" > "$FIXTURE_ROOT/print_count"
      if [ "$count" -ge 2 ]; then
        printf '14581\n' > "$FIXTURE_ROOT/pid"
      fi
    fi
    if [ -f "$FIXTURE_ROOT/pid" ]; then
      printf 'gui/501/rs.ctx.daemon = {\n\tstate = running\n\tpid = %s\n}\n' "$(cat "$FIXTURE_ROOT/pid")"
    else
      printf 'gui/501/rs.ctx.daemon = {\n\tstate = not running\n}\n'
    fi
    exit 0 ;;
  kickstart)
    for arg in "$@"; do
      if [ "$arg" = "-k" ]; then
        printf 'unexpected -k flag in kickstart\n' >&2
        exit 91
      fi
    done
    printf '16149\n' > "$FIXTURE_ROOT/pid"
    exit 0 ;;
  *)
    printf 'unexpected launchctl command: %s\n' "$*" >&2
    exit 90 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&launchctl, fs::Permissions::from_mode(0o700)).unwrap();
        let path_env = format!("{}:/usr/bin:/bin", temp.path().display());
        let environment = SupervisorManagerEnvironment::new(BTreeMap::from([
            (OsString::from("PATH"), OsString::from(path_env)),
            (
                OsString::from("FIXTURE_ROOT"),
                temp.path().as_os_str().to_owned(),
            ),
            (OsString::from("SCENARIO"), OsString::from(scenario)),
        ]));
        let plist = temp
            .path()
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"));
        let spec = SupervisorSpec::new(
            SupervisorIdentity::new(LABEL, plist).unwrap(),
            "ctx test daemon",
            crate::supervisor_environment_path(temp.path()),
            crate::NormalizedLaunch::new(
                PathBuf::from("/opt/ctx/bin/ctx"),
                vec![OsString::from("daemon"), OsString::from("run")],
                BTreeMap::from([(OsString::from("HOME"), temp.path().as_os_str().to_owned())]),
            ),
        )
        .unwrap();
        Self {
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
}

#[test]
fn launchctl_kickstart_command_never_passes_kill_flag() {
    let environment = SupervisorManagerEnvironment::new(BTreeMap::new());
    let command = launchctl_kickstart_command(DOMAIN, LABEL, &environment);
    assert_eq!(command.get_program(), "launchctl");
    let args = command.get_args().collect::<Vec<_>>();
    assert_eq!(
        args,
        ["kickstart", "gui/501/rs.ctx.daemon"]
            .iter()
            .map(OsStr::new)
            .collect::<Vec<_>>()
    );
    assert!(!args.contains(&OsStr::new("-k")));
}

#[test]
fn launchd_install_skips_kickstart_when_bootstrap_already_spawns_agent() {
    let fixture = LaunchdFixture::new("immediate-spawn");
    let migrations = Cell::new(0_u32);
    let installed = install_launch_agent_in_domain(
        fixture.temp.path(),
        &fixture.spec,
        DOMAIN,
        &fixture.environment,
        &|_| {
            migrations.set(migrations.get() + 1);
            Ok(())
        },
        Duration::from_millis(50),
        Duration::from_millis(5),
    )
    .unwrap();
    assert_eq!(installed, fixture.spec.identity().artifact_path());
    assert_eq!(migrations.get(), 1);
    let commands = fixture.commands();
    assert!(commands.iter().any(|cmd| cmd.starts_with("bootout ")));
    assert!(commands.iter().any(|cmd| cmd.starts_with("bootstrap ")));
    assert!(
        !commands.iter().any(|cmd| cmd.starts_with("kickstart")),
        "RunAtLoad bootstrap already spawned the agent; kickstart must not run: {commands:?}"
    );
}

#[test]
fn launchd_install_waits_briefly_for_delayed_bootstrap_spawn_before_kickstart() {
    let fixture = LaunchdFixture::new("delayed-spawn");
    install_launch_agent_in_domain(
        fixture.temp.path(),
        &fixture.spec,
        DOMAIN,
        &fixture.environment,
        &|_| Ok(()),
        Duration::from_millis(200),
        Duration::from_millis(5),
    )
    .unwrap();
    let commands = fixture.commands();
    assert!(commands.iter().any(|cmd| cmd.starts_with("bootstrap ")));
    assert!(
        !commands.iter().any(|cmd| cmd.starts_with("kickstart")),
        "delayed RunAtLoad spawn within grace window must skip kickstart: {commands:?}"
    );
}

#[test]
fn launchd_install_uses_non_killing_kickstart_only_when_bootstrap_does_not_spawn() {
    let fixture = LaunchdFixture::new("no-spawn");
    install_launch_agent_in_domain(
        fixture.temp.path(),
        &fixture.spec,
        DOMAIN,
        &fixture.environment,
        &|_| Ok(()),
        Duration::from_millis(10),
        Duration::from_millis(5),
    )
    .unwrap();
    let commands = fixture.commands();
    let kickstarts = commands
        .iter()
        .filter(|cmd| cmd.starts_with("kickstart"))
        .collect::<Vec<_>>();
    assert_eq!(kickstarts, vec!["kickstart gui/501/rs.ctx.daemon"]);
}

#[test]
fn launchd_start_preserves_running_agent_and_starts_stopped_agent_without_killing() {
    let fixture = LaunchdFixture::new("no-spawn");
    fs::write(fixture.temp.path().join("registered"), "").unwrap();

    // Stopped registration -> non-killing kickstart.
    start_launch_agent_in_domain(fixture.spec.identity(), DOMAIN, &fixture.environment).unwrap();
    assert_eq!(
        fixture.commands(),
        [
            "print gui/501/rs.ctx.daemon",
            "kickstart gui/501/rs.ctx.daemon",
        ]
    );

    // Already-running registration -> preserved without kickstart or -k.
    fs::write(fixture.temp.path().join("commands"), "").unwrap();
    start_launch_agent_in_domain(fixture.spec.identity(), DOMAIN, &fixture.environment).unwrap();
    assert_eq!(fixture.commands(), ["print gui/501/rs.ctx.daemon"]);
}
