export type PluginManifest = {
  schema_version?: number;
  id: string;
  name: string;
  version: string;
  description?: string | null;
  entrypoints?: PluginEntrypoint[];
  contributes?: PluginContributions;
  compatibility?: PluginCompatibility;
};

export type PluginEntrypointKind = "process" | "worker" | "webview";

export type PluginEntrypoint = {
  id: string;
  kind?: PluginEntrypointKind;
  command: string;
  args?: string[];
  cwd?: string | null;
  environment?: Record<string, string>;
};

export type PluginContributions = {
  providers?: PluginProviderContribution[];
  runtimes?: PluginRuntimeContribution[];
  commands?: PluginCommandContribution[];
  collectors?: PluginCollectorContribution[];
  observers?: PluginObserverContribution[];
  ui_surfaces?: PluginUiSurfaceContribution[];
  templates?: PluginWorkbenchTemplateContribution[];
  toolbar_actions?: PluginWorkbenchToolbarActionContribution[];
  artifact_renderers?: PluginArtifactRendererContribution[];
  card_renderers?: PluginWorkbenchCardRendererContribution[];
  detail_sections?: PluginWorkbenchSectionContribution[];
  review_sections?: PluginWorkbenchSectionContribution[];
};

export type PluginProviderContribution = {
  id: string;
  name: string;
  description?: string | null;
  entrypoint?: string | null;
  capabilities?: string[];
};

export type PluginRuntimeContribution = {
  id: string;
  name: string;
  description?: string | null;
  entrypoint?: string | null;
  capabilities?: string[];
};

export type PluginCommandContribution = {
  id: string;
  title: string;
  description?: string | null;
  category?: string | null;
  entrypoint?: string | null;
};

export type PluginCollectorContribution = {
  id: string;
  name: string;
  description?: string | null;
  entrypoint?: string | null;
  events?: string[];
};

export type PluginObserverContribution = {
  id: string;
  name: string;
  description?: string | null;
  entrypoint?: string | null;
  events?: string[];
};

export type PluginUiSurfaceKind =
  | "panel"
  | "sidebar"
  | "status_bar"
  | "command_palette"
  | "settings";

export type PluginUiSurfaceContribution = {
  id: string;
  name: string;
  surface: PluginUiSurfaceKind;
  description?: string | null;
  entrypoint?: string | null;
  contexts?: string[];
};

export type PluginDeclarativeWorkbenchContribution = {
  id: string;
  name: string;
  description?: string | null;
  contexts?: string[];
  data_sources?: string[];
};

export type PluginWorkbenchTemplateContribution =
  PluginDeclarativeWorkbenchContribution & {
    title: string;
    template: string;
  };

export type ApprovedCtxActionId =
  | "work.focus"
  | "task.start"
  | "ctx.command.run"
  | "plugin.command.run"
  | "work.export_redact"
  | "artifact.attach"
  | "note.attest"
  | "gate.update"
  | "provider.settings.open"
  | "provider.session.restart";

export type PluginWorkbenchToolbarActionContribution =
  PluginDeclarativeWorkbenchContribution & {
    title: string;
    command?: string;
    action?: ApprovedCtxActionId;
    icon?: string | null;
  };

export type PluginArtifactRendererContribution =
  PluginDeclarativeWorkbenchContribution & {
    artifact_types: string[];
    renderer: string;
  };

export type PluginWorkbenchCardRendererContribution =
  PluginDeclarativeWorkbenchContribution & {
    card: string;
    renderer: string;
  };

export type PluginWorkbenchSectionContribution =
  PluginDeclarativeWorkbenchContribution & {
    section: string;
    renderer: string;
  };

export type PluginCompatibility = {
  min_ctx_version?: string | null;
  capabilities?: string[];
};

export type PluginEnablement = "enabled" | "disabled";

export type PluginLoadStatus = "not_loaded" | "loaded" | "error";

export type PluginDiagnosticSeverity = "info" | "warning" | "error";

export type PluginDiagnostic = {
  severity: PluginDiagnosticSeverity;
  message: string;
  code?: string | null;
};

export type PluginInventoryItem = {
  id: string;
  name: string;
  version: string;
  enabled: PluginEnablement;
  status: PluginLoadStatus;
  path: string;
  diagnostics?: PluginDiagnostic[];
  last_loaded_at?: string | null;
  revision?: string | null;
  manifest?: PluginManifest | null;
};

export type PluginContributionRegistration<TContribution> = {
  plugin_id: string;
  plugin_name: string;
  plugin_version: string;
  plugin_path: string;
  plugin_revision?: string | null;
  contribution: TContribution;
};

export type PluginExtensionRegistry = {
  revision: number;
  providers?: PluginContributionRegistration<PluginProviderContribution>[];
  runtimes?: PluginContributionRegistration<PluginRuntimeContribution>[];
  commands?: PluginContributionRegistration<PluginCommandContribution>[];
  collectors?: PluginContributionRegistration<PluginCollectorContribution>[];
  observers?: PluginContributionRegistration<PluginObserverContribution>[];
  ui_surfaces?: PluginContributionRegistration<PluginUiSurfaceContribution>[];
  templates?: PluginContributionRegistration<PluginWorkbenchTemplateContribution>[];
  toolbar_actions?: PluginContributionRegistration<PluginWorkbenchToolbarActionContribution>[];
  artifact_renderers?: PluginContributionRegistration<PluginArtifactRendererContribution>[];
  card_renderers?: PluginContributionRegistration<PluginWorkbenchCardRendererContribution>[];
  detail_sections?: PluginContributionRegistration<PluginWorkbenchSectionContribution>[];
  review_sections?: PluginContributionRegistration<PluginWorkbenchSectionContribution>[];
};

export type PluginCommandExecutionRequest = {
  plugin_id: string;
  command_id: string;
  input?: string | null;
  workspace_id?: string | null;
  task_id?: string | null;
  session_id?: string | null;
};

export type PluginCommandExecutionStatus = "completed" | "failed";

export type PluginCommandExecutionResponse = {
  plugin_id: string;
  command_id: string;
  status: PluginCommandExecutionStatus;
  message?: string | null;
  error?: string | null;
  stdout?: string;
  stderr?: string;
  exit_code?: number | null;
};
