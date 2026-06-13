import { GeneralSettingsSection } from "./sections/GeneralSettingsSection";
import { NotificationsSettingsSection } from "./sections/NotificationsSettingsSection";
import { AnalyticsSettingsSection } from "./sections/AnalyticsSettingsSection";
import { WorktreeBootstrapSection } from "./sections/WorktreeBootstrapSection";
import { AgentSystemPromptSection } from "./sections/AgentSystemPromptSection";
import { WorkspaceAttachmentsSection } from "./sections/WorkspaceAttachmentsSection";
import { ContainerNetworkSection } from "./sections/ContainerNetworkSection";
import { MergeQueueSection } from "./sections/MergeQueueSection";
import { ResourceGovernanceSection } from "./sections/ResourceGovernanceSection";
import { ResourceUtilizationSection } from "./sections/ResourceUtilizationSection";
import { DictationSection } from "./sections/DictationSection";
import { TitleGenerationSection } from "./sections/TitleGenerationSection";
import { HarnessAuthenticationSection } from "./sections/HarnessAuthenticationSection";
import { CodexAccountsSection } from "./sections/CodexAccountsSection";
import { DevToolsSection } from "./sections/DevToolsSection";
import type { SettingsAccountController } from "./hooks/useSettingsAccountController";
import type { SettingsDaemonDocumentController } from "./hooks/useSettingsDaemonDocumentController";
import type { SettingsDevToolsController } from "./hooks/useSettingsDevToolsController";
import type {
  SettingsClientTelemetryController,
  SettingsGeneralPreferencesController,
  SettingsNotificationPreferencesController,
} from "./hooks/useSettingsLocalPreferencesController";
import type { SettingsResourceUtilizationController } from "./hooks/useSettingsResourceUtilizationController";
import type { SectionId } from "./SettingsPage.types";

export function SettingsContentRouter(props: {
  active: SectionId;
  workspaceId: string | null;
  general: SettingsGeneralPreferencesController;
  notifications: SettingsNotificationPreferencesController;
  clientTelemetry: SettingsClientTelemetryController;
  daemonSettings: SettingsDaemonDocumentController;
  themeVariant: "light" | "dark";
  account: SettingsAccountController;
  mobileQrFgColor: string;
  resourceUtilization: SettingsResourceUtilizationController;
  devTools: SettingsDevToolsController;
}) {
  const {
    active,
    workspaceId,
    general,
    notifications,
    clientTelemetry,
    daemonSettings,
    themeVariant,
    account,
    mobileQrFgColor,
    resourceUtilization,
    devTools,
  } = props;

  const renderDaemonDocumentState = () => {
    if (!daemonSettings.loaded) {
      return <div className="settings-empty">Loading…</div>;
    }
    if (daemonSettings.loadError) {
      return <div className="settings-empty settings-empty-error">{daemonSettings.loadError}</div>;
    }
    return null;
  };

  if (active === "general") {
    return (
      <GeneralSettingsSection
        theme={general.theme}
        onThemeChange={general.onThemeChange}
        editorSettings={general.editorSettings}
        setEditorSettings={general.setEditorSettings}
        editorLoaded={general.editorLoaded}
        editorError={general.editorError}
        updateChannel={general.updateChannel}
        setUpdateChannel={general.setUpdateChannel}
        updateChannelLoaded={general.updateChannelLoaded}
        updateChannelError={general.updateChannelError}
        clientSettingsError={notifications.clientSettingsError}
        showRemoteAuthority={general.showRemoteAuthority}
        isDesktopApp={general.isDesktopApp}
      />
    );
  }

  if (active === "notifications") {
    return (
      <NotificationsSettingsSection
        isDesktopApp={notifications.isDesktopApp}
        completedNotifications={notifications.completedNotifications}
        failedNotifications={notifications.failedNotifications}
        badgeUnreadCount={notifications.badgeUnreadCount}
        desktopNotificationPermission={notifications.desktopNotificationPermission}
        desktopNotificationPermissionBusy={notifications.desktopNotificationPermissionBusy}
        clientSettingsState={notifications.clientSettingsState}
        clientSettingsSaving={notifications.clientSettingsSaving}
        clientSettingsError={notifications.clientSettingsError}
        onToggleCompletedNotifications={notifications.onToggleCompletedNotifications}
        onToggleFailedNotifications={notifications.onToggleFailedNotifications}
        onToggleBadgeUnreadCount={notifications.onToggleBadgeUnreadCount}
        onRequestDesktopNotificationPermission={notifications.onRequestDesktopNotificationPermission}
      />
    );
  }

  if (active === "analytics") {
    const status = renderDaemonDocumentState();
    if (status) return status;
    return (
      <AnalyticsSettingsSection
        clientTelemetryEnabled={clientTelemetry.enabled}
        clientLoaded={clientTelemetry.loaded}
        clientSaving={clientTelemetry.saving}
        clientError={clientTelemetry.error}
        setClientTelemetryEnabled={clientTelemetry.setEnabled}
        daemonTelemetryEnabled={daemonSettings.telemetry.enabled}
        daemonLoaded={daemonSettings.loaded}
        daemonTelemetrySource={daemonSettings.telemetry.source}
        setDaemonTelemetryEnabled={daemonSettings.telemetry.setEnabled}
      />
    );
  }

  if (active === "worktree_bootstrap") {
    return <WorktreeBootstrapSection workspaceId={workspaceId} active />;
  }

  if (active === "agent_system_prompt") {
    return <AgentSystemPromptSection workspaceId={workspaceId} active themeVariant={themeVariant} />;
  }

  if (active === "workspace_attachments") {
    return <WorkspaceAttachmentsSection workspaceId={workspaceId} active />;
  }

  if (active === "container_network") {
    return (
      <ContainerNetworkSection
        workspaceId={workspaceId}
        active
        themeVariant={themeVariant}
        sandboxRuntimeLoaded={daemonSettings.loaded}
        sandboxRuntimeLoadError={daemonSettings.loadError}
        sandboxRuntime={daemonSettings.sandboxing}
      />
    );
  }

  if (active === "merge_queue") {
    return <MergeQueueSection workspaceId={workspaceId} active />;
  }

  if (active === "resource_governance") {
    const status = renderDaemonDocumentState();
    if (status) return status;
    return (
      <ResourceGovernanceSection
        loaded={daemonSettings.loaded}
        enabled={daemonSettings.resourceGovernance.enabled}
        onEnabledChange={daemonSettings.resourceGovernance.setEnabled}
        mode={daemonSettings.resourceGovernance.mode}
        onModeChange={daemonSettings.resourceGovernance.setMode}
        cpuQuotaPct={daemonSettings.resourceGovernance.cpuQuotaPct}
        onCpuQuotaPctChange={daemonSettings.resourceGovernance.setCpuQuotaPct}
        memoryHighGb={daemonSettings.resourceGovernance.memoryHighGb}
        onMemoryHighGbChange={daemonSettings.resourceGovernance.setMemoryHighGb}
        memoryMaxGb={daemonSettings.resourceGovernance.memoryMaxGb}
        onMemoryMaxGbChange={daemonSettings.resourceGovernance.setMemoryMaxGb}
        effective={daemonSettings.resourceGovernance.effective}
        status={daemonSettings.resourceGovernance.status}
        saving={daemonSettings.saving}
        canSave={daemonSettings.resourceGovernance.canSave}
        payload={daemonSettings.resourceGovernance.payload}
        onApplyNow={daemonSettings.resourceGovernance.onApplyNow}
      />
    );
  }

  void account;
  void mobileQrFgColor;

  if (active === "mobile_access" || active === "billing" || active === "team_enterprise") {
    return <div className="settings-empty">This hosted service setting is not included in the public ADE export.</div>;
  }

  if (active === "resource_utilization") {
    return (
      <ResourceUtilizationSection
        workspaceId={workspaceId}
        workspaces={resourceUtilization.workspaces}
        resourceSnapshot={resourceUtilization.snapshot}
        resourceLoading={resourceUtilization.loading}
        resourceError={resourceUtilization.error}
        expandedProcessPids={resourceUtilization.expandedProcessPids}
        onToggleExpanded={resourceUtilization.onToggleExpanded}
      />
    );
  }

  if (active === "dictation") return <DictationSection active />;
  if (active === "title_generation") return <TitleGenerationSection active />;

  if (active === "agent_harnesses") {
    return <HarnessAuthenticationSection workspaceId={workspaceId} active />;
  }

  if (active === "harness_subscriptions") {
    return <CodexAccountsSection active />;
  }

  if (active === "dev_tools") {
    return (
      <DevToolsSection
        devToolsEnabled={devTools.enabled}
        devRestartBusy={devTools.restartBusy}
        devRestartError={devTools.restartError}
        devRestartResults={devTools.restartResults}
        onRestart={devTools.onRestart}
      />
    );
  }

  if (active === "models_routing" || active === "context_pack" || active === "usage_analytics") {
    return null;
  }

  return <div className="settings-empty">No settings yet.</div>;
}
