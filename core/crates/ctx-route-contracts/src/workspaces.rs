mod attachments;
mod common;
mod config;
mod management;
mod responses;
mod stream;
mod worktrees;

pub use attachments::{
    CreateWorkspaceAttachmentRouteRequest, DeleteWorkspaceAttachmentRouteRequest,
    SyncWorkspaceAttachmentsRouteRequest, WorkspaceAttachmentCreateRouteSpec,
    WorkspaceAttachmentDeleteRouteSpec,
};
pub use common::{
    WorkspaceRouteError, WorkspaceRouteErrorKind, WorkspaceRouteParams, WorktreeRouteParams,
};
pub use config::{
    AgentSystemPromptConfigRouteResponse, SubagentSystemPromptConfigRouteResponse,
    UpdateAgentSystemPromptConfigRouteRequest, UpdateSubagentSystemPromptConfigRouteRequest,
    UpdateWorkspaceExecutionConfigRequest, UpdateWorkspaceMergeQueueConfigRequest,
    UpdateWorkspaceProviderModelPreferenceRouteRequest, UpdateWorktreeBootstrapConfigRequest,
    WorkspaceExecutionConfigRouteSnapshot, WorkspaceMergeQueueConfigRouteResponse,
    WorkspacePromptConfigRouteParams, WorkspaceProviderModelPreferenceRouteParams,
    WorkspaceProviderModelPreferenceRouteResponse, WorkspaceWorktreeBootstrapConfigRouteResponse,
};
pub use management::{
    CreateWorkspaceRequest, UpdateWorkspacePrimaryBranchRequest, WorkspaceConfigUpdateResult,
    WorkspacePrimaryBranchSnapshot,
};
pub use responses::{
    WorkspaceActiveHeadBatchRouteResponse, WorkspaceActiveSnapshotRouteResponse,
    WorkspaceAgentWorkRouteQuery, WorkspaceAgentWorkRouteResponse,
    WorkspaceAttachmentRouteResponse, WorkspaceHarnessContainerMountModeRouteValue,
    WorkspaceHarnessContainerNetworkModeRouteValue, WorkspaceHarnessContainerStatusRouteResponse,
    WorkspaceRouteResponse, WorktreeRouteResponse,
};
pub use stream::{
    WorkspaceStreamRouteError, WorkspaceStreamRouteErrorKind, WorkspaceStreamRouteParams,
};
pub use worktrees::WorkspaceFileCompletionsRouteQuery;

#[cfg(test)]
mod tests;
