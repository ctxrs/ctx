use std::path::Path;

use anyhow::Result;
use ctx_history_refresh::{DurableAdmissionPersistence, RefreshJournal};
use serde_json::Value;

use crate::semantic::paths_status::{
    daemon_source_backed_refresh_job_path, read_daemon_job_status, sync_private_file_parent,
    write_daemon_job_status,
};

#[derive(Debug, Default)]
pub(in crate::semantic) struct DaemonRefreshJournal;

impl RefreshJournal for DaemonRefreshJournal {
    fn load(&self, data_root: &Path) -> Result<Option<Value>> {
        Ok(read_daemon_job_status(
            &daemon_source_backed_refresh_job_path(data_root),
        ))
    }

    fn store(&self, data_root: &Path, value: &Value) -> Result<()> {
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), value)
    }

    fn store_before_ack(&self, data_root: &Path, value: &Value) -> DurableAdmissionPersistence {
        let path = daemon_source_backed_refresh_job_path(data_root);
        if let Err(error) = write_daemon_job_status(&path, value) {
            return if error
                .downcast_ref::<crate::semantic::paths_status::PrivateJsonReplacementError>()
                .is_some()
                || read_daemon_job_status(&path).as_ref() == Some(value)
            {
                DurableAdmissionPersistence::Retained(error)
            } else {
                DurableAdmissionPersistence::Failed(error)
            };
        }
        match sync_private_file_parent(&path) {
            Ok(()) => DurableAdmissionPersistence::Confirmed,
            Err(error) => DurableAdmissionPersistence::Retained(error),
        }
    }
}
