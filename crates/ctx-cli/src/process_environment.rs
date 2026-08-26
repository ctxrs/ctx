use std::{
    env,
    ffi::{OsStr, OsString},
    process::Command,
};

const RELEASE_AUTHORITY_ENV_VARS: &[&str] = &[
    "CTX_ALLOW_CUSTOM_RELEASE_BASE_URL",
    "CTX_FUNCTIONS_BASE",
    "CTX_UPGRADE_FUNCTIONS_BASE",
];

/// Prevent a child process from inheriting ambient release-root authority.
///
/// Call this after adding command-specific environment entries and immediately
/// before spawning or replacing the current process. Normal runtime controls
/// remain inherited; only release metadata, signature, key, and artifact-origin
/// substitution is removed.
pub(crate) fn sanitize_release_authority_env(command: &mut Command) -> &mut Command {
    let configured_authority = command
        .get_envs()
        .filter(|(key, _)| is_release_authority_env_var(key))
        .map(|(key, _)| key.to_os_string())
        .collect::<Vec<_>>();
    let inherited_authority = env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| is_release_authority_env_var(key))
        .collect::<Vec<_>>();

    for key in RELEASE_AUTHORITY_ENV_VARS
        .iter()
        .map(OsString::from)
        .chain(inherited_authority)
        .chain(configured_authority)
    {
        command.env_remove(key);
    }
    command
}

fn is_release_authority_env_var(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    let upper = key.to_ascii_uppercase();
    upper.starts_with("CTX_RELEASE_")
        || RELEASE_AUTHORITY_ENV_VARS
            .iter()
            .any(|forbidden| upper == *forbidden)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs, process::Stdio};

    const DESCENDANT_PROBE_PATH: &str = "CTX_SANITIZER_DESCENDANT_PROBE_PATH";
    const DESCENDANT_PROBE_STAGE: &str = "CTX_SANITIZER_DESCENDANT_PROBE_STAGE";
    const DESCENDANT_PROBE_TEST: &str =
        "process_environment::tests::sanitized_detached_descendant_probe";

    #[test]
    fn sanitizer_is_narrow_and_covers_future_release_authority_names() {
        let mut command = Command::new("ctx-child");
        command
            .env("CTX_RELEASE_METADATA_URL", "file:///attacker/metadata")
            .env(
                "CTX_RELEASE_METADATA_SIGNATURE_URL",
                "custom://attacker/signature",
            )
            .env("CTX_RELEASE_FUTURE_AUTHORITY", "attacker")
            .env("CTX_UPGRADE_FUNCTIONS_BASE", "https://attacker.invalid")
            .env("CTX_FUNCTIONS_BASE", "https://legacy-attacker.invalid")
            .env("CTX_ALLOW_CUSTOM_RELEASE_BASE_URL", "1")
            .env("CTX_UPGRADE_CHANNEL", "stable")
            .env("CTX_UPGRADE_AUTO", "off")
            .env("CTX_DATA_ROOT", "/legitimate/data");

        sanitize_release_authority_env(&mut command);

        let configured = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for forbidden in [
            "CTX_RELEASE_METADATA_URL",
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            "CTX_RELEASE_FUTURE_AUTHORITY",
            "CTX_UPGRADE_FUNCTIONS_BASE",
            "CTX_FUNCTIONS_BASE",
            "CTX_ALLOW_CUSTOM_RELEASE_BASE_URL",
        ] {
            assert_eq!(
                configured.get(forbidden),
                Some(&None),
                "{forbidden} was not removed"
            );
        }
        assert_eq!(
            configured.get("CTX_UPGRADE_CHANNEL"),
            Some(&Some("stable".to_owned()))
        );
        assert_eq!(
            configured.get("CTX_UPGRADE_AUTO"),
            Some(&Some("off".to_owned()))
        );
        assert_eq!(
            configured.get("CTX_DATA_ROOT"),
            Some(&Some("/legitimate/data".to_owned()))
        );
    }

    #[test]
    fn sanitized_spawn_blocks_detached_descendant_inheritance() {
        let temp = tempfile::tempdir().expect("create sanitizer test root");
        let probe = temp.path().join("descendant-environment.txt");
        let mut command = Command::new(env::current_exe().expect("resolve current test binary"));
        command
            .args(["--exact", DESCENDANT_PROBE_TEST, "--nocapture"])
            .env(DESCENDANT_PROBE_PATH, &probe)
            .env("CTX_RELEASE_METADATA_URL", "file:///attacker/metadata")
            .env(
                "CTX_RELEASE_METADATA_SIGNATURE_URL",
                "custom://attacker/signature",
            )
            .env("CTX_RELEASE_METADATA_PUBLIC_KEY_PEM", "attacker-key")
            .env("CTX_RELEASE_FUTURE_AUTHORITY", "attacker")
            .env("CTX_UPGRADE_FUNCTIONS_BASE", "https://attacker.invalid")
            .env("CTX_FUNCTIONS_BASE", "https://legacy-attacker.invalid")
            .env("CTX_ALLOW_CUSTOM_RELEASE_BASE_URL", "1")
            .env("CTX_UPGRADE_CHANNEL", "stable")
            .env("CTX_UPGRADE_AUTO", "off");
        sanitize_release_authority_env(&mut command);

        assert!(command
            .status()
            .expect("run sanitized child process")
            .success());
        assert_eq!(
            fs::read_to_string(probe).expect("read detached descendant probe"),
            "sanitized:stable:off"
        );
    }

    #[test]
    fn sanitized_detached_descendant_probe() {
        let Some(probe) = env::var_os(DESCENDANT_PROBE_PATH) else {
            return;
        };
        assert!(
            !env::vars_os().any(|(key, _)| is_release_authority_env_var(&key)),
            "release authority leaked into descendant environment"
        );
        assert_eq!(env::var("CTX_UPGRADE_CHANNEL").as_deref(), Ok("stable"));
        assert_eq!(env::var("CTX_UPGRADE_AUTO").as_deref(), Ok("off"));

        if env::var_os(DESCENDANT_PROBE_STAGE).is_none() {
            let mut descendant =
                Command::new(env::current_exe().expect("resolve descendant test binary"));
            descendant
                .args(["--exact", DESCENDANT_PROBE_TEST, "--nocapture"])
                .env(DESCENDANT_PROBE_STAGE, "detached")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            assert!(descendant
                .spawn()
                .expect("spawn detached descendant")
                .wait()
                .expect("wait for detached descendant")
                .success());
            return;
        }

        fs::write(probe, "sanitized:stable:off").expect("write descendant environment probe");
    }
}
