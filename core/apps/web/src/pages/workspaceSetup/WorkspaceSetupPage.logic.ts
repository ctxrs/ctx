import type {
  PublicSettings,
  TitleGenerationLocalStatus,
  TitleGenerationSettings,
  UpdateTitleGenerationSettingsRequest,
} from "../../api/client";

type SourceKind = "clone" | "import" | "new";

export type SessionTitlingMode = "unset" | "remote" | "local" | "skip";

export type SessionTitlingReadinessReason =
  | "missing"
  | "remote_ready"
  | "remote_incomplete"
  | "local_ready"
  | "local_missing_model";

export type SessionTitlingReadiness = {
  ready: boolean;
  reason: SessionTitlingReadinessReason;
};

export const DEFAULT_TITLE_REMOTE_BASE_URL = "";
export const DEFAULT_TITLE_REMOTE_MODEL = "";
export const DEFAULT_TITLE_LOCAL_MODEL_ID = "ggml-org/Qwen3-1.7B-GGUF";

const trim = (value: string | null | undefined): string => String(value ?? "").trim();

const boolOrDefault = (value: boolean | null | undefined, defaultValue: boolean): boolean =>
  typeof value === "boolean" ? value : defaultValue;

export const isRemoteTitlingConfigured = (
  titleGeneration: TitleGenerationSettings | null | undefined,
): boolean => {
  if (!titleGeneration || titleGeneration.mode !== "remote") return false;
  return trim(titleGeneration.remote?.base_url) !== ""
    && boolOrDefault(titleGeneration.remote?.api_key_set, false)
    && trim(titleGeneration.remote?.model) !== "";
};

export const isLocalTitlingConfiguredReady = (
  titleGeneration: TitleGenerationSettings | null | undefined,
  _localStatus: TitleGenerationLocalStatus | null | undefined,
): boolean => {
  if (!titleGeneration || titleGeneration.mode !== "local") return false;
  if (trim(titleGeneration.local?.model_id) === "") return false;
  return true;
};

export const resolveSessionTitlingReadiness = (
  settings: Pick<PublicSettings, "title_generation"> | null | undefined,
  localStatus: TitleGenerationLocalStatus | null | undefined,
): SessionTitlingReadiness => {
  const titleGeneration = settings?.title_generation ?? null;
  if (!titleGeneration) {
    return { ready: false, reason: "missing" };
  }
  if (titleGeneration.mode === "remote") {
    return isRemoteTitlingConfigured(titleGeneration)
      ? { ready: true, reason: "remote_ready" }
      : { ready: false, reason: "remote_incomplete" };
  }
  if (trim(titleGeneration.local?.model_id) === "") {
    return { ready: false, reason: "local_missing_model" };
  }
  return { ready: true, reason: "local_ready" };
};

export type SessionTitlingDraft = {
  mode: SessionTitlingMode;
  remote: {
    baseUrl: string;
    apiKey: string;
    model: string;
    useJson: boolean;
  };
  local: {
    modelId: string;
    useJson: boolean;
  };
};

export const buildSessionTitlingDraft = (
  settings: Pick<PublicSettings, "title_generation"> | null | undefined,
): SessionTitlingDraft => {
  const titleGeneration = settings?.title_generation ?? null;
  const mode = titleGeneration?.mode === "remote" || titleGeneration?.mode === "local"
    ? titleGeneration.mode
    : "unset";
  return {
    mode,
    remote: {
      baseUrl: trim(titleGeneration?.remote?.base_url) || DEFAULT_TITLE_REMOTE_BASE_URL,
      apiKey: "",
      model: trim(titleGeneration?.remote?.model) || DEFAULT_TITLE_REMOTE_MODEL,
      useJson: boolOrDefault(titleGeneration?.remote?.use_json, true),
    },
    local: {
      modelId: trim(titleGeneration?.local?.model_id) || DEFAULT_TITLE_LOCAL_MODEL_ID,
      useJson: boolOrDefault(titleGeneration?.local?.use_json, true),
    },
  };
};

export type BuildSessionTitlingPayloadInput = {
  mode: "remote" | "local";
  draft: SessionTitlingDraft;
  existing: TitleGenerationSettings | null | undefined;
};

export const buildSessionTitlingPayload = ({
  mode,
  draft,
  existing,
}: BuildSessionTitlingPayloadInput): UpdateTitleGenerationSettingsRequest => {
  const existingRemote = existing?.remote;
  const existingLocal = existing?.local;
  const remote: UpdateTitleGenerationSettingsRequest["remote"] = {
    base_url: trim(draft.remote.baseUrl) || trim(existingRemote?.base_url) || DEFAULT_TITLE_REMOTE_BASE_URL,
    model: trim(draft.remote.model) || trim(existingRemote?.model) || DEFAULT_TITLE_REMOTE_MODEL,
    use_json: boolOrDefault(draft.remote.useJson, boolOrDefault(existingRemote?.use_json, true)),
  };
  const apiKey = trim(draft.remote.apiKey);
  if (apiKey) {
    remote.api_key = apiKey;
  }
  return {
    mode,
    remote,
    local: {
      model_id: trim(draft.local.modelId) || trim(existingLocal?.model_id) || DEFAULT_TITLE_LOCAL_MODEL_ID,
      use_json: boolOrDefault(draft.local.useJson, boolOrDefault(existingLocal?.use_json, true)),
    },
  };
};

export const sessionTitlingPayloadHash = (payload: UpdateTitleGenerationSettingsRequest): string =>
  JSON.stringify(payload);

export type CloneDestination = {
  dest_parent: string;
  dest_name?: string | null;
};

const isSourceKind = (value: string | null | undefined): value is SourceKind =>
  value === "clone" || value === "import" || value === "new";

const normalizeWorkspaceNames = (names: Iterable<string>): Set<string> => {
  const normalized = new Set<string>();
  for (const name of names) {
    const trimmed = String(name ?? "").trim();
    if (trimmed) normalized.add(trimmed);
  }
  return normalized;
};

export const parseCloneDestPath = (raw: string): CloneDestination | null => {
  const input = String(raw || "").trim();
  if (!input) return null;
  const hasTrailingSlash = /\/+$/.test(input);
  const normalized = input.replace(/\/+$/, "");
  if (!normalized) return null;
  // Basic POSIX parsing (desktop app is our primary target here).
  if (hasTrailingSlash) {
    return { dest_parent: normalized, dest_name: null };
  }
  const idx = normalized.lastIndexOf("/");
  if (idx < 0) return null;
  const dest_parent = normalized.slice(0, idx) || "/";
  const dest_name = normalized.slice(idx + 1).trim();
  if (!dest_name) return null;
  return { dest_parent, dest_name };
};

export const deriveRepoNameFromUrl = (url: string): string | null => {
  const trimmed = url.trim().replace(/\/+$/, "");
  if (!trimmed) return null;
  const normalized = trimmed.replace(":", "/");
  const parts = normalized.split("/");
  const last = parts[parts.length - 1]?.trim();
  if (!last) return null;
  const name = last.replace(/\.git$/i, "").trim();
  return name || null;
};

export type SourceStepValidationInput = {
  source?: string | null;
  sourcePath: string;
  repoUrl: string;
  useSandboxStaging: boolean;
};

export type SourceStepValidation = {
  sourceSelected: boolean;
  needsSourcePath: boolean;
  hasSourcePath: boolean;
  needsRepoUrl: boolean;
  hasRepoUrl: boolean;
  hasValidCloneDestination: boolean;
  isComplete: boolean;
};

export const getSourceStepValidation = ({
  source,
  sourcePath,
  repoUrl,
  useSandboxStaging,
}: SourceStepValidationInput): SourceStepValidation => {
  const selectedSource = isSourceKind(source) ? source : null;
  const sourceSelected = selectedSource !== null;
  const needsRepoUrl = selectedSource === "clone";
  const hasRepoUrl = !needsRepoUrl || repoUrl.trim() !== "";
  const needsSourcePath = sourceSelected && !useSandboxStaging;
  const hasSourcePath = !needsSourcePath || sourcePath.trim() !== "";
  const hasValidCloneDestination = selectedSource !== "clone"
    || !needsSourcePath
    || Boolean(parseCloneDestPath(sourcePath));
  const isComplete = sourceSelected && hasRepoUrl && hasSourcePath && hasValidCloneDestination;

  return {
    sourceSelected,
    needsSourcePath,
    hasSourcePath,
    needsRepoUrl,
    hasRepoUrl,
    hasValidCloneDestination,
    isComplete,
  };
};

export const dedupeGeneratedWorkspaceName = (
  baseName: string,
  existingWorkspaceNames: Iterable<string>,
): string => {
  const existing = normalizeWorkspaceNames(existingWorkspaceNames);
  const normalizedBase = baseName.trim() || "workspace";
  if (!existing.has(normalizedBase)) return normalizedBase;
  let suffix = 2;
  while (existing.has(`${normalizedBase} ${suffix}`)) {
    suffix += 1;
  }
  return `${normalizedBase} ${suffix}`;
};

export type ResolveWorkspaceNameInput = {
  source?: string | null;
  workspaceName: string;
  repoUrl: string;
  destPath?: string | null;
  useSandboxStaging: boolean;
  existingWorkspaceNames: Iterable<string>;
};

export const resolveWorkspaceName = ({
  source,
  workspaceName,
  repoUrl,
  destPath,
  useSandboxStaging,
  existingWorkspaceNames,
}: ResolveWorkspaceNameInput): string | undefined => {
  const userProvidedName = workspaceName.trim();
  if (userProvidedName) return userProvidedName;

  const selectedSource = isSourceKind(source) ? source : null;
  if (selectedSource === "clone") {
    const base = deriveRepoNameFromUrl(repoUrl)
      || parseCloneDestPath(destPath ?? "")?.dest_name
      || null;
    return base ? dedupeGeneratedWorkspaceName(base, existingWorkspaceNames) : undefined;
  }

  if (selectedSource === "new") {
    const base = useSandboxStaging
      ? "new-workspace"
      : parseCloneDestPath(destPath ?? "")?.dest_name || null;
    return base ? dedupeGeneratedWorkspaceName(base, existingWorkspaceNames) : undefined;
  }

  return undefined;
};
