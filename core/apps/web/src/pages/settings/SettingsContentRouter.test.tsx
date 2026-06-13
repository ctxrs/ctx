import type { ComponentProps } from "react";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { SettingsContentRouter } from "./SettingsContentRouter";

function sectionComponent(name: string, renderProp?: (props: Record<string, unknown>) => string) {
  return function MockSection(props: Record<string, unknown>) {
    return <div data-testid={name}>{renderProp ? renderProp(props) : name}</div>;
  };
}

vi.mock("./sections/GeneralSettingsSection", () => ({
  GeneralSettingsSection: sectionComponent("general", (props) => String(props.theme)),
}));
vi.mock("./sections/NotificationsSettingsSection", () => ({
  NotificationsSettingsSection: sectionComponent("notifications"),
}));
vi.mock("./sections/AnalyticsSettingsSection", () => ({
  AnalyticsSettingsSection: sectionComponent(
    "analytics",
    (props) => `${String(props.clientTelemetryEnabled)}:${String(props.daemonTelemetryEnabled)}`,
  ),
}));
vi.mock("./sections/WorktreeBootstrapSection", () => ({
  WorktreeBootstrapSection: sectionComponent("worktree_bootstrap"),
}));
vi.mock("./sections/AgentSystemPromptSection", () => ({
  AgentSystemPromptSection: sectionComponent("agent_system_prompt"),
}));
vi.mock("./sections/WorkspaceAttachmentsSection", () => ({
  WorkspaceAttachmentsSection: sectionComponent("workspace_attachments"),
}));
vi.mock("./sections/ContainerNetworkSection", () => ({
  ContainerNetworkSection: sectionComponent("container_network"),
}));
vi.mock("./sections/MergeQueueSection", () => ({
  MergeQueueSection: sectionComponent("merge_queue"),
}));
vi.mock("./sections/ResourceGovernanceSection", () => ({
  ResourceGovernanceSection: sectionComponent("resource_governance"),
}));
vi.mock("./sections/MobileAccessSection", () => ({
  MobileAccessSection: sectionComponent("mobile_access"),
}));
vi.mock("./sections/ResourceUtilizationSection", () => ({
  ResourceUtilizationSection: sectionComponent("resource_utilization"),
}));
vi.mock("./sections/DictationSection", () => ({
  DictationSection: sectionComponent("dictation"),
}));
vi.mock("./sections/TitleGenerationSection", () => ({
  TitleGenerationSection: sectionComponent("title_generation"),
}));
vi.mock("./sections/BillingSection", () => ({
  BillingSection: sectionComponent("billing", (props) => String(props.plan)),
}));
vi.mock("./sections/TeamEnterpriseSection", () => ({
  TeamEnterpriseSection: sectionComponent(
    "team_enterprise",
    (props) =>
      `${String(props.plan)}:${String((props.entitlements as { org_id?: string | null } | null)?.org_id ?? "none")}`,
  ),
}));
vi.mock("./sections/HarnessAuthenticationSection", () => ({
  HarnessAuthenticationSection: sectionComponent("harness_authentication"),
}));
vi.mock("./sections/CodexAccountsSection", () => ({
  CodexAccountsSection: sectionComponent("codex_accounts"),
}));
vi.mock("./sections/DevToolsSection", () => ({
  DevToolsSection: sectionComponent("dev_tools"),
}));

const makeProps = (): ComponentProps<typeof SettingsContentRouter> => ({
  active: "general",
  workspaceId: "workspace-1",
  general: {
    theme: "dark",
    onThemeChange: vi.fn(),
    editorSettings: {
      target: "system",
      custom_command: null,
      remote_authority: null,
    },
    setEditorSettings: vi.fn(),
    editorLoaded: true,
    editorError: null,
    updateChannel: "stable",
    setUpdateChannel: vi.fn(),
    updateChannelLoaded: true,
    updateChannelError: null,
    showRemoteAuthority: false,
    isDesktopApp: () => false,
  },
  notifications: {
    isDesktopApp: () => false,
    clientSettingsState: {
      loaded: true,
      settings: {
        v: 3,
        desktopNotifications: {
          turnCompleted: true,
          turnFailed: false,
          badgeUnreadCount: true,
        },
        telemetry: {
          clientEnabled: true,
        },
      },
    },
    clientSettingsSaving: false,
    clientSettingsError: null,
    completedNotifications: true,
    failedNotifications: false,
    badgeUnreadCount: true,
    desktopNotificationPermission: "unsupported",
    desktopNotificationPermissionBusy: false,
    onToggleCompletedNotifications: vi.fn(async () => {}),
    onToggleFailedNotifications: vi.fn(async () => {}),
    onToggleBadgeUnreadCount: vi.fn(async () => {}),
    onRequestDesktopNotificationPermission: vi.fn(async () => {}),
  },
  daemonSettings: {
    loaded: true,
    loadError: null,
    saveError: null,
    saving: false,
    telemetry: {
      enabled: true,
      source: "configured",
      setEnabled: vi.fn(),
    },
    resourceGovernance: {
      enabled: true,
      setEnabled: vi.fn(),
      mode: "auto",
      setMode: vi.fn(),
      cpuQuotaPct: "",
      setCpuQuotaPct: vi.fn(),
      memoryHighGb: "",
      setMemoryHighGb: vi.fn(),
      memoryMaxGb: "",
      setMemoryMaxGb: vi.fn(),
      effective: null,
      status: null,
      canSave: true,
      payload: {
        enabled: true,
        mode: "auto",
        cpu_quota_pct: null,
        memory_high_mb: null,
        memory_max_mb: null,
      },
      onApplyNow: vi.fn(async () => {}),
    },
    sandboxing: {
      machineResolvedMemoryMb: 4096,
      machineIdleShutdownSeconds: "3600",
      setMachineIdleShutdownSeconds: vi.fn(),
      machineHostPressureSwapThresholdMb: "1024",
      setMachineHostPressureSwapThresholdMb: vi.fn(),
      sandboxMachineCanSave: true,
    },
  },
  clientTelemetry: {
    loaded: true,
    saving: false,
    error: null,
    enabled: true,
    setEnabled: vi.fn(async () => {}),
  },
  themeVariant: "dark",
  account: {
    hostedServicesConfigured: true,
    billing: {
      checkoutStatus: null,
      billingUser: null,
      billingEmail: "",
      setBillingEmail: vi.fn(),
      billingPassword: "",
      setBillingPassword: vi.fn(),
      billingBusy: false,
      billingError: null,
      entitlementsBusy: false,
      plan: "pro",
      proEnabled: true,
      onSignIn: vi.fn(async () => {}),
      onSignUp: vi.fn(async () => {}),
      onSignOut: vi.fn(async () => {}),
      onStartCheckout: vi.fn(async () => {}),
      onOpenPortal: vi.fn(async () => {}),
    },
    mobileAccess: {
      billingUser: null,
      entitlementsBusy: false,
      proEnabled: true,
      mobileStatus: null,
      mobileStatusBusy: false,
      mobileStatusError: null,
      mobileEnableBusy: false,
      mobileEnableError: null,
      mobileQr: null,
      onEnable: vi.fn(async () => {}),
      onDisable: vi.fn(async () => {}),
    },
    teamEnterprise: {
      billingUser: null,
      entitlementsBusy: false,
      plan: "pro",
      entitlements: {
        plan_type: "pro",
        org_id: "org-1",
        features: {},
      },
      cloudState: {
        orgs: [],
        activeOrgId: null,
        activeOrg: null,
        billingSubjectId: null,
        invites: [],
        subscriptions: [],
        featureGrants: [],
        adminState: null,
        memberDirectoryAvailable: false,
      },
      cloudBusy: false,
      cloudError: null,
      actionBusy: false,
      actionError: null,
      actionNotice: null,
      orgName: "",
      setOrgName: vi.fn(),
      inviteEmail: "",
      setInviteEmail: vi.fn(),
      inviteRole: "member",
      setInviteRole: vi.fn(),
      seatTarget: "",
      setSeatTarget: vi.fn(),
      policyDraft: {
        providers: "openai, anthropic",
        models: "",
        allowPersonalRoutes: false,
        sandboxProfile: "sandbox_required",
        networkProfile: "default",
        archiveVisibility: "org_summary",
      },
      setPolicyDraft: vi.fn(),
      onRefresh: vi.fn(async () => {}),
      onSelectOrg: vi.fn(async () => {}),
      onCreateOrg: vi.fn(async () => {}),
      onInviteMember: vi.fn(async () => {}),
      onAcceptInvite: vi.fn(async () => {}),
      onUpdateSeats: vi.fn(async () => {}),
      onSavePolicy: vi.fn(async () => {}),
      onStartTeamCheckout: vi.fn(async () => {}),
      onRequestEnterpriseSetup: vi.fn(async () => {}),
    },
  },
  mobileQrFgColor: "#fff",
  resourceUtilization: {
    workspaces: [],
    snapshot: null,
    loading: false,
    error: null,
    expandedProcessPids: {},
    onToggleExpanded: vi.fn(),
  },
  devTools: {
    enabled: true,
    restartBusy: false,
    restartError: null,
    restartResults: null,
    onRestart: vi.fn(async () => {}),
  },
});

describe("SettingsContentRouter", () => {
  it("renders the local-preferences branch without waiting on daemon settings", () => {
    render(
      <SettingsContentRouter
        {...makeProps()}
        active="general"
        daemonSettings={{ ...makeProps().daemonSettings, loaded: false }}
      />,
    );

    expect(screen.getByTestId("general")).toHaveTextContent("dark");
    expect(screen.queryByText("Loading…")).not.toBeInTheDocument();
  });

  it("shows daemon settings loading state only for daemon-backed sections", () => {
    render(
      <SettingsContentRouter
        {...makeProps()}
        active="analytics"
        daemonSettings={{ ...makeProps().daemonSettings, loaded: false }}
      />,
    );

    expect(screen.getByText("Loading…")).toBeInTheDocument();
    expect(screen.queryByTestId("analytics")).not.toBeInTheDocument();
  });

  it("passes split client and daemon telemetry props to the analytics section", () => {
    render(
      <SettingsContentRouter
        {...makeProps()}
        active="analytics"
      />,
    );

    expect(screen.getByTestId("analytics")).toHaveTextContent("true:true");
  });

  it("shows daemon settings errors for daemon-backed sections", () => {
    render(
      <SettingsContentRouter
        {...makeProps()}
        active="analytics"
        daemonSettings={{ ...makeProps().daemonSettings, loadError: "daemon failed" }}
      />,
    );

    expect(screen.getByText("daemon failed")).toBeInTheDocument();
    expect(screen.queryByTestId("analytics")).not.toBeInTheDocument();
  });

  it("keeps the merged sandbox and networking page available even when daemon runtime settings fail to load", () => {
    render(
      <SettingsContentRouter
        {...makeProps()}
        active="container_network"
        daemonSettings={{ ...makeProps().daemonSettings, loadError: "daemon failed" }}
      />,
    );

    expect(screen.getByTestId("container_network")).toBeInTheDocument();
  });

  it("does not route hosted account or team settings in the public export", () => {
    const props = makeProps();
    const { rerender } = render(<SettingsContentRouter {...props} active="billing" />);
    expect(screen.getByText("This hosted service setting is not included in the public ADE export.")).toBeInTheDocument();
    expect(screen.queryByTestId("billing")).not.toBeInTheDocument();

    rerender(<SettingsContentRouter {...props} active="team_enterprise" />);
    expect(screen.getByText("This hosted service setting is not included in the public ADE export.")).toBeInTheDocument();
    expect(screen.queryByTestId("team_enterprise")).not.toBeInTheDocument();

    rerender(<SettingsContentRouter {...props} active="mobile_access" />);
    expect(screen.getByText("This hosted service setting is not included in the public ADE export.")).toBeInTheDocument();
    expect(screen.queryByTestId("mobile_access")).not.toBeInTheDocument();

    rerender(<SettingsContentRouter {...props} active="dev_tools" />);
    expect(screen.getByTestId("dev_tools")).toBeInTheDocument();
  });

  it("returns null for intentionally hidden placeholder sections", () => {
    const { container } = render(
      <SettingsContentRouter
        {...makeProps()}
        active="models_routing"
      />,
    );

    expect(container).toBeEmptyDOMElement();
  });
});
