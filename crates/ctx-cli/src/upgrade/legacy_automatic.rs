#[cfg(unix)]
mod unix {
    use std::{
        env, fs,
        os::unix::{ffi::OsStringExt as _, process::CommandExt as _},
        path::{Path, PathBuf},
        process::{self, Command, Stdio},
        time::{Duration, Instant},
    };

    use anyhow::{anyhow, Context, Result};
    use ctx_history_core::default_data_root;
    use uuid::Uuid;

    use crate::semantic::{
        begin_legacy_daemon_upgrade_handoff, complete_replacement_daemon_handoff,
        finish_replacement_daemon_handoff, replacement_helper_owns_daemon_handoff,
    };

    use super::super::{
        env_flag,
        install::{classify_install_marker_at, ManagedInstallMarker},
        platform_key, sha256_hex,
    };

    const LEGACY_BACKGROUND_ENV: &str = "CTX_UPGRADE_BACKGROUND_CHILD";
    const HELPER_ENV: &str = "CTX_LEGACY_AUTO_UPGRADE_HANDOFF_HELPER";
    const HELPER_DATA_ROOT_ENV: &str = "CTX_LEGACY_AUTO_UPGRADE_DATA_ROOT";
    const HELPER_HANDOFF_ID_ENV: &str = "CTX_LEGACY_AUTO_UPGRADE_HANDOFF_ID";
    const HELPER_TARGET_ENV: &str = "CTX_LEGACY_AUTO_UPGRADE_TARGET";
    const HELPER_CANDIDATE_SHA_ENV: &str = "CTX_LEGACY_AUTO_UPGRADE_CANDIDATE_SHA256";
    const LEGACY_VERSION: &str = "0.25.0";
    const HELPER_TIMEOUT: Duration = Duration::from_secs(30);

    pub(super) fn run() -> Result<bool> {
        if env::var_os(HELPER_ENV).is_some() {
            run_replacement_helper()?;
            return Ok(true);
        }
        if !is_legacy_staged_version_probe() {
            return Ok(false);
        }

        let staged = env::current_exe().context("resolve staged ctx upgrade candidate")?;
        let Some(target) = legacy_managed_target(&staged)? else {
            return Ok(false);
        };
        let data_root = legacy_data_root()?;
        let attempt_id = format!("ua_legacy_{}", Uuid::now_v7());
        let handoff = begin_legacy_daemon_upgrade_handoff(&data_root, &attempt_id, &target)
            .context("quiesce legacy ctx daemon before automatic replacement")?;
        let candidate_sha256 = sha256_hex(
            &fs::read(&staged)
                .with_context(|| format!("read staged ctx candidate {}", staged.display()))?,
        );
        let mut command = Command::new(&staged);
        command
            .env(HELPER_ENV, "1")
            .env(HELPER_DATA_ROOT_ENV, &data_root)
            .env(HELPER_HANDOFF_ID_ENV, &attempt_id)
            .env(HELPER_TARGET_ENV, &target)
            .env(HELPER_CANDIDATE_SHA_ENV, candidate_sha256)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .context("spawn legacy automatic-upgrade handoff helper")?;
        handoff.transfer_to_replacement_helper(child.id())?;
        Ok(false)
    }

    fn is_legacy_staged_version_probe() -> bool {
        if !env_flag(LEGACY_BACKGROUND_ENV) {
            return false;
        }
        let arguments = env::args_os().collect::<Vec<_>>();
        if arguments.len() != 2 || arguments[1] != "--version" {
            return false;
        }
        Path::new(&arguments[0])
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".ctx-upgrade-") && name.ends_with(".new"))
    }

    fn legacy_managed_target(staged: &Path) -> Result<Option<PathBuf>> {
        let parent = staged
            .parent()
            .ok_or_else(|| anyhow!("staged ctx upgrade candidate has no parent"))?;
        let mut matches = Vec::new();
        for entry in fs::read_dir(parent).with_context(|| {
            format!("inspect managed ctx install directory {}", parent.display())
        })? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(target_name) = name.strip_suffix(".install.json") else {
                continue;
            };
            let target = parent.join(target_name);
            if matches!(
                classify_install_marker_at(&target, platform_key()?),
                ManagedInstallMarker::Valid(ref marker) if marker.version == LEGACY_VERSION
            ) {
                matches.push(target);
            }
        }
        if matches.len() > 1 {
            return Err(anyhow!(
                "multiple managed ctx 0.25.0 installations share the staged upgrade directory"
            ));
        }
        Ok(matches.pop())
    }

    fn legacy_data_root() -> Result<PathBuf> {
        if let Some(root) = env::var_os("CTX_DATA_ROOT") {
            return validate_data_root(PathBuf::from(root));
        }
        #[cfg(target_os = "linux")]
        if let Some(root) = linux_parent_data_root() {
            return validate_data_root(root);
        }
        validate_data_root(default_data_root()?)
    }

    fn validate_data_root(root: PathBuf) -> Result<PathBuf> {
        if !root.is_absolute() {
            return Err(anyhow!(
                "legacy automatic-upgrade data root must be absolute: {}",
                root.display()
            ));
        }
        Ok(root)
    }

    #[cfg(target_os = "linux")]
    fn linux_parent_data_root() -> Option<PathBuf> {
        let parent = unsafe { libc::getppid() };
        if parent <= 0 {
            return None;
        }
        let bytes = fs::read(format!("/proc/{parent}/cmdline")).ok()?;
        let arguments = bytes
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| std::ffi::OsString::from_vec(argument.to_vec()))
            .collect::<Vec<_>>();
        for (index, argument) in arguments.iter().enumerate() {
            if argument == "--data-root" {
                return arguments.get(index + 1).map(PathBuf::from);
            }
            if let Some(value) = argument
                .to_str()
                .and_then(|value| value.strip_prefix("--data-root="))
            {
                return Some(PathBuf::from(value));
            }
        }
        None
    }

    fn run_replacement_helper() -> Result<()> {
        let data_root = required_absolute_path(HELPER_DATA_ROOT_ENV)?;
        let target = required_absolute_path(HELPER_TARGET_ENV)?;
        let handoff_id = required_text(HELPER_HANDOFF_ID_ENV)?;
        if !crate::upgrade::is_valid_upgrade_attempt_id(&handoff_id) {
            return Err(anyhow!("invalid legacy automatic-upgrade handoff identity"));
        }
        let candidate_sha256 = required_text(HELPER_CANDIDATE_SHA_ENV)?;
        if candidate_sha256.len() != 64
            || !candidate_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(anyhow!("invalid legacy automatic-upgrade candidate digest"));
        }
        let deadline = Instant::now() + helper_timeout();
        while !replacement_helper_owns_daemon_handoff(&data_root, &handoff_id, process::id()) {
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "legacy automatic-upgrade helper never received durable handoff ownership"
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let legacy_lock = data_root.join("upgrade.lock");
        loop {
            if !legacy_lock.exists() {
                match classify_install_marker_at(&target, platform_key()?) {
                    ManagedInstallMarker::Valid(marker)
                        if marker.sha256.eq_ignore_ascii_case(&candidate_sha256) =>
                    {
                        complete_replacement_daemon_handoff(
                            &data_root,
                            &target,
                            &handoff_id,
                            None,
                        )?;
                        finish_replacement_daemon_handoff(&data_root, &handoff_id)?;
                        return Ok(());
                    }
                    ManagedInstallMarker::Valid(marker) if marker.version == LEGACY_VERSION => {
                        complete_replacement_daemon_handoff(
                            &data_root,
                            &target,
                            &handoff_id,
                            None,
                        )?;
                        finish_replacement_daemon_handoff(&data_root, &handoff_id)?;
                        return Ok(());
                    }
                    _ => {}
                }
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "legacy automatic ctx replacement did not publish a verified old or new managed image"
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn required_absolute_path(key: &str) -> Result<PathBuf> {
        validate_data_root(PathBuf::from(
            env::var_os(key).ok_or_else(|| anyhow!("missing {key}"))?,
        ))
    }

    fn required_text(key: &str) -> Result<String> {
        env::var(key).with_context(|| format!("missing {key}"))
    }

    fn helper_timeout() -> Duration {
        if crate::upgrade::test_harness_enabled() {
            if let Ok(milliseconds) = env::var("CTX_LEGACY_UPGRADE_HELPER_TIMEOUT_MS_FOR_TESTS") {
                if let Ok(milliseconds) = milliseconds.parse::<u64>() {
                    return Duration::from_millis(milliseconds.clamp(100, 30_000));
                }
            }
        }
        HELPER_TIMEOUT
    }
}

pub(crate) fn run_legacy_automatic_upgrade_bridge() -> anyhow::Result<bool> {
    #[cfg(unix)]
    {
        unix::run()
    }
    #[cfg(not(unix))]
    {
        Ok(false)
    }
}
