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
    WorkspaceRouteResponse, WorkspaceWorkArtifactRenderKind, WorkspaceWorkArtifactRouteItem,
    WorkspaceWorkArtifactSummaryRouteResponse, WorkspaceWorkChangeSummaryRouteResponse,
    WorkspaceWorkCommandPreviewRouteResponse, WorkspaceWorkContextRouteQuery,
    WorkspaceWorkContextRouteResponse, WorkspaceWorkDetailRouteResponse,
    WorkspaceWorkDuplicateStrongLinkRouteItem, WorkspaceWorkEventRouteItem,
    WorkspaceWorkEvidenceCreateRouteRequest, WorkspaceWorkEvidenceCreateRouteResponse,
    WorkspaceWorkEvidenceRouteItem, WorkspaceWorkEvidenceRouteResponse,
    WorkspaceWorkEvidenceSummaryRouteResponse, WorkspaceWorkInspectorOverviewRouteResponse,
    WorkspaceWorkInspectorRouteResponse, WorkspaceWorkLinkRouteItem, WorkspaceWorkListRouteQuery,
    WorkspaceWorkListRouteResponse, WorkspaceWorkRecordRouteItem, WorkspaceWorkReportRouteResponse,
    WorkspaceWorkSafeJsonRouteResponse, WorkspaceWorkSubagentRouteItem,
    WorkspaceWorkSummaryClaimCreateRouteRequest, WorkspaceWorkSummaryClaimRouteItem,
    WorkspaceWorkSummaryCreateRouteRequest, WorkspaceWorkSummaryCreateRouteResponse,
    WorkspaceWorkSummaryRouteItem, WorkspaceWorkTimelineItemRouteResponse,
    WorkspaceWorkTimelineRouteQuery, WorkspaceWorkTimelineRouteResponse,
    WorkspaceWorkTranscriptItemRouteResponse, WorkspaceWorkTrustRouteSummary,
    WorktreeRouteResponse,
};
pub use stream::{
    WorkspaceStreamRouteError, WorkspaceStreamRouteErrorKind, WorkspaceStreamRouteParams,
};
pub use worktrees::WorkspaceFileCompletionsRouteQuery;

#[cfg(test)]
mod tests;
