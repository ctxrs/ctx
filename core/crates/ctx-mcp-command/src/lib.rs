use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ctx_bundled_assets as bundled_assets;
use ctx_core::boolish::parse_boolish;
use sha2::Digest;

const CTX_MCP_COMMAND_ENV: &str = "CTX_MCP_COMMAND";
const CTX_MCP_DISABLED_ENV: &str = "CTX_MCP_DISABLED";
const CTX_MCP_RUNTIME_ID: &str = "ctx-mcp";

pub fn configure_runtime_mcp_command(
    provider_id: &str,
    provider_env: &mut HashMap<String, String>,
    data_root: &Path,
) -> Result<()> {
    if !provider_supports_ctx_mcp(provider_id) {
        provider_env.insert(CTX_MCP_DISABLED_ENV.to_string(), "1".to_string());
        provider_env.remove(CTX_MCP_COMMAND_ENV);
        return Ok(());
    }

    if !mcp_enabled(provider_env) {
        return Ok(());
    }

    if provider_env_targets_linux_sandbox(provider_env) {
        let bundled = bundled_assets::bundled_runtime_for(
            CTX_MCP_RUNTIME_ID,
            "linux",
            std::env::consts::ARCH,
        )
        .ok_or_else(|| {
            anyhow::anyhow!(
                "linux sandbox ctx-mcp runtime is unavailable for {}",
                std::env::consts::ARCH
            )
        })?;

        let staged_path = stage_linux_sandbox_mcp_runtime(data_root, &bundled)?;
        provider_env.insert(
            CTX_MCP_COMMAND_ENV.to_string(),
            staged_path.to_string_lossy().to_string(),
        );
        return Ok(());
    }

    if let Some(command) = provider_env
        .get(CTX_MCP_COMMAND_ENV)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        validate_explicit_mcp_command(command)?;
        return Ok(());
    }

    let bundled = bundled_assets::bundled_runtime_for(
        CTX_MCP_RUNTIME_ID,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
    .ok_or_else(|| {
        anyhow::anyhow!(
            "host ctx-mcp runtime is unavailable for {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    provider_env.insert(
        CTX_MCP_COMMAND_ENV.to_string(),
        bundled.bin.to_string_lossy().to_string(),
    );
    Ok(())
}

fn validate_explicit_mcp_command(command: &str) -> Result<()> {
    let path = Path::new(command);
    if !path.is_absolute() && !looks_like_windows_absolute_path(command) {
        anyhow::bail!("CTX_MCP_COMMAND must be an explicit absolute path, got `{command}`");
    }
    if !path.exists() {
        anyhow::bail!("CTX_MCP_COMMAND path does not exist: {command}");
    }
    Ok(())
}

fn looks_like_windows_absolute_path(command: &str) -> bool {
    let bytes = command.as_bytes();
    bytes.len() >= 3
        && bytes[1] == b':'
        && bytes[0].is_ascii_alphabetic()
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn provider_supports_ctx_mcp(provider_id: &str) -> bool {
    match provider_id {
        "codex" | "claude-crp" => true,
        "fake" | "broken" | "opencode" | "kimi" => false,
        id => ctx_provider_runtime::provider_adapters::is_acp_provider_id(id),
    }
}

fn mcp_enabled(provider_env: &HashMap<String, String>) -> bool {
    provider_env
        .get(CTX_MCP_DISABLED_ENV)
        .and_then(|value| parse_boolish(value))
        .map(|disabled| !disabled)
        .unwrap_or(true)
}

fn provider_env_targets_linux_sandbox(provider_env: &HashMap<String, String>) -> bool {
    provider_env
        .get(ctx_harness_runtime::CTX_HARNESS_LINUX_SANDBOX_ENV)
        .is_some_and(|value| value == "1")
        || provider_env.contains_key("CTX_HARNESS_CONTAINER_ID")
}

fn stage_linux_sandbox_mcp_runtime(
    data_root: &Path,
    bundled: &bundled_assets::BundledRuntimePaths,
) -> Result<PathBuf> {
    let expected_sha256 = validate_runtime_sha256_metadata(&bundled.sha256)?;
    let runtime_dir = data_root
        .join("runtimes")
        .join(CTX_MCP_RUNTIME_ID)
        .join(&bundled.version);
    std::fs::create_dir_all(&runtime_dir).with_context(|| {
        format!(
            "creating linux sandbox ctx-mcp runtime dir {}",
            runtime_dir.display()
        )
    })?;

    let file_name = bundled
        .bin
        .file_name()
        .context("bundled ctx-mcp runtime missing binary file name")?;
    let staged_path = runtime_dir.join(file_name);
    if staged_path.exists() {
        let digest = sha256_file(&staged_path).with_context(|| {
            format!("verifying staged ctx-mcp runtime {}", staged_path.display())
        })?;
        if digest.eq_ignore_ascii_case(&expected_sha256) {
            return Ok(staged_path);
        }
        std::fs::remove_file(&staged_path).with_context(|| {
            format!(
                "removing checksum-mismatched staged ctx-mcp runtime {}",
                staged_path.display()
            )
        })?;
    }

    let bundled_digest = sha256_file(&bundled.bin).with_context(|| {
        format!(
            "verifying bundled linux sandbox ctx-mcp runtime {}",
            bundled.bin.display()
        )
    })?;
    if !bundled_digest.eq_ignore_ascii_case(&expected_sha256) {
        anyhow::bail!(
            "bundled linux sandbox ctx-mcp runtime checksum mismatch: expected {}, got {}",
            expected_sha256,
            bundled_digest
        );
    }

    let staged_tmp = runtime_dir.join(format!(
        ".{}.tmp-{}-{}",
        file_name.to_string_lossy(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default(),
    ));

    std::fs::copy(&bundled.bin, &staged_tmp).with_context(|| {
        format!(
            "staging linux sandbox ctx-mcp runtime from {} to {}",
            bundled.bin.display(),
            staged_tmp.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&staged_tmp)
            .with_context(|| format!("stat staged ctx-mcp runtime {}", staged_tmp.display()))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&staged_tmp, perms).with_context(|| {
            format!(
                "marking staged linux sandbox ctx-mcp runtime executable at {}",
                staged_tmp.display()
            )
        })?;
    }

    let staged_tmp_digest = sha256_file(&staged_tmp).with_context(|| {
        format!(
            "verifying staged linux sandbox ctx-mcp runtime {}",
            staged_tmp.display()
        )
    })?;
    if !staged_tmp_digest.eq_ignore_ascii_case(&expected_sha256) {
        let _ = std::fs::remove_file(&staged_tmp);
        anyhow::bail!(
            "staged linux sandbox ctx-mcp runtime checksum mismatch: expected {}, got {}",
            expected_sha256,
            staged_tmp_digest
        );
    }

    if let Err(err) = std::fs::rename(&staged_tmp, &staged_path) {
        if staged_path.exists() {
            let digest = sha256_file(&staged_path).with_context(|| {
                format!(
                    "verifying concurrently staged ctx-mcp runtime {}",
                    staged_path.display()
                )
            })?;
            if !digest.eq_ignore_ascii_case(&expected_sha256) {
                let _ = std::fs::remove_file(&staged_tmp);
                anyhow::bail!(
                    "concurrently staged linux sandbox ctx-mcp runtime checksum mismatch: expected {}, got {}",
                    expected_sha256,
                    digest
                );
            }
            let _ = std::fs::remove_file(&staged_tmp);
            return Ok(staged_path);
        }
        return Err(err).with_context(|| {
            format!(
                "finalizing staged linux sandbox ctx-mcp runtime {} -> {}",
                staged_tmp.display(),
                staged_path.display()
            )
        });
    }

    let final_digest = sha256_file(&staged_path).with_context(|| {
        format!(
            "verifying finalized linux sandbox ctx-mcp runtime {}",
            staged_path.display()
        )
    })?;
    if !final_digest.eq_ignore_ascii_case(&expected_sha256) {
        let _ = std::fs::remove_file(&staged_path);
        anyhow::bail!(
            "finalized linux sandbox ctx-mcp runtime checksum mismatch: expected {}, got {}",
            expected_sha256,
            final_digest
        );
    }

    Ok(staged_path)
}

fn validate_runtime_sha256_metadata(raw: &str) -> Result<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.len() != 64 || !trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("linux sandbox ctx-mcp runtime is missing valid sha256 metadata");
    }
    Ok(trimmed)
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading file for sha256 {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests;
