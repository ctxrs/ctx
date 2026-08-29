use anyhow::{anyhow, Context, Result};
use ctx_history_platform::platform_security::verify_private_file_handle;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use crate::NormalizedLaunch;

pub const SUPERVISOR_ENVIRONMENT_FILE_ENV: &str = "CTX_INTERNAL_SUPERVISOR_ENVIRONMENT_FILE";
const SUPERVISOR_ENVIRONMENT_FILE: &str = "supervisor-environment.json";

pub fn supervisor_environment_path(data_root: &Path) -> PathBuf {
    crate::daemon_root_path(data_root).join(SUPERVISOR_ENVIRONMENT_FILE)
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SupervisorIdentity {
    name: String,
    artifact_path: PathBuf,
}

impl SupervisorIdentity {
    pub fn new(name: impl Into<String>, artifact_path: PathBuf) -> Result<Self> {
        let name = name.into();
        let name = validated_supervisor_artifact_text("supervisor identity", &name)?;
        if name.is_empty() {
            return Err(anyhow!("supervisor identity may not be empty"));
        }
        Ok(Self {
            name: name.to_owned(),
            artifact_path,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn artifact_path(&self) -> &Path {
        &self.artifact_path
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SupervisorSpec {
    identity: SupervisorIdentity,
    description: String,
    environment_path: PathBuf,
    launch: NormalizedLaunch,
}

impl SupervisorSpec {
    pub fn new(
        identity: SupervisorIdentity,
        description: impl Into<String>,
        environment_path: PathBuf,
        launch: NormalizedLaunch,
    ) -> Result<Self> {
        let description =
            validated_supervisor_artifact_text("service description", &description.into())?
                .to_owned();
        validated_supervisor_artifact_path("environment handoff path", &environment_path)?;
        for (name, value) in launch.environment() {
            let name = name
                .to_str()
                .ok_or_else(|| anyhow!("supervisor environment name is not Unicode"))?;
            validated_supervisor_artifact_text("environment variable name", name)?;
            let value = value
                .to_str()
                .ok_or_else(|| anyhow!("supervisor environment value {name} is not Unicode"))?;
            validated_supervisor_artifact_text(&format!("environment variable {name}"), value)?;
        }
        Ok(Self {
            identity,
            description,
            environment_path,
            launch,
        })
    }

    pub fn identity(&self) -> &SupervisorIdentity {
        &self.identity
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn environment_path(&self) -> &Path {
        &self.environment_path
    }

    pub fn launch(&self) -> &NormalizedLaunch {
        &self.launch
    }
}

pub fn linux_systemd_unit(spec: &SupervisorSpec) -> Result<String> {
    let executable =
        validated_supervisor_artifact_path("daemon executable", spec.launch.program())?;
    let environment_path =
        validated_supervisor_artifact_path("environment handoff path", spec.environment_path())?;
    let args =
        spec.launch
            .args()
            .map(|arg| {
                let arg = arg
                    .to_str()
                    .ok_or_else(|| anyhow!("supervisor argument is not Unicode"))?;
                validated_supervisor_artifact_text("daemon argument", arg)?;
                Ok(
                    if arg.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'=')
                    }) {
                        arg.to_owned()
                    } else {
                        systemd_quote_text(arg)
                    },
                )
            })
            .collect::<Result<Vec<_>>>()?
            .join(" ");
    Ok(format!(
        "[Unit]\nDescription={}\n\n[Service]\nType=simple\nEnvironment={}\nExecStart={}{}\nRestart=always\nRestartSec=2\nStandardOutput=null\nStandardError=journal\n\n[Install]\nWantedBy=default.target\n",
        spec.description(),
        systemd_quote_text(&format!(
            "{SUPERVISOR_ENVIRONMENT_FILE_ENV}={environment_path}"
        )),
        systemd_quote_text(executable),
        if args.is_empty() { String::new() } else { format!(" {args}") },
    ))
}

pub fn launch_agent_plist(spec: &SupervisorSpec) -> Result<String> {
    let executable =
        validated_supervisor_artifact_path("daemon executable", spec.launch.program())?;
    let environment_path =
        validated_supervisor_artifact_path("environment handoff path", spec.environment_path())?;
    let args = spec
        .launch
        .args()
        .map(|arg| {
            let arg = arg
                .to_str()
                .ok_or_else(|| anyhow!("supervisor argument is not Unicode"))?;
            validated_supervisor_artifact_text("daemon argument", arg)?;
            Ok(format!("<string>{}</string>", xml_escape(arg)))
        })
        .collect::<Result<Vec<_>>>()?
        .join("");
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{}</string>\n<key>EnvironmentVariables</key><dict><key>{}</key><string>{}</string></dict>\n<key>ProgramArguments</key><array><string>{}</string>{}</array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n<key>ProcessType</key><string>Background</string>\n<key>StandardOutPath</key><string>/dev/null</string>\n<key>StandardErrorPath</key><string>/dev/null</string>\n</dict></plist>\n",
        xml_escape(spec.identity().name()),
        SUPERVISOR_ENVIRONMENT_FILE_ENV,
        xml_escape(environment_path),
        xml_escape(executable),
        args,
    ))
}

fn supervisor_environment_document(launch: &NormalizedLaunch) -> Result<Value> {
    let environment = launch
        .environment()
        .map(|(name, value)| -> Result<Value> {
            let name = name
                .to_str()
                .ok_or_else(|| anyhow!("supervisor environment name is not Unicode"))?;
            validated_supervisor_environment_name(name)?;
            let value = value
                .to_str()
                .ok_or_else(|| anyhow!("supervisor environment value {name} is not Unicode"))?;
            validated_supervisor_artifact_text(&format!("environment variable {name}"), value)?;
            Ok(json!({"name": name, "value": value}))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "schema_version": 1,
        "environment": environment,
    }))
}

pub fn write_supervisor_environment(spec: &SupervisorSpec) -> Result<PathBuf> {
    crate::write_private_json_file(
        spec.environment_path(),
        &supervisor_environment_document(spec.launch())?,
    )?;
    Ok(spec.environment_path().to_path_buf())
}

pub fn verify_supervisor_environment(spec: &SupervisorSpec) -> Result<()> {
    let installed = read_private_supervisor_environment(spec.environment_path())?;
    if installed != supervisor_environment_document(spec.launch())? {
        return Err(anyhow!(
            "supervisor environment does not match the maintained definition"
        ));
    }
    Ok(())
}

pub fn remove_supervisor_environment(data_root: &Path) -> Result<()> {
    let path = supervisor_environment_path(data_root);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("remove supervisor environment {}", path.display()))
        }
    }
}

pub fn apply_supervisor_environment_handoff() -> Result<bool> {
    let Some(path) = env::var_os(SUPERVISOR_ENVIRONMENT_FILE_ENV) else {
        return Ok(false);
    };
    let document = read_private_supervisor_environment(Path::new(&path))?;
    let environment = parse_supervisor_environment_document(&document)?;

    for (name, _) in env::vars_os() {
        env::remove_var(name);
    }
    for (name, value) in environment {
        env::set_var(name, value);
    }
    Ok(true)
}

fn read_private_supervisor_environment(path: &Path) -> Result<Value> {
    let file = crate::private_open_existing_file_nofollow(path)
        .with_context(|| format!("open private supervisor environment {}", path.display()))?;
    verify_private_file_handle(&file)
        .with_context(|| format!("verify private supervisor environment {}", path.display()))?;
    serde_json::from_reader(file)
        .with_context(|| format!("parse private supervisor environment {}", path.display()))
}

fn parse_supervisor_environment_document(document: &Value) -> Result<BTreeMap<OsString, OsString>> {
    if document.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(anyhow!("unsupported supervisor environment schema"));
    }
    let entries = document
        .get("environment")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("supervisor environment entries are missing"))?;
    let mut environment = BTreeMap::new();
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("supervisor environment entry name is missing"))?;
        validated_supervisor_environment_name(name)?;
        let value = entry
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("supervisor environment entry value is missing"))?;
        validated_supervisor_artifact_text(&format!("environment variable {name}"), value)?;
        if environment
            .insert(OsString::from(name), OsString::from(value))
            .is_some()
        {
            return Err(anyhow!(
                "supervisor environment contains duplicate variable {name}"
            ));
        }
    }
    Ok(environment)
}

fn validated_supervisor_environment_name(name: &str) -> Result<&str> {
    validated_supervisor_artifact_text("environment variable name", name)?;
    if name.is_empty() || name.contains('=') {
        return Err(anyhow!("supervisor environment variable name is invalid"));
    }
    Ok(name)
}

fn systemd_quote_text(value: &str) -> String {
    let value = value.replace('%', "%%");
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn validated_supervisor_artifact_path<'a>(
    label: &str,
    path: &'a std::path::Path,
) -> Result<&'a str> {
    let value = path.to_str().ok_or_else(|| {
        anyhow!("supervisor {label} is not Unicode and cannot be persisted safely")
    })?;
    validated_supervisor_artifact_text(label, value)
}

pub fn validated_supervisor_artifact_text<'a>(label: &str, value: &'a str) -> Result<&'a str> {
    if value.chars().any(char::is_control) {
        return Err(anyhow!(
            "supervisor {label} contains control characters and cannot be persisted safely"
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, process::Command};

    use super::*;

    const HANDOFF_STAGE: &str = "CTX_TEST_SUPERVISOR_ENVIRONMENT_HANDOFF_STAGE";
    const HOSTILE_AMBIENT: &str = "CTX_TEST_HOSTILE_AMBIENT";
    const TEST_NAME: &str =
        "supervisor::artifact::tests::private_handoff_replaces_the_ambient_environment_exactly";

    #[test]
    fn private_handoff_replaces_the_ambient_environment_exactly() -> Result<()> {
        if env::var_os(HANDOFF_STAGE).as_deref() == Some(std::ffi::OsStr::new("child")) {
            assert_eq!(env::var(HOSTILE_AMBIENT).as_deref(), Ok("must-be-cleared"));
            assert!(apply_supervisor_environment_handoff()?);
            assert_eq!(env::var("HOME").as_deref(), Ok("/private/home"));
            assert_eq!(
                env::var("CTX_SEMANTIC_EMBEDDING_TOKEN").as_deref(),
                Ok("private-bearer-token")
            );
            assert!(env::var_os(HOSTILE_AMBIENT).is_none());
            assert!(env::var_os(HANDOFF_STAGE).is_none());
            assert!(env::var_os(SUPERVISOR_ENVIRONMENT_FILE_ENV).is_none());
            return Ok(());
        }

        let temp = tempfile::tempdir()?;
        let identity = SupervisorIdentity::new(
            "ctx.service",
            temp.path().join("registration").join("ctx.service"),
        )?;
        let spec = SupervisorSpec::new(
            identity,
            "ctx test daemon",
            supervisor_environment_path(temp.path()),
            NormalizedLaunch::new(
                PathBuf::from("/opt/ctx/bin/ctx"),
                vec![OsString::from("daemon"), OsString::from("run")],
                BTreeMap::from([
                    (OsString::from("HOME"), OsString::from("/private/home")),
                    (
                        OsString::from("CTX_SEMANTIC_EMBEDDING_TOKEN"),
                        OsString::from("private-bearer-token"),
                    ),
                ]),
            ),
        )?;
        let path = write_supervisor_environment(&spec)?;
        verify_supervisor_environment(&spec)?;

        let status = Command::new(env::current_exe()?)
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env_clear()
            .env(HANDOFF_STAGE, "child")
            .env(HOSTILE_AMBIENT, "must-be-cleared")
            .env(SUPERVISOR_ENVIRONMENT_FILE_ENV, path)
            .status()?;
        assert!(status.success(), "handoff child failed: {status}");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn private_handoff_reader_rejects_symlinked_credentials() -> Result<()> {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let temp = tempfile::tempdir()?;
        let target = temp.path().join("credentials.json");
        let link = temp.path().join("supervisor-environment.json");
        fs::write(
            &target,
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "environment": [{
                    "name": "CTX_SEMANTIC_EMBEDDING_TOKEN",
                    "value": "attacker-selected-token",
                }],
            }))?,
        )?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
        symlink(&target, &link)?;

        let error = read_private_supervisor_environment(&link)
            .expect_err("credential handoff must not follow a symlink");

        assert!(
            error
                .to_string()
                .contains("open private supervisor environment"),
            "{error:#}"
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn private_handoff_reader_rejects_reparse_points() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("target");
        let junction = temp.path().join("supervisor-environment.json");
        fs::create_dir(&target)?;
        let status = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()?;
        if !status.success() {
            return Err(anyhow!("failed to create junction fixture"));
        }

        let error = read_private_supervisor_environment(&junction)
            .expect_err("credential handoff must not accept a reparse point");

        assert!(error
            .to_string()
            .contains("verify private supervisor environment"));
        assert!(format!("{error:#}").contains("reparse point"));
        Ok(())
    }
}
