use anyhow::Context;
use ctx_desktop_ipc::{
    DesktopWebviewRecoveryAction, DesktopWebviewRecoveryIncident,
    DesktopWebviewRecoveryTriggerKind, DesktopWebviewSurface,
};
use sqlx::{Row, SqlitePool};
use tauri::Manager;

use super::super::desktop_storage::DesktopStorage;
use super::policy::now_ms;

const WEBVIEW_RECOVERY_INCIDENTS_CREATE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS webview_recovery_incidents (\
        incident_id TEXT PRIMARY KEY,\
        window_label TEXT NOT NULL,\
        window_surface TEXT NOT NULL,\
        trigger_kind TEXT NOT NULL,\
        action TEXT NOT NULL,\
        created_at_ms INTEGER NOT NULL,\
        consumed_at_ms INTEGER,\
        payload TEXT NOT NULL\
    )";

fn surface_str(surface: DesktopWebviewSurface) -> &'static str {
    match surface {
        DesktopWebviewSurface::Main => "main",
        DesktopWebviewSurface::Workbench => "workbench",
        DesktopWebviewSurface::Launcher => "launcher",
        DesktopWebviewSurface::Settings => "settings",
        DesktopWebviewSurface::FilePreview => "file_preview",
        DesktopWebviewSurface::WorkspaceSetup => "workspace_setup",
        DesktopWebviewSurface::Unknown => "unknown",
    }
}

fn trigger_str(trigger: DesktopWebviewRecoveryTriggerKind) -> &'static str {
    match trigger {
        DesktopWebviewRecoveryTriggerKind::NativeProcessTermination => "native_process_termination",
        DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout => "heartbeat_timeout",
    }
}

fn action_str(action: DesktopWebviewRecoveryAction) -> &'static str {
    match action {
        DesktopWebviewRecoveryAction::Noop => "noop",
        DesktopWebviewRecoveryAction::Reload => "reload",
        DesktopWebviewRecoveryAction::Recreate => "recreate",
        DesktopWebviewRecoveryAction::PromptRestart => "prompt_restart",
    }
}

async fn ensure_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(WEBVIEW_RECOVERY_INCIDENTS_CREATE_SQL)
        .execute(pool)
        .await
        .context("creating webview_recovery_incidents table")?;
    Ok(())
}

pub(super) async fn record_incident(
    app: &tauri::AppHandle,
    incident: &DesktopWebviewRecoveryIncident,
) -> anyhow::Result<()> {
    let storage = app.state::<DesktopStorage>();
    let pool = storage.pool(app).await?;
    record_incident_in_pool(pool, incident).await
}

async fn record_incident_in_pool(
    pool: &SqlitePool,
    incident: &DesktopWebviewRecoveryIncident,
) -> anyhow::Result<()> {
    ensure_schema(pool).await?;
    let payload =
        serde_json::to_string(incident).context("serializing webview recovery incident")?;
    sqlx::query(
        "INSERT INTO webview_recovery_incidents (\
            incident_id, window_label, window_surface, trigger_kind, action, created_at_ms, consumed_at_ms, payload\
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
    )
    .bind(&incident.incident_id)
    .bind(&incident.window_label)
    .bind(surface_str(incident.window_surface))
    .bind(trigger_str(incident.trigger_kind))
    .bind(action_str(incident.action))
    .bind(incident.created_at_ms as i64)
    .bind(payload)
    .execute(pool)
    .await
    .context("writing webview recovery incident")?;
    Ok(())
}

pub(super) async fn consume_incidents(
    app: &tauri::AppHandle,
) -> anyhow::Result<Vec<DesktopWebviewRecoveryIncident>> {
    let storage = app.state::<DesktopStorage>();
    let pool = storage.pool(app).await?;
    consume_incidents_from_pool(pool).await
}

async fn consume_incidents_from_pool(
    pool: &SqlitePool,
) -> anyhow::Result<Vec<DesktopWebviewRecoveryIncident>> {
    ensure_schema(pool).await?;

    let mut tx = pool
        .begin()
        .await
        .context("starting webview recovery consume transaction")?;
    let rows = sqlx::query(
        "SELECT incident_id, payload FROM webview_recovery_incidents \
         WHERE consumed_at_ms IS NULL ORDER BY created_at_ms ASC",
    )
    .fetch_all(&mut *tx)
    .await
    .context("reading webview recovery incidents")?;

    if rows.is_empty() {
        tx.commit()
            .await
            .context("committing empty webview recovery consume transaction")?;
        return Ok(Vec::new());
    }

    let consumed_at_ms = now_ms() as i64;
    let mut incidents = Vec::with_capacity(rows.len());
    for row in rows {
        let incident_id: String = row.try_get("incident_id").context("reading incident id")?;
        let payload: String = row.try_get("payload").context("reading incident payload")?;
        sqlx::query(
            "UPDATE webview_recovery_incidents SET consumed_at_ms = ?2 WHERE incident_id = ?1",
        )
        .bind(&incident_id)
        .bind(consumed_at_ms)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("marking incident {incident_id} consumed"))?;
        match serde_json::from_str::<DesktopWebviewRecoveryIncident>(&payload) {
            Ok(incident) => incidents.push(incident),
            Err(err) => {
                eprintln!("dropping invalid webview recovery incident payload: {err}");
            }
        }
    }

    tx.commit()
        .await
        .context("committing webview recovery consume transaction")?;
    Ok(incidents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_desktop_ipc::{
        DesktopWebviewRecoveryDaemonHealth, DesktopWebviewRecoverySuppressionReason,
    };
    use sqlx::sqlite::SqlitePoolOptions;

    async fn open_memory_pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("opening sqlite memory pool")
    }

    fn test_incident(id: &str) -> DesktopWebviewRecoveryIncident {
        DesktopWebviewRecoveryIncident {
            incident_id: id.to_string(),
            window_label: "main".to_string(),
            window_surface: DesktopWebviewSurface::Main,
            route: "/workspaces/ws-1".to_string(),
            trigger_kind: DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout,
            action: DesktopWebviewRecoveryAction::Noop,
            daemon_health: DesktopWebviewRecoveryDaemonHealth::Down,
            suppression_reason: Some(DesktopWebviewRecoverySuppressionReason::DaemonDown),
            created_at_ms: 42,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consume_incidents_returns_queue_in_order() {
        let pool = open_memory_pool().await;
        record_incident_in_pool(&pool, &test_incident("inc-1"))
            .await
            .unwrap();
        record_incident_in_pool(&pool, &test_incident("inc-2"))
            .await
            .unwrap();

        let first = consume_incidents_from_pool(&pool).await.unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].incident_id, "inc-1");
        assert_eq!(first[1].incident_id, "inc-2");

        let second = consume_incidents_from_pool(&pool).await.unwrap();
        assert!(second.is_empty());
    }
}
