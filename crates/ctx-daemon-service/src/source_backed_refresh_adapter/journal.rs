use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::Result;
use ctx_history_refresh::{DurableAdmissionPersistence, RefreshJournal};
use serde_json::Value;

use crate::paths_status::{
    create_private_dir_all_before_ack, daemon_jobs_path, daemon_root_path,
    daemon_source_backed_refresh_job_path, read_daemon_job_status, read_daemon_job_status_strict,
    sync_private_file_parent, write_daemon_job_status,
};

#[derive(Debug, Default)]
pub(crate) struct DaemonRefreshJournal {
    initialized_root: Mutex<Option<PathBuf>>,
}

impl RefreshJournal for DaemonRefreshJournal {
    fn load(&self, data_root: &Path) -> Result<Option<Value>> {
        read_daemon_job_status_strict(&daemon_source_backed_refresh_job_path(data_root))
    }

    fn store(&self, data_root: &Path, value: &Value) -> Result<()> {
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), value)
    }

    fn store_before_ack(&self, data_root: &Path, value: &Value) -> DurableAdmissionPersistence {
        self.store_with_parent_sync(data_root, value, sync_private_file_parent)
    }
}

impl DaemonRefreshJournal {
    fn initialize(&self, data_root: &Path) -> Result<()> {
        let data_root = std::path::absolute(data_root)?;
        let mut initialized = self
            .initialized_root
            .lock()
            .map_err(|_| anyhow::anyhow!("refresh journal initialization lock poisoned"))?;
        if initialized.as_ref() == Some(&data_root) {
            return Ok(());
        }
        // The platform directory owner confirms cold-created ancestor links
        // before descent. Confirm our three existing boundaries too, including
        // retries after another creator or a failed initialization. Remember
        // success only after all of them complete; no per-progress flush.
        for directory in [
            &data_root,
            &daemon_root_path(&data_root),
            &daemon_jobs_path(&data_root),
        ] {
            create_private_dir_all_before_ack(directory)?;
        }
        *initialized = Some(data_root);
        Ok(())
    }

    fn store_with_parent_sync(
        &self,
        data_root: &Path,
        value: &Value,
        sync_parent: impl FnOnce(&Path) -> Result<()>,
    ) -> DurableAdmissionPersistence {
        let path = daemon_source_backed_refresh_job_path(data_root);
        if let Err(error) = self
            .initialize(data_root)
            .and_then(|()| write_daemon_job_status(&path, value))
        {
            return if error
                .downcast_ref::<crate::paths_status::PrivateJsonReplacementError>()
                .is_some()
                || read_daemon_job_status(&path).as_ref() == Some(value)
            {
                DurableAdmissionPersistence::Retained(error)
            } else {
                DurableAdmissionPersistence::Failed(error)
            };
        }
        match sync_parent(&path) {
            Ok(()) => DurableAdmissionPersistence::Confirmed,
            Err(error) => DurableAdmissionPersistence::Retained(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn authoritative_load_distinguishes_absence_from_decode_and_read_errors() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = daemon_source_backed_refresh_job_path(temp.path());
        let journal = DaemonRefreshJournal::default();
        assert_eq!(journal.load(temp.path())?, None);

        fs::create_dir_all(path.parent().unwrap())?;
        for bytes in [b"{\"request_id\":".as_slice(), &[0xff]] {
            fs::write(&path, bytes)?;
            let error = journal
                .load(temp.path())
                .expect_err("invalid journal must fail");
            assert!(
                error.downcast_ref::<serde_json::Error>().is_some(),
                "{error:#}"
            );
            assert_eq!(fs::read(&path)?, bytes);
            assert_eq!(read_daemon_job_status(&path), None);
        }

        fs::remove_file(&path)?;
        fs::create_dir(&path)?;
        let error = journal
            .load(temp.path())
            .expect_err("unreadable journal must fail");
        assert!(
            error.downcast_ref::<std::io::Error>().is_some(),
            "{error:#}"
        );
        assert!(path.is_dir());
        Ok(())
    }

    #[test]
    fn authoritative_load_preserves_valid_json_without_new_schema_policy() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let journal = DaemonRefreshJournal::default();
        // The engine, not this persistence adapter, owns parsed-state policy.
        let value = serde_json::json!({"request_state": "unknown", "retained": null});
        journal.store(temp.path(), &value)?;
        assert_eq!(journal.load(temp.path())?, Some(value));
        Ok(())
    }
}

#[cfg(test)]
mod terminal_durability_tests;
