use super::*;

pub(super) fn daemon_current_source_progress(
    progress: CaptureSourceBackedCurrentSourceProgress,
) -> SourceBackedCurrentSourceProgress {
    SourceBackedCurrentSourceProgress {
        stage: match progress.stage {
            CaptureSourceBackedCurrentSourceProgressStage::SourceFamilyCopy => {
                SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
            }
            CaptureSourceBackedCurrentSourceProgressStage::OnlineBackup => {
                SourceBackedCurrentSourceProgressStage::OnlineBackup
            }
            CaptureSourceBackedCurrentSourceProgressStage::LogicalFingerprint => {
                SourceBackedCurrentSourceProgressStage::LogicalFingerprint
            }
            CaptureSourceBackedCurrentSourceProgressStage::LogicalScan => {
                SourceBackedCurrentSourceProgressStage::LogicalScan
            }
        },
        snapshot_pages_completed: progress.snapshot_pages_completed,
        snapshot_pages_total: progress.snapshot_pages_total,
        snapshot_bytes_completed: progress.snapshot_bytes_completed,
        snapshot_bytes_total: progress.snapshot_bytes_total,
        logical_rows_scanned: progress.logical_rows_scanned,
        logical_certified_bytes: progress.logical_certified_bytes,
    }
}

pub(super) fn record_source_backed_refresh_progress(
    data_root: &Path,
    coordinator: &CoreRefreshEngine,
    request_id: &str,
    update: SourceBackedRefreshProgressUpdate,
) -> Result<()> {
    coordinator.persist_progress(data_root, request_id, update)
}
