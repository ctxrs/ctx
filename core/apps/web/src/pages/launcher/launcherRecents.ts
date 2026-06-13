import {
  getWorkspaceExecutionConfig,
  idToString,
  listWorkspaces,
  repoStatus,
} from "../../api/client";
import type {
  LauncherExecutionEnvironment,
  LauncherRecentEntry,
} from "../../state/launcherRecentsStore";

type WorkspaceSummary = Awaited<ReturnType<typeof listWorkspaces>>[number];

type ResolvedWorkspace = {
  workspaceId: string;
  rootPath: string;
  label: string;
};

const sleepMs = (ms: number) => new Promise((resolve) => window.setTimeout(resolve, ms));

function lastSegment(path: string): string {
  const s = String(path || "").trim().replace(/\/+$/, "");
  const idx = s.lastIndexOf("/");
  return idx >= 0 ? s.slice(idx + 1) : s;
}

function normalizeWorkspacePathForCompare(path: string): string {
  const normalized = String(path || "").trim().replace(/\\/g, "/");
  if (!normalized) return "";

  const windowsDriveRoot = normalized.match(/^([A-Za-z]):\/?$/);
  if (windowsDriveRoot) {
    return `${windowsDriveRoot[1].toLowerCase()}:/`;
  }

  const withoutTrailing = normalized === "/" ? "/" : normalized.replace(/\/+$/, "");
  const windowsDrivePath = withoutTrailing.match(/^([A-Za-z]):(\/.*)$/);
  if (windowsDrivePath) {
    return `${windowsDrivePath[1].toLowerCase()}:${windowsDrivePath[2]}`;
  }

  return withoutTrailing || "/";
}

function findWorkspaceByPath(
  workspaces: WorkspaceSummary[],
  candidatePath: string,
): ResolvedWorkspace | null {
  const normalizedCandidate = normalizeWorkspacePathForCompare(candidatePath);
  if (!normalizedCandidate) return null;

  for (const workspace of workspaces) {
    const workspaceId = idToString(workspace.id ?? "").trim();
    const workspaceRootPath = String(workspace.root_path ?? "").trim();
    if (!workspaceId || !workspaceRootPath) continue;
    if (normalizeWorkspacePathForCompare(workspaceRootPath) !== normalizedCandidate) continue;
    const workspaceLabel = String(workspace.name ?? "").trim();
    return {
      workspaceId,
      rootPath: workspaceRootPath,
      label: workspaceLabel || lastSegment(workspaceRootPath),
    };
  }

  return null;
}

async function resolveWorkspaceByPath(rootPath: string): Promise<ResolvedWorkspace | null> {
  const all = await listWorkspaces();
  const directMatch = findWorkspaceByPath(all, rootPath);
  if (directMatch) return directMatch;

  const trimmedRootPath = String(rootPath || "").trim();
  if (!trimmedRootPath) return null;

  try {
    const status = await repoStatus({ path: trimmedRootPath });
    const canonicalPath = String(status.canonical_path ?? "").trim();
    if (!canonicalPath) return null;
    return findWorkspaceByPath(all, canonicalPath);
  } catch {
    return null;
  }
}

export async function resolveWorkspaceByPathWithRetry(
  rootPath: string,
  timeoutMs: number,
): Promise<ResolvedWorkspace | null> {
  const started = Date.now();
  let lastErr: unknown = null;
  while (Date.now() - started < timeoutMs) {
    try {
      return await resolveWorkspaceByPath(rootPath);
    } catch (e) {
      lastErr = e;
    }
    await sleepMs(200);
  }
  throw lastErr ?? new Error("Timed out resolving workspace.");
}

export async function loadWorkspaceExecutionEnvironment(
  workspaceId: string,
): Promise<LauncherExecutionEnvironment | undefined> {
  if (!workspaceId) return undefined;
  try {
    const config = await getWorkspaceExecutionConfig(workspaceId);
    return normalizeExecutionEnvironment(config.environment);
  } catch {
    return undefined;
  }
}

function normalizeExecutionEnvironment(value: unknown): LauncherExecutionEnvironment | undefined {
  if (value === "host" || value === "sandbox") {
    return value;
  }
  return undefined;
}

function pathForDisplay(path: string): string {
  const normalized = String(path || "").trim().replace(/\\/g, "/");
  if (!normalized) return normalized;
  if (normalized.startsWith("~")) return normalized;

  const macosHome = normalized.match(/^\/Users\/[^/]+(\/.*)?$/);
  if (macosHome) return `~${macosHome[1] ?? ""}`;

  const linuxHome = normalized.match(/^\/home\/[^/]+(\/.*)?$/);
  if (linuxHome) return `~${linuxHome[1] ?? ""}`;

  const windowsHome = normalized.match(/^[A-Za-z]:\/Users\/[^/]+(\/.*)?$/);
  if (windowsHome) return `~${windowsHome[1] ?? ""}`;

  return normalized;
}

export function recentLocationDisplay(recent: LauncherRecentEntry): { label: string; title: string } {
  if (recent.kind === "local") {
    const env = normalizeExecutionEnvironment(recent.execution_environment);
    if (env === "sandbox") {
      return {
        label: "Local sandbox",
        title: recent.root_path,
      };
    }
    const displayPath = pathForDisplay(recent.root_path);
    if (env !== "host") {
      return {
        label: displayPath,
        title: recent.root_path,
      };
    }
    return {
      label: `${displayPath} (Host)`,
      title: `${recent.root_path} (Host)`,
    };
  }

  const target = sshTarget(recent);
  const env = normalizeExecutionEnvironment(recent.execution_environment);
  if (env === "sandbox") {
    return {
      label: `${target} (Remote sandbox)`,
      title: `Remote sandbox on ${target}`,
    };
  }

  const workspaceRootPath = String(recent.workspace_root_path ?? "").trim();
  if (workspaceRootPath) {
    const displayPath = pathForDisplay(workspaceRootPath);
    if (env !== "host") {
      return {
        label: `${target}:${displayPath}`,
        title: `${target}:${workspaceRootPath}`,
      };
    }
    return {
      label: `${target}:${displayPath} (Host)`,
      title: `${target}:${workspaceRootPath} (Host)`,
    };
  }

  const remoteDir = recent.remote_data_dir?.trim() || "/workspace";
  return {
    label: `Remote daemon (${target})`,
    title: `Host ${target} (data dir: ${remoteDir})`,
  };
}

function sshTarget(recent: Extract<LauncherRecentEntry, { kind: "ssh" }>): string {
  const user = recent.user?.trim();
  return user ? `${user}@${recent.host}` : recent.host;
}

export async function recentsFromWorkspaces(
  workspaces: Awaited<ReturnType<typeof listWorkspaces>>,
): Promise<LauncherRecentEntry[]> {
  const entries = await Promise.all(workspaces
    .filter((workspace) => workspace.root_path.trim().length > 0)
    .map(async (workspace) => {
      const workspaceId = idToString(workspace.id ?? "").trim();
      const executionEnvironment = await loadWorkspaceExecutionEnvironment(workspaceId);
      return {
        kind: "local" as const,
        label: workspace.name.trim() || lastSegment(workspace.root_path),
        root_path: workspace.root_path,
        execution_environment: executionEnvironment,
        updated_at_ms: workspaceCreatedAtMs(workspace.created_at),
      };
    }));
  return entries.sort((a, b) => b.updated_at_ms - a.updated_at_ms);
}

function workspaceCreatedAtMs(createdAt: string): number {
  const parsed = Date.parse(createdAt);
  if (!Number.isFinite(parsed)) return 0;
  return parsed;
}

export function recentRenderKey(recent: LauncherRecentEntry): string {
  if (recent.kind === "local") return `local:${recent.root_path}`;
  return `ssh:${recent.user ?? ""}@${recent.host}:${recent.remote_port}:${recent.workspace_root_path ?? ""}:${recent.execution_environment ?? ""}`;
}
