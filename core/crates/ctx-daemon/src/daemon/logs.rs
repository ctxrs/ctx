use ctx_observability::logs;

use crate::daemon::LogsHandle;

impl LogsHandle {
    pub async fn open_logs_folder(&self) -> anyhow::Result<()> {
        logs::open_logs_folder(self.data_root()).await
    }

    pub async fn append_desktop_log(
        &self,
        level: Option<String>,
        message: String,
    ) -> anyhow::Result<()> {
        let level = level.unwrap_or_else(|| "info".to_string());
        let line = format!(
            "{} [{level}] {message}",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        );
        logs::append_desktop_log_line(self.data_root(), &line).await
    }
}
