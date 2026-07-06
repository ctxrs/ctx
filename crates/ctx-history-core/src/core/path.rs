#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VcsWorkspace {
    pub id: Uuid,
    pub kind: VcsKind,
    pub root_path: String,
    pub repo_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_remote_url_normalized: Option<String>,
    #[serde(default)]
    pub host: VcsHost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monorepo_subpath: Option<String>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

pub fn default_data_root() -> Result<PathBuf> {
    if let Some(value) = env::var_os("CTX_DATA_ROOT") {
        return Ok(PathBuf::from(value));
    }

    let base = BaseDirs::new().ok_or(CoreError::MissingHome)?;
    Ok(base.home_dir().join(".ctx"))
}

pub fn history_dir(root: PathBuf) -> PathBuf {
    root
}

pub fn object_dir(root: PathBuf) -> PathBuf {
    history_dir(root).join("objects")
}

pub fn blob_dir(root: PathBuf) -> PathBuf {
    object_dir(root)
}

pub fn config_path(root: PathBuf) -> PathBuf {
    history_dir(root).join("config.toml")
}

pub fn logs_dir(root: PathBuf) -> PathBuf {
    history_dir(root).join("logs")
}

pub fn device_path(root: PathBuf) -> PathBuf {
    history_dir(root).join("device.json")
}

pub(crate) fn redact_local_paths(text: &str) -> String {
    let mut value = text.to_owned();
    if let Some(regex) = private_path_prefix_regex() {
        value = regex.replace_all(&value, "$1[REDACTED_PATH]").into_owned();
    }
    for regex in local_path_regexes() {
        value = regex.replace_all(&value, "$1[REDACTED_PATH]").into_owned();
    }
    value
}

pub(crate) fn private_path_prefix_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(
                r#"(?i)(^|[\s"'(=\[])(/(?:home|Users)/[^\s/,;"'<>)\]]+/(?:src|code|work|repo|repos)/[^\s/,;"'<>)\]]*secret[^\s/,;"'<>)\]]*)"#,
            )
            .ok()
        })
        .as_ref()
}

pub(crate) fn local_path_regexes() -> &'static [Regex] {
    static REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();
    REGEXES
        .get_or_init(|| {
            [
                r#"(^|[\s"'(=\[])(/(?:home|Users|tmp|var/tmp|private/tmp|Volumes|mnt|workspace|workspaces|repo|repos|code)(?:/[^\s,;"'<>)\]]*)?)"#,
                r#"(^|[\s"'(=\[])(/(?:[A-Za-z0-9._-]+/)+[^\s,;"'<>)\]]*)"#,
                r#"(?i)(^|[\s"'(=\[])(?:[A-Z]:\\|\\\\)[^\s,;"'<>)\]]+"#,
            ]
            .into_iter()
            .filter_map(|pattern| Regex::new(pattern).ok())
            .collect()
        })
        .as_slice()
}
