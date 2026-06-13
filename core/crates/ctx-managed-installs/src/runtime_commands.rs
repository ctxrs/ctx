use std::fs;
use std::path::{Path, Path as StdPath, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::AgentServerCommand;

fn file_stem_matches(path: &StdPath, name: &str) -> bool {
    path.file_stem()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(name))
        .unwrap_or(false)
}

fn resolve_existing_absolute_path(raw: &str, label: &str) -> Result<PathBuf> {
    let path = PathBuf::from(raw);
    anyhow::ensure!(
        path.is_absolute(),
        "{label} must be an absolute path; got '{raw}'"
    );
    anyhow::ensure!(path.exists(), "{label} does not exist: {}", path.display());
    Ok(path)
}

fn gemini_cli_root_from_entrypoint(path: &StdPath) -> Option<PathBuf> {
    let file_name = path.file_name()?.to_str()?;
    let bundle_dir = path.parent()?;
    let package_dir = bundle_dir.parent()?;
    let scope_dir = package_dir.parent()?;
    let node_modules_dir = scope_dir.parent()?;
    if file_name != "gemini.js"
        || bundle_dir.file_name()?.to_str()? != "bundle"
        || package_dir.file_name()?.to_str()? != "gemini-cli"
        || scope_dir.file_name()?.to_str()? != "@google"
        || node_modules_dir.file_name()?.to_str()? != "node_modules"
    {
        return None;
    }
    Some(package_dir.to_path_buf())
}

pub(crate) fn is_acp_provider_id(provider_id: &str) -> bool {
    matches!(
        provider_id,
        "gemini"
            | "qwen"
            | "cursor"
            | "pi"
            | "opencode"
            | "mistral"
            | "goose"
            | "kimi"
            | "auggie"
            | "amp"
            | "droid"
            | "copilot"
            | "cline"
            | "openhands"
    )
}

fn resolve_explicit_gemini_cli_paths(
    command: &str,
    args: &[String],
) -> Result<ExplicitGeminiCliPaths> {
    let node_path = resolve_existing_absolute_path(command, "Gemini ACP runtime command")?;
    anyhow::ensure!(
        file_stem_matches(&node_path, "node"),
        "Gemini ACP runtime must use an explicit absolute node executable plus @google/gemini-cli/bundle/gemini.js; got command '{command}'"
    );

    let arg0 = args.first().ok_or_else(|| {
        anyhow!(
            "Gemini ACP runtime must pass an explicit absolute @google/gemini-cli/bundle/gemini.js entrypoint as the first argument"
        )
    })?;
    let cli_entry_path = resolve_existing_absolute_path(arg0, "Gemini ACP entrypoint")?;
    let cli_root = gemini_cli_root_from_entrypoint(&cli_entry_path).ok_or_else(|| {
        anyhow!(
            "Gemini ACP entrypoint must point to @google/gemini-cli/bundle/gemini.js; got '{}'",
            cli_entry_path.display()
        )
    })?;
    let bundle_dir = cli_root.join("bundle");
    let mut core_entries = fs::read_dir(&bundle_dir)
        .with_context(|| format!("reading Gemini ACP bundle dir {}", bundle_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(|value| value.to_str()) == Some("js")
                && path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(|value| {
                        value.starts_with("core-") || value.eq_ignore_ascii_case("core.js")
                    })
                    .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !core_entries.is_empty(),
        "Gemini ACP bundled core entrypoint is missing under {}",
        bundle_dir.display()
    );
    core_entries.sort();
    anyhow::ensure!(
        cli_root.join("package.json").exists(),
        "Gemini ACP entrypoint must live under a node_modules/@google/gemini-cli install tree: {}",
        cli_entry_path.display()
    );
    Ok(ExplicitGeminiCliPaths {
        cli_entry_path,
        core_entry_paths: core_entries,
    })
}

fn maybe_wrap_gemini_acp_command(
    data_root: &Path,
    mut cmd: AgentServerCommand,
) -> Result<AgentServerCommand> {
    let paths = resolve_explicit_gemini_cli_paths(&cmd.command, &cmd.args)?;

    let wrapper_path = data_root
        .join("providers")
        .join("agent-servers")
        .join("gemini-acp-wrapper.mjs");
    if let Some(parent) = wrapper_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating Gemini ACP wrapper dir {}", parent.display()))?;
    }

    let core_imports = paths
        .core_entry_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            format!(
                "import * as coreCandidate{index} from 'file://{}';",
                path.to_string_lossy()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let core_candidates = (0..paths.core_entry_paths.len())
        .map(|index| format!("coreCandidate{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let wrapper_contents = format!(
        "{core_imports}\n\
const coreCandidates = [{core_candidates}];\n\
const core = coreCandidates.find((candidate) =>\n\
  candidate &&\n\
  candidate.coreEvents &&\n\
  candidate.CoreEvent &&\n\
  typeof candidate.writeToStdout === 'function' &&\n\
  typeof candidate.writeToStderr === 'function'\n\
);\n\
if (!core) {{\n\
  throw new Error('Gemini ACP core module exports are missing from bundled core entrypoints');\n\
}}\n\
const {{ coreEvents, CoreEvent, writeToStdout, writeToStderr }} = core;\n\
coreEvents.on(CoreEvent.Output, (payload) => {{\n\
  if (payload.isStderr) {{\n\
    writeToStderr(payload.chunk, payload.encoding);\n\
  }} else {{\n\
    writeToStdout(payload.chunk, payload.encoding);\n\
  }}\n\
}});\n\
coreEvents.on(CoreEvent.ConsoleLog, (payload) => {{\n\
  writeToStderr(String(payload?.content ?? '') + '\\n');\n\
}});\n\
const consentRaw = process.env.CTX_GEMINI_AUTO_OAUTH_CONSENT ?? '';\n\
const consentDisabled = consentRaw === '0' || consentRaw.toLowerCase() === 'false';\n\
if (!consentDisabled) {{\n\
  coreEvents.on(CoreEvent.ConsentRequest, (payload) => {{\n\
    if (typeof payload?.onConfirm === 'function') {{\n\
      payload.onConfirm(true);\n\
    }}\n\
  }});\n\
}}\n\
process.env.GEMINI_CLI_NO_RELAUNCH ??= 'true';\n\
await import('file://{}');\n",
        paths.cli_entry_path.to_string_lossy(),
    );

    let write_wrapper = match fs::read_to_string(&wrapper_path) {
        Ok(existing) => existing != wrapper_contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading Gemini ACP wrapper {}", wrapper_path.display()));
        }
    };
    if write_wrapper {
        fs::write(&wrapper_path, wrapper_contents)
            .with_context(|| format!("writing Gemini ACP wrapper {}", wrapper_path.display()))?;
    }

    let first = cmd.args.first_mut().ok_or_else(|| {
        anyhow!(
            "Gemini ACP runtime must pass an explicit absolute @google/gemini-cli/bundle/gemini.js entrypoint as the first argument"
        )
    })?;
    *first = wrapper_path.to_string_lossy().to_string();
    Ok(cmd)
}

fn maybe_set_qwen_openai_auth_type(mut cmd: AgentServerCommand) -> AgentServerCommand {
    if cmd.args.iter().any(|arg| arg == "--auth-type") {
        return cmd;
    }
    cmd.args.push("--auth-type".to_string());
    cmd.args.push("openai".to_string());
    cmd
}

fn maybe_set_bridge_env_override(mut cmd: AgentServerCommand) -> AgentServerCommand {
    if cmd.args.iter().any(|arg| arg == "--override-with-envs") {
        return cmd;
    }
    cmd.args.push("--override-with-envs".to_string());
    cmd
}

fn goose_args_include_developer_builtin(args: &[String]) -> bool {
    args.windows(2).any(|window| {
        window[0] == "--with-builtin"
            && window[1]
                .split(',')
                .any(|value| value.trim() == "developer")
    })
}

fn maybe_set_goose_acp_subcommand(mut cmd: AgentServerCommand) -> AgentServerCommand {
    if !cmd.args.iter().any(|arg| arg == "acp")
        && file_stem_matches(StdPath::new(&cmd.command), "goose")
    {
        cmd.args.insert(0, "acp".to_string());
    }
    if !goose_args_include_developer_builtin(&cmd.args) {
        cmd.args.push("--with-builtin".to_string());
        cmd.args.push("developer".to_string());
    }
    cmd
}

pub(crate) fn normalize_acp_provider_command(
    data_root: &Path,
    provider_id: &str,
    cmd: AgentServerCommand,
) -> Result<AgentServerCommand> {
    if !is_acp_provider_id(provider_id) {
        return Ok(cmd);
    }
    let cmd = if provider_id == "gemini" {
        maybe_wrap_gemini_acp_command(data_root, cmd)?
    } else {
        cmd
    };
    let cmd = if provider_id == "qwen" {
        maybe_set_qwen_openai_auth_type(cmd)
    } else {
        cmd
    };
    let cmd = if provider_id == "goose" {
        maybe_set_goose_acp_subcommand(cmd)
    } else {
        cmd
    };
    let cmd = if provider_id == "openhands" {
        maybe_set_bridge_env_override(cmd)
    } else {
        cmd
    };
    Ok(cmd)
}

pub(crate) fn acp_bridge_command(
    bridge_cmd: &AgentServerCommand,
    acp_cmd: AgentServerCommand,
) -> AgentServerCommand {
    let mut parts = Vec::with_capacity(1 + acp_cmd.args.len());
    parts.push(acp_cmd.command);
    parts.extend(acp_cmd.args);
    let mut args = bridge_cmd.args.clone();
    args.push("--acp-command".to_string());
    args.push(parts.join(" "));
    AgentServerCommand {
        command: bridge_cmd.command.clone(),
        args,
        dependencies: Vec::new(),
        managed: None,
    }
}

pub(crate) fn managed_provider_runtime_command(
    data_root: &Path,
    provider_id: &str,
    managed_cmd: AgentServerCommand,
    bridge_cmd: Option<&AgentServerCommand>,
) -> Result<AgentServerCommand> {
    if !is_acp_provider_id(provider_id) {
        return Ok(managed_cmd);
    }

    let bridge_cmd = bridge_cmd.ok_or_else(|| {
        anyhow!("ACP bridge runtime is not configured or invalid for provider '{provider_id}'")
    })?;
    let acp_cmd = normalize_acp_provider_command(data_root, provider_id, managed_cmd)?;
    Ok(acp_bridge_command(bridge_cmd, acp_cmd))
}
#[derive(Debug, Clone)]
struct ExplicitGeminiCliPaths {
    cli_entry_path: PathBuf,
    core_entry_paths: Vec<PathBuf>,
}
