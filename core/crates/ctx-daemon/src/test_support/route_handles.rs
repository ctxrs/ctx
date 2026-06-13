use crate::daemon::route_handles_from_state;

use super::TestDaemon;

impl TestDaemon {
    pub fn provider_accounts_handle_for_test(&self) -> crate::daemon::ProviderAccountsHandle {
        route_handles_from_state(&self.state).provider_accounts
    }

    pub fn blob_handle_for_test(&self) -> crate::daemon::BlobHandle {
        route_handles_from_state(&self.state).blob
    }

    pub fn repo_onboarding_handle_for_test(&self) -> crate::daemon::RepoOnboardingHandle {
        route_handles_from_state(&self.state).repo_onboarding
    }

    pub fn task_session_listing_handle_for_test(&self) -> crate::daemon::TaskSessionListingHandle {
        route_handles_from_state(&self.state).task_session_listing
    }

    pub fn task_session_admission_handle_for_test(
        &self,
    ) -> crate::daemon::TaskSessionAdmissionHandle {
        route_handles_from_state(&self.state).task_session_admission
    }

    pub fn task_read_state_handle_for_test(&self) -> crate::daemon::TaskReadStateHandle {
        route_handles_from_state(&self.state).task_read_state
    }

    pub fn task_title_handle_for_test(&self) -> crate::daemon::TaskTitleHandle {
        route_handles_from_state(&self.state).task_title
    }

    pub fn task_lifecycle_handle_for_test(&self) -> crate::daemon::TaskLifecycleHandle {
        route_handles_from_state(&self.state).task_lifecycle
    }

    pub fn session_title_model_mode_handle_for_test(
        &self,
    ) -> crate::daemon::SessionTitleModelModeHandle {
        route_handles_from_state(&self.state).session_title_model_mode
    }

    pub fn session_artifacts_handle_for_test(&self) -> crate::daemon::SessionArtifactsHandle {
        route_handles_from_state(&self.state).session_artifacts
    }

    pub fn session_control_handle_for_test(&self) -> crate::daemon::SessionControlHandle {
        route_handles_from_state(&self.state).session_control
    }

    pub fn session_file_completions_handle_for_test(
        &self,
    ) -> crate::daemon::SessionFileCompletionsHandle {
        route_handles_from_state(&self.state).session_file_completions
    }

    pub fn session_message_command_handle_for_test(
        &self,
    ) -> crate::daemon::SessionMessageCommandHandle {
        route_handles_from_state(&self.state).session_message_command
    }

    pub fn session_read_models_handle_for_test(&self) -> crate::daemon::SessionReadModelsHandle {
        route_handles_from_state(&self.state).session_read_models
    }

    pub fn session_vcs_handle_for_test(&self) -> crate::daemon::SessionVcsHandle {
        route_handles_from_state(&self.state).session_vcs
    }

    pub fn session_subagent_read_handle_for_test(
        &self,
    ) -> crate::daemon::SessionSubagentReadHandle {
        route_handles_from_state(&self.state).session_subagent_read
    }

    pub fn session_subagent_mcp_read_handle_for_test(
        &self,
    ) -> crate::daemon::SessionSubagentMcpReadHandle {
        route_handles_from_state(&self.state).session_subagent_mcp_read
    }

    pub fn session_subagent_mcp_control_handle_for_test(
        &self,
    ) -> crate::daemon::SessionSubagentMcpControlHandle {
        route_handles_from_state(&self.state).session_subagent_mcp_control
    }

    pub fn workspace_stream_handle_for_test(&self) -> crate::daemon::WorkspaceStreamHandle {
        route_handles_from_state(&self.state).workspace_stream
    }

    pub fn workspace_vcs_stream_handle_for_test(&self) -> crate::daemon::WorkspaceVcsStreamHandle {
        route_handles_from_state(&self.state).workspace_vcs_stream
    }

    pub fn workspace_org_policy_handle_for_test(&self) -> crate::daemon::WorkspaceOrgPolicyHandle {
        route_handles_from_state(&self.state).workspace_org_policy
    }

    pub fn workspace_prompt_bootstrap_config_handle_for_test(
        &self,
    ) -> crate::daemon::WorkspacePromptBootstrapConfigHandle {
        route_handles_from_state(&self.state).workspace_prompt_bootstrap_config
    }

    pub fn workspace_execution_config_handle_for_test(
        &self,
    ) -> crate::daemon::WorkspaceExecutionConfigHandle {
        route_handles_from_state(&self.state).workspace_execution_config
    }

    pub fn workspace_harness_container_handle_for_test(
        &self,
    ) -> crate::daemon::WorkspaceHarnessContainerHandle {
        route_handles_from_state(&self.state).workspace_harness_container
    }

    pub fn workspace_provider_model_preferences_handle_for_test(
        &self,
    ) -> crate::daemon::WorkspaceProviderModelPreferenceHandle {
        route_handles_from_state(&self.state).workspace_provider_model_preferences
    }

    pub fn workspace_worktree_handle_for_test(&self) -> crate::daemon::WorkspaceWorktreeHandle {
        route_handles_from_state(&self.state).workspace_worktree
    }

    pub fn workspace_registry_handle_for_test(&self) -> crate::daemon::WorkspaceRegistryHandle {
        route_handles_from_state(&self.state).workspace_registry
    }

    pub fn workspace_merge_queue_config_handle_for_test(
        &self,
    ) -> crate::daemon::WorkspaceMergeQueueConfigHandle {
        route_handles_from_state(&self.state).workspace_merge_queue_config
    }

    pub fn workspace_attachments_handle_for_test(
        &self,
    ) -> crate::daemon::WorkspaceAttachmentsHandle {
        route_handles_from_state(&self.state).workspace_attachments
    }

    pub fn workspace_primary_branch_handle_for_test(
        &self,
    ) -> crate::daemon::WorkspacePrimaryBranchHandle {
        route_handles_from_state(&self.state).workspace_primary_branch
    }

    pub fn workspace_deletion_handle_for_test(&self) -> crate::daemon::WorkspaceDeletionHandle {
        route_handles_from_state(&self.state).workspace_deletion
    }

    pub fn provider_harness_config_handle_for_test(
        &self,
    ) -> crate::daemon::ProviderHarnessConfigHandle {
        route_handles_from_state(&self.state).provider_harness_config
    }

    pub fn provider_status_handle_for_test(&self) -> crate::daemon::ProviderStatusHandle {
        route_handles_from_state(&self.state).provider_status
    }

    pub fn provider_install_handle_for_test(&self) -> crate::daemon::ProviderInstallHandle {
        route_handles_from_state(&self.state).provider_install
    }

    pub fn settings_handle_for_test(&self) -> crate::daemon::SettingsHandle {
        route_handles_from_state(&self.state).settings
    }

    pub fn update_drain_handle_for_test(&self) -> crate::daemon::UpdateDrainHandle {
        route_handles_from_state(&self.state).update_drain
    }

    pub fn org_policy_handle_for_test(&self) -> crate::daemon::OrgPolicyHandle {
        route_handles_from_state(&self.state).org_policy
    }

    pub fn telemetry_handle_for_test(&self) -> crate::daemon::TelemetryHandle {
        route_handles_from_state(&self.state).telemetry
    }

    pub fn run_archive_handle_for_test(&self) -> crate::daemon::RunArchiveHandle {
        route_handles_from_state(&self.state).run_archive
    }

    pub fn resource_utilization_handle_for_test(&self) -> crate::daemon::ResourceUtilizationHandle {
        route_handles_from_state(&self.state).resource_utilization
    }

    #[cfg(test)]
    pub(crate) fn workspace_primary_branch_with_refresh_effect_for_test(
        &self,
        refresh_vcs_snapshot: crate::daemon::WorkspacePrimaryBranchRefreshEffect,
    ) -> crate::daemon::WorkspacePrimaryBranchHandle {
        crate::daemon::workspace_primary_branch_with_refresh_effect_from_state(
            &self.state,
            refresh_vcs_snapshot,
        )
    }
}
