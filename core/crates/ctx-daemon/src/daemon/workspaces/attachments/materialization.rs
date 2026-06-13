use std::sync::atomic::Ordering;
use std::sync::Arc;

use ctx_core::ids::WorkspaceAttachmentId;
use ctx_core::models::Workspace;

use super::runtime::{AttachmentMaterializationTask, WorkspaceAttachmentsRuntime};

pub async fn spawn_attachment_materialization(
    runtime: Arc<WorkspaceAttachmentsRuntime>,
    workspace: Workspace,
    attachment_id: WorkspaceAttachmentId,
    refresh: bool,
) {
    cancel_attachment_materialization(runtime.as_ref(), attachment_id).await;
    let generation = runtime
        .materialization()
        .generation
        .fetch_add(1, Ordering::SeqCst)
        + 1;
    let task_runtime = Arc::clone(&runtime);
    let handle = tokio::spawn(async move {
        run_attachment_materialization(
            Arc::clone(&task_runtime),
            workspace,
            attachment_id,
            refresh,
        )
        .await;
        clear_attachment_materialization_task(task_runtime.as_ref(), attachment_id, generation)
            .await;
    });
    let mut tasks = runtime.materialization().tasks.lock().await;
    tasks.insert(
        attachment_id,
        AttachmentMaterializationTask { generation, handle },
    );
}

pub async fn cancel_attachment_materialization(
    runtime: &WorkspaceAttachmentsRuntime,
    attachment_id: WorkspaceAttachmentId,
) {
    let existing = {
        let mut tasks = runtime.materialization().tasks.lock().await;
        tasks.remove(&attachment_id)
    };
    if let Some(task) = existing {
        task.handle.abort();
        let _ = task.handle.await;
    }
}

async fn clear_attachment_materialization_task(
    runtime: &WorkspaceAttachmentsRuntime,
    attachment_id: WorkspaceAttachmentId,
    generation: u64,
) {
    let mut tasks = runtime.materialization().tasks.lock().await;
    if tasks
        .get(&attachment_id)
        .is_some_and(|task| task.generation == generation)
    {
        tasks.remove(&attachment_id);
    }
}

async fn run_attachment_materialization(
    runtime: Arc<WorkspaceAttachmentsRuntime>,
    workspace: Workspace,
    attachment_id: WorkspaceAttachmentId,
    refresh: bool,
) {
    if let Err(err) = ctx_workspace_attachments::run_attachment_materialization(
        runtime.as_ref(),
        &workspace,
        attachment_id,
        refresh,
    )
    .await
    {
        tracing::warn!("attachment sync failed: {err:#}");
    }
}
