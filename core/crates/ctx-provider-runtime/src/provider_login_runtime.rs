use std::path::Path;

use anyhow::Context;

#[derive(Debug, Clone)]
pub struct ProviderLoginRuntimeCommand {
    pub command_abs_path: String,
    pub args: Vec<String>,
}

impl From<ctx_managed_installs::ProviderRuntimeCommand> for ProviderLoginRuntimeCommand {
    fn from(value: ctx_managed_installs::ProviderRuntimeCommand) -> Self {
        Self {
            command_abs_path: value.command_abs_path,
            args: value.args,
        }
    }
}

async fn resolve_runtime_provider_command_from_config(
    data_root: &Path,
    provider_id: &str,
) -> anyhow::Result<Option<ctx_managed_installs::ProviderRuntimeCommand>> {
    let cfg = ctx_managed_installs::load_agent_server_config(data_root)
        .await
        .context("loading agent server config")?;
    ctx_managed_installs::resolve_runtime_provider_command(&cfg, provider_id)
        .with_context(|| format!("resolving runtime command for {provider_id}"))
}

async fn resolve_provider_login_command_from_config(
    data_root: &Path,
    provider_id: &str,
) -> anyhow::Result<Option<ctx_managed_installs::ProviderRuntimeCommand>> {
    let cfg = ctx_managed_installs::load_agent_server_config(data_root)
        .await
        .context("loading agent server config")?;
    ctx_managed_installs::resolve_provider_login_command(&cfg, provider_id)
        .with_context(|| format!("resolving prepared login executable for {provider_id}"))
}

fn is_cursor_login_command(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "cursor-agent" || name == "cursor-agent.exe")
}

pub async fn resolve_cursor_login_runtime_from_config(
    data_root: &Path,
) -> anyhow::Result<ProviderLoginRuntimeCommand> {
    if let Some(runtime) = resolve_provider_login_command_from_config(data_root, "cursor").await? {
        if is_cursor_login_command(Path::new(&runtime.command_abs_path)) {
            return Ok(runtime.into());
        }
        anyhow::bail!(
            "runtime_command_invalid: provider=cursor-login (configured login executable must point to `cursor-agent`)"
        );
    }

    if let Some(runtime) = resolve_runtime_provider_command_from_config(data_root, "cursor").await?
    {
        if matches!(
            runtime.source,
            ctx_managed_installs::ProviderRuntimeCommandSource::BundledSeed
        ) {
            anyhow::bail!(
                "runtime_command_missing: provider=cursor-login (ctx requires a managed or explicitly configured `cursor-agent` login executable; bundled runtime discovery is not supported)"
            );
        }
        if is_cursor_login_command(Path::new(&runtime.command_abs_path)) {
            return Ok(runtime.into());
        }
        anyhow::bail!(
            "runtime_command_invalid: provider=cursor-login (configured runtime command must point to `cursor-agent`)"
        );
    }

    anyhow::bail!(
        "runtime_command_missing: provider=cursor-login (ctx requires a managed or explicitly configured `cursor-agent` login executable; host PATH lookup is not supported)"
    );
}

pub async fn resolve_claude_login_runtime_from_config(
    data_root: &Path,
) -> anyhow::Result<ProviderLoginRuntimeCommand> {
    if let Some(runtime_command) =
        resolve_runtime_provider_command_from_config(data_root, "claude-cli").await?
    {
        return Ok(runtime_command.into());
    }

    anyhow::bail!(
        "runtime_command_missing: provider=claude-cli (ctx requires a managed or explicitly configured Claude CLI runtime command; host PATH lookup is not supported)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(prev) = self.prev.as_deref() {
                    std::env::set_var(self.key, prev);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn write_mock_command(dir: &Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write mock command");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)
                .expect("mock command metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("set mock command permissions");
        }
        path
    }

    #[tokio::test]
    async fn cursor_login_runtime_requires_managed_or_configured_command() {
        let _env_lock = ENV_LOCK.lock().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let _host_cursor = write_mock_command(temp.path(), "cursor-agent");
        let existing_path = std::env::var("PATH").unwrap_or_default();
        let combined_path = if existing_path.is_empty() {
            temp.path().to_string_lossy().to_string()
        } else {
            format!("{}:{}", temp.path().to_string_lossy(), existing_path)
        };
        let _path_guard = EnvGuard::set("PATH", &combined_path);

        let err = resolve_cursor_login_runtime_from_config(temp.path())
            .await
            .expect_err("host PATH cursor-agent should not be accepted");
        assert!(err
            .to_string()
            .contains("runtime_command_missing: provider=cursor-login"));
        assert!(err
            .to_string()
            .contains("host PATH lookup is not supported"));

        let cfg = ctx_managed_installs::load_agent_server_config(temp.path())
            .await
            .expect("load config");
        assert!(
            !cfg.provider_login_executables.contains_key("cursor"),
            "host discovery must not persist a cursor login executable"
        );
    }

    #[tokio::test]
    async fn cursor_login_runtime_accepts_configured_login_command() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime_path = write_mock_command(temp.path(), "cursor-agent");
        let runtime_path_str = runtime_path.to_string_lossy().to_string();
        let mut cfg = ctx_managed_installs::load_agent_server_config(temp.path())
            .await
            .expect("load config");
        cfg.provider_login_executables.insert(
            "cursor".to_string(),
            ctx_managed_installs::ProviderLoginExecutable {
                executable_path: runtime_path_str,
            },
        );
        ctx_managed_installs::save_agent_server_config(temp.path(), &cfg)
            .await
            .expect("save config");

        let resolved = resolve_cursor_login_runtime_from_config(temp.path())
            .await
            .expect("resolve configured login command");
        let expected = std::fs::canonicalize(&runtime_path).unwrap_or(runtime_path);
        assert_eq!(
            resolved.command_abs_path,
            expected.to_string_lossy().to_string()
        );
        assert!(resolved.args.is_empty());
    }

    #[tokio::test]
    async fn cursor_login_runtime_accepts_configured_runtime_command() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime_path = write_mock_command(temp.path(), "cursor-agent");
        let runtime_path_str = runtime_path.to_string_lossy().to_string();
        let mut cfg = ctx_managed_installs::load_agent_server_config(temp.path())
            .await
            .expect("load config");
        cfg.providers.insert(
            "cursor".to_string(),
            ctx_managed_installs::AgentServerCommand {
                command: runtime_path_str,
                args: vec!["cli.js".to_string()],
                dependencies: Vec::new(),
                managed: None,
            },
        );
        ctx_managed_installs::save_agent_server_config(temp.path(), &cfg)
            .await
            .expect("save config");

        let resolved = resolve_cursor_login_runtime_from_config(temp.path())
            .await
            .expect("resolve configured runtime command");
        let expected = std::fs::canonicalize(&runtime_path).unwrap_or(runtime_path);
        assert_eq!(
            resolved.command_abs_path,
            expected.to_string_lossy().to_string()
        );
        assert_eq!(resolved.args, vec!["cli.js".to_string()]);
    }

    #[tokio::test]
    async fn claude_login_runtime_requires_managed_or_configured_command() {
        let temp = tempfile::tempdir().expect("tempdir");

        let err = resolve_claude_login_runtime_from_config(temp.path())
            .await
            .expect_err("missing managed/configured claude login command should fail");
        assert!(err
            .to_string()
            .contains("runtime_command_missing: provider=claude-cli"));
        assert!(err
            .to_string()
            .contains("host PATH lookup is not supported"));
    }

    #[tokio::test]
    async fn claude_login_runtime_uses_configured_runtime_command() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime_path = write_mock_command(temp.path(), "claude-cli-mock.sh");
        let runtime_path_str = runtime_path.to_string_lossy().to_string();
        let mut cfg = ctx_managed_installs::load_agent_server_config(temp.path())
            .await
            .expect("load config for runtime resolution test");
        cfg.providers.insert(
            "claude-cli".to_string(),
            ctx_managed_installs::AgentServerCommand {
                command: runtime_path_str,
                args: vec!["--shim".to_string()],
                dependencies: vec!["dep-node".to_string()],
                managed: None,
            },
        );
        ctx_managed_installs::save_agent_server_config(temp.path(), &cfg)
            .await
            .expect("save config for runtime resolution test");

        let resolved = resolve_claude_login_runtime_from_config(temp.path())
            .await
            .expect("resolve runtime from config");
        assert!(resolved.command_abs_path.contains("claude-cli-mock.sh"));
        assert_eq!(resolved.args, vec!["--shim".to_string()]);
    }

    #[tokio::test]
    async fn claude_login_runtime_prefers_configured_runtime_command_when_login_command_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime_path = write_mock_command(temp.path(), "claude-cli-runtime-mock.sh");
        let runtime_path_str = runtime_path.to_string_lossy().to_string();
        let mut cfg = ctx_managed_installs::load_agent_server_config(temp.path())
            .await
            .expect("load config for runtime resolution test");
        cfg.providers.insert(
            "claude-cli".to_string(),
            ctx_managed_installs::AgentServerCommand {
                command: runtime_path_str,
                args: vec!["cli.js".to_string()],
                dependencies: Vec::new(),
                managed: None,
            },
        );
        ctx_managed_installs::save_agent_server_config(temp.path(), &cfg)
            .await
            .expect("save config for runtime resolution test");

        let resolved = resolve_claude_login_runtime_from_config(temp.path())
            .await
            .expect("resolve runtime from provider command");
        assert!(resolved
            .command_abs_path
            .contains("claude-cli-runtime-mock.sh"));
        assert_eq!(resolved.args, vec!["cli.js".to_string()]);
    }
}
