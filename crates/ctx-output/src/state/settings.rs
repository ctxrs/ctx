// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
use super::*;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SemanticMode {
    #[default]
    Off,
    Shadow,
    Select,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SemanticSelection {
    pub mode: SemanticMode,
    pub allowed_projects: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub record_usage: bool,
    pub keep_originals: bool,
    pub exclude_commands: Vec<String>,
    pub originals_max_entries: usize,
    pub originals_max_bytes: u64,
    pub originals_max_days: u64,
    pub semantic_selection: SemanticSelection,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            record_usage: true,
            keep_originals: false,
            exclude_commands: Vec::new(),
            originals_max_entries: 100,
            originals_max_bytes: ORIGINAL_LIMIT,
            originals_max_days: ORIGINAL_AGE,
            semantic_selection: SemanticSelection::default(),
        }
    }
}
impl Settings {
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path()?)
    }
    /// Missing settings return defaults without creating a directory or file.
    pub fn load_from(path: &Path) -> Result<Self> {
        match File::open(path) {
            Ok(file) => {
                let settings: Self = serde_json::from_reader(file)
                    .context("invalid ctx output config; file left unchanged")?;
                ensure!(
                    settings.exclude_commands.iter().all(|s| valid_label(s)),
                    "exclude_commands must contain exact executable basenames"
                );
                settings.validate()?;
                Ok(settings)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            self.originals_max_entries > 0,
            "originals_max_entries must be positive"
        );
        ensure!(
            self.originals_max_bytes > 0,
            "originals_max_bytes must be positive"
        );
        ensure!(
            self.originals_max_days > 0 && self.originals_max_days <= u64::MAX / 86_400_000,
            "originals_max_days must be positive and fit milliseconds"
        );
        ensure!(
            self.semantic_selection.allowed_projects.len() <= 256
                && self.semantic_selection.allowed_projects.iter().all(|p| {
                    !p.is_empty()
                        && p.len() <= 4096
                        && !p.contains('\0')
                        && Path::new(p).is_absolute()
                }),
            "semantic_selection.allowed_projects contains an invalid project"
        );
        let mut projects = self
            .semantic_selection
            .allowed_projects
            .iter()
            .collect::<Vec<_>>();
        projects.sort();
        projects.dedup();
        ensure!(
            projects.len() == self.semantic_selection.allowed_projects.len(),
            "semantic_selection.allowed_projects contains duplicates"
        );
        Ok(())
    }
    pub fn excludes(&self, executable: &str) -> bool {
        let basename = executable.rsplit(['/', '\\']).next().unwrap_or(executable);
        self.exclude_commands.iter().any(|s| s == basename)
    }
    /// Explicit creation only; never overwrites even corrupt existing settings.
    pub fn create_default(path: &Path) -> Result<()> {
        private_dir(path.parent().context("config path has no parent")?)?;
        let mut file = private_open(path, false, true)?;
        serde_json::to_writer_pretty(&mut file, &Self::default())?;
        file.write_all(b"\n")?;
        Ok(())
    }

    pub(super) fn save_to(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path.parent().context("config path has no parent")?;
        private_dir(parent)?;
        if let Ok(meta) = fs::symlink_metadata(path) {
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "ctx output config must be a regular file"
            );
        }
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        for _ in 0..32 {
            let suffix = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temporary = parent.join(format!(
                ".{}.{}.{}.tmp",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .context("config filename must be UTF-8")?,
                std::process::id(),
                suffix
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = match options.open(&temporary) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            };
            let result = (|| -> Result<()> {
                file.write_all(&bytes)?;
                file.flush()?;
                file.sync_all()?;
                drop(file);
                fs::rename(&temporary, path)?;
                #[cfg(unix)]
                File::open(parent)?.sync_all()?;
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            return result;
        }
        bail!("cannot allocate a temporary ctx output config file")
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}
pub fn config_dir() -> Result<PathBuf> {
    directory(true)
}
pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.json"))
}
pub fn state_dir() -> Result<PathBuf> {
    directory(false)
}
fn directory(config: bool) -> Result<PathBuf> {
    if let Some(path) = env_path(if config {
        "CTX_OUTPUT_CONFIG_DIR"
    } else {
        "CTX_OUTPUT_STATE_DIR"
    })
    .or_else(|| {
        env_path(if config {
            "SIFT_CONFIG_DIR"
        } else {
            "SIFT_STATE_DIR"
        })
    }) {
        return Ok(path);
    }
    #[cfg(windows)]
    {
        Ok(env_path(if config { "APPDATA" } else { "LOCALAPPDATA" })
            .context("Windows application data directory unavailable")?
            .join("ctx")
            .join("output"))
    }
    #[cfg(not(windows))]
    {
        if let Some(path) = env_path(if config {
            "XDG_CONFIG_HOME"
        } else {
            "XDG_STATE_HOME"
        }) {
            return Ok(path.join("ctx").join("output"));
        }
        Ok(env_path("HOME")
            .context(if config {
                "HOME unavailable; set CTX_OUTPUT_CONFIG_DIR"
            } else {
                "HOME unavailable; set CTX_OUTPUT_STATE_DIR"
            })?
            .join(if config {
                ".config/ctx/output"
            } else {
                ".local/state/ctx/output"
            }))
    }
}
