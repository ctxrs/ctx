#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use super::secret_service_environment;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[test]
fn helper_forwards_only_secret_service_socket_locators() {
    let mut requested = Vec::new();
    let forwarded = secret_service_environment(|key| match key {
        "DBUS_SESSION_BUS_ADDRESS" => {
            requested.push(key.to_owned());
            Some("unix:path=/run/user/1000/bus".into())
        }
        "XDG_RUNTIME_DIR" => {
            requested.push(key.to_owned());
            Some("/run/user/1000".into())
        }
        _ => panic!("unexpected environment lookup: {key}"),
    });
    assert_eq!(requested, ["DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"]);
    assert_eq!(forwarded.len(), 2);
    assert_eq!(forwarded[0].0, "DBUS_SESSION_BUS_ADDRESS");
    assert_eq!(forwarded[1].0, "XDG_RUNTIME_DIR");
}

#[test]
fn helper_receives_exact_opaque_installation_identity_and_clears_unknown_values() {
    use std::ffi::OsStr;
    use std::process::Command;

    const ID: &str = "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8";
    let mut command = Command::new("ctx-pro-test");
    command
        .env("CTX_TEST_SECRET", "must-not-survive")
        .env("CTX_PRO_CORE_PREPARATION_WORKERS", " 999 ");
    super::configure_environment_with_preparation_workers(
        &mut command,
        std::path::Path::new("/ctx/data"),
        ID,
        Some(std::path::Path::new("/usr/bin/git")),
        16,
    )
    .unwrap();
    let environment = command.get_envs().collect::<Vec<_>>();
    assert!(environment.iter().any(|(key, value)| {
        *key == OsStr::new("CTX_PRO_INSTALLATION_ID") && *value == Some(OsStr::new(ID))
    }));
    assert!(environment
        .iter()
        .all(|(key, _)| *key != OsStr::new("CTX_TEST_SECRET")));
    assert!(environment.iter().any(|(key, value)| {
        *key == OsStr::new("CTX_PRO_CORE_PREPARATION_WORKERS") && *value == Some(OsStr::new("16"))
    }));
}

#[test]
fn helper_rejects_a_zero_preparation_worker_budget() {
    let mut command = std::process::Command::new("ctx-pro-test");
    let error = super::configure_environment_with_preparation_workers(
        &mut command,
        std::path::Path::new("/ctx/data"),
        "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8",
        None,
        0,
    )
    .unwrap_err();
    assert!(error.to_string().contains("must be positive"));
}

#[cfg(unix)]
#[test]
fn helper_process_does_not_receive_a_forbidden_environment_value() {
    use std::{fs, os::unix::fs::PermissionsExt as _, process::Command};

    let root = tempfile::tempdir().unwrap();
    let helper = root.path().join("environment-fixture");
    fs::write(
        &helper,
        r#"#!/bin/sh
set -eu
[ "${CTX_TEST_SECRET+x}" != x ] || exit 41
printf '%s' 'bounded-environment'
"#,
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = Command::new(&helper);
    command.env("CTX_TEST_SECRET", "must-not-survive");
    super::configure_environment_with_preparation_workers(
        &mut command,
        root.path(),
        "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8",
        None,
        4,
    )
    .unwrap();

    let output = command.output().unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"bounded-environment");
    assert!(output.stderr.is_empty());
}

#[cfg(windows)]
mod windows {
    use std::{
        collections::BTreeMap, ffi::OsString, os::windows::ffi::OsStrExt as _, path::Path,
        process::Command,
    };

    use super::super::{configure_environment_with_preparation_workers, windows_system_root_from};

    #[test]
    fn system_root_grows_the_api_buffer_without_reading_the_environment() {
        let expected = OsString::from(format!(r"C:\{}", "w".repeat(300)));
        let encoded: Vec<u16> = expected.encode_wide().collect();
        let mut capacities = Vec::new();
        let actual = windows_system_root_from(|buffer| {
            capacities.push(buffer.len());
            if buffer.len() <= encoded.len() {
                return (encoded.len() + 1) as u32;
            }
            buffer[..encoded.len()].copy_from_slice(&encoded);
            encoded.len() as u32
        })
        .unwrap();

        assert_eq!(actual, expected);
        assert_eq!(capacities, [260, encoded.len() + 2]);
    }

    #[test]
    fn system_root_rejects_non_absolute_api_results() {
        let encoded: Vec<u16> = OsString::from("Windows").encode_wide().collect();
        let error = windows_system_root_from(|buffer| {
            buffer[..encoded.len()].copy_from_slice(&encoded);
            encoded.len() as u32
        })
        .unwrap_err();

        assert!(error.to_string().contains("not absolute"));
    }

    #[test]
    fn helper_environment_is_cleared_and_allowlisted() {
        let mut command = Command::new(r"C:\ctx\ctx-pro.exe");
        command
            .env("PATH", r"C:\untrusted")
            .env("TEMP", r"C:\secret-temp")
            .env("TMP", r"C:\secret-tmp")
            .env("USERPROFILE", r"C:\Users\secret")
            .env("APPDATA", r"C:\Users\secret\AppData\Roaming")
            .env("LOCALAPPDATA", r"C:\Users\secret\AppData\Local")
            .env("CTX_TEST_SECRET", "must-not-survive")
            .env("SystemRoot", r"Z:\attacker-controlled");

        configure_environment_with_preparation_workers(
            &mut command,
            Path::new(r"C:\ctx\data"),
            "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8",
            Some(Path::new(r"C:\Program Files\Git\cmd\git.exe")),
            8,
        )
        .unwrap();

        let environment: BTreeMap<_, _> = command
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.unwrap().to_os_string()))
            .collect();
        let keys: Vec<_> = environment
            .keys()
            .map(|key| key.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            keys,
            [
                "CTX_DATA_ROOT",
                "CTX_PRO_CORE_PREPARATION_WORKERS",
                "CTX_PRO_DATA_ROOT",
                "CTX_PRO_GIT_EXECUTABLE",
                "CTX_PRO_INSTALLATION_ID",
                "SystemRoot",
            ]
        );
        assert!(Path::new(environment.get(&OsString::from("SystemRoot")).unwrap()).is_absolute());
        assert_ne!(
            environment.get(&OsString::from("SystemRoot")).unwrap(),
            &OsString::from(r"Z:\attacker-controlled")
        );
    }
}
