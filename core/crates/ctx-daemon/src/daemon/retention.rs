use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use ctx_store::StoreManager;

const DEFAULT_TOOL_SUMMARY_RETENTION_DAYS: u64 = 30;

fn tool_summary_retention_days() -> u64 {
    std::env::var("CTX_TOOL_SUMMARY_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_TOOL_SUMMARY_RETENTION_DAYS)
}

pub async fn prune_archived_session_data_for_all_workspaces(
    stores: &StoreManager,
    retention_days: u64,
) -> Result<()> {
    let workspaces = stores.global().list_workspaces().await?;
    for workspace in workspaces {
        match stores.workspace_transient(workspace.id).await {
            Ok(store) => {
                let prune_result = store
                    .prune_session_data_older_than_days(retention_days)
                    .await;
                store.close().await;
                match prune_result {
                    Ok(stats) => {
                        tracing::info!(
                            workspace_id = %workspace.id.0,
                            tool_summaries_deleted = stats.tool_summaries_deleted,
                            turn_thoughts_cleared = stats.turn_thoughts_cleared,
                            retention_days,
                            "pruned archived session data",
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            workspace_id = %workspace.id.0,
                            retention_days,
                            "failed to prune old session data: {err:#}",
                        );
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    workspace_id = %workspace.id.0,
                    "failed to open workspace store for pruning: {err:#}",
                );
            }
        }
    }
    Ok(())
}

pub(super) fn spawn_archived_session_data_pruner(stores: StoreManager) {
    tokio::spawn(async move {
        let mut last_cleanup = None::<String>;
        loop {
            let today = Utc::now().format("%Y-%m-%d").to_string();
            if last_cleanup.as_deref() != Some(&today) {
                let retention_days = tool_summary_retention_days();
                if let Err(err) =
                    prune_archived_session_data_for_all_workspaces(&stores, retention_days).await
                {
                    tracing::warn!(
                        retention_days,
                        "failed to list workspaces for pruning: {err:#}",
                    );
                }
                last_cleanup = Some(today);
            }
            tokio::time::sleep(Duration::from_secs(60 * 60)).await;
        }
    });
}
