use chrono::{DateTime, Utc};
use ctx_core::ids::{TerminalId, WorkspaceId};
use ctx_core::models::TerminalSession;
use ctx_transport_runtime::terminal_launch::TerminalLaunchError;
use ctx_transport_runtime::terminals::{TerminalManager, TerminalStreamSession};

mod launch;
mod route_contract;

pub(in crate::daemon) use self::launch::{CreateTerminalLaunchRequest, TerminalLaunchHost};

async fn list_workspace_terminals(
    terminals: &TerminalManager,
    workspace_id: WorkspaceId,
) -> Vec<TerminalSession> {
    terminals.list(workspace_id).await
}

pub(in crate::daemon) async fn create_workspace_terminal(
    host: &TerminalLaunchHost,
    req: CreateTerminalLaunchRequest,
) -> Result<TerminalSession, TerminalLaunchError> {
    launch::create_workspace_terminal(host, req).await
}

async fn delete_terminal(terminals: &TerminalManager, terminal_id: TerminalId) -> bool {
    let session = terminals.remove(terminal_id).await;
    if let Some(session) = session {
        let _ = session.kill();
        session.mark_exited(None);
        return true;
    }
    false
}

pub struct TerminalStreamConnectPath {
    pub stream_path: String,
    pub expires_at: DateTime<Utc>,
}

pub struct TerminalStreamRouteAdmission {
    pub session: TerminalStreamSession,
    pub tail_bytes: usize,
}

async fn mint_terminal_stream_token(
    terminals: &TerminalManager,
    terminal_id: TerminalId,
) -> Option<TerminalStreamConnectPath> {
    let handle = terminals.get(terminal_id).await?;
    let (stream_path, expires_at) = handle.issue_stream_connect_path();
    Some(TerminalStreamConnectPath {
        stream_path,
        expires_at,
    })
}
