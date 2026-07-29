use std::{fs, path::Path};

use anyhow::{Context, Result};

use super::super::{
    health_search::{create_private_dir_all, secure_private_file_permissions},
    paths_status::{daemon_root_path, open_or_create_pid_lock_file},
};

const SUPERVISOR_INSTALLATION_LOCK_FILE: &str = "supervisor-installation.lock";

pub(super) struct SupervisorInstallationLock {
    file: fs::File,
}

impl SupervisorInstallationLock {
    pub(super) fn acquire(data_root: &Path) -> Result<Self> {
        let root = daemon_root_path(data_root);
        create_private_dir_all(&root)?;
        let path = root.join(SUPERVISOR_INSTALLATION_LOCK_FILE);
        let (file, _) = open_or_create_pid_lock_file(&path)
            .with_context(|| format!("open ctx supervisor installation lock {}", path.display()))?;
        secure_private_file_permissions(&path)?;
        fs2::FileExt::lock_exclusive(&file).with_context(|| {
            format!(
                "acquire ctx supervisor installation lock {}",
                path.display()
            )
        })?;
        Ok(Self { file })
    }
}

impl Drop for SupervisorInstallationLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}
