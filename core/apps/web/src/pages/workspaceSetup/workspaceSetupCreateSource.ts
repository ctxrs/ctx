import {
  idToString,
  listWorkspaces,
  repoClone,
  repoInit,
  repoStatus,
  repoStagingPath,
} from "../../api/client";
import {
  deriveRepoNameFromUrl,
  parseCloneDestPath,
  resolveWorkspaceName,
} from "./WorkspaceSetupPage.logic";
import type { WorkspaceSetupCreateIntent } from "./createHandoff";
import type { WorkspaceSetupProvisioningPhase } from "./launchProgress";

type BeginProvisioningPhase = (
  phase: WorkspaceSetupProvisioningPhase,
  stepLabel: string,
  message: string,
  workspaceId?: string | null,
) => void;

type PrepareWorkspaceSetupSourceArgs = {
  intent: WorkspaceSetupCreateIntent;
  beginProvisioningPhase: BeginProvisioningPhase;
  confirmInitImportFolder: (path: string) => Promise<boolean>;
  setImportRepoStatus: (status: "idle" | "checking" | "ok" | "error") => void;
  setImportRepoNote: (note: string | null) => void;
};

export type PreparedWorkspaceSetupSource = {
  rootPath: string;
  name: string | undefined;
  workspaceId: string;
};

const getExistingWorkspaceNamesForGenerated = async () => {
  try {
    const all = await listWorkspaces();
    return all
      .map((workspace) => String(workspace.name ?? "").trim())
      .filter(Boolean);
  } catch {
    return [];
  }
};

export async function prepareWorkspaceSetupSource({
  intent,
  beginProvisioningPhase,
  confirmInitImportFolder,
  setImportRepoStatus,
  setImportRepoNote,
}: PrepareWorkspaceSetupSourceArgs): Promise<PreparedWorkspaceSetupSource> {
  const {
    selections,
    sourcePath,
    repoUrl,
    repoBranch,
    workspaceName,
    useSandboxStaging,
  } = intent;

  let rootPath = "";
  let name: string | undefined;
  let workspaceId = "";
  if (selections.source === "import") {
    beginProvisioningPhase(
      "import_repo",
      "Checking import source",
      "Checking the selected folder before importing it as a workspace.",
    );
    rootPath = sourcePath.trim().replace(/\/+$/, "");
    if (!rootPath) throw new Error("Folder is required.");
    let status = await repoStatus({ path: rootPath }).catch(() => null);
    if (!status) {
      setImportRepoStatus("error");
      setImportRepoNote("Could not verify the selected folder. Check the path and try again.");
      throw new Error("Could not verify the selected folder as a repository.");
    }
    if (status && !status.is_repo) {
      const confirmed = await confirmInitImportFolder(rootPath);
      if (!confirmed) {
        setImportRepoStatus("error");
        setImportRepoNote("Folder is not a repo. Initialization cancelled.");
        throw new Error("Selected folder is not a repo.");
      }
      beginProvisioningPhase(
        "init_repo",
        "Initializing Git repository",
        "Initializing a new Git repository in the selected folder.",
      );
      setImportRepoStatus("checking");
      setImportRepoNote("Initializing Git repo in selected folder…");
      const init = await repoInit({ path: rootPath, allow_existing: true, allow_non_empty: true });
      rootPath = String(init.path ?? "").trim() || rootPath;
      status = await repoStatus({ path: rootPath });
      if (!status.is_repo) {
        const detailAfter = String(status.error ?? "").trim();
        throw new Error(detailAfter ? `Selected folder is not a repo: ${detailAfter}` : "Selected folder is not a repo.");
      }
    }
    if (status?.canonical_path) {
      rootPath = String(status.canonical_path).trim() || rootPath;
    }
    setImportRepoStatus("ok");
    setImportRepoNote(null);
    const all = await listWorkspaces();
    const hit = all.find((workspace) => String(workspace.root_path) === rootPath);
    if (hit) {
      workspaceId = idToString(hit.id);
    } else {
      name = workspaceName.trim() || undefined;
    }
  } else if (selections.source === "clone") {
    beginProvisioningPhase(
      "prepare_source",
      "Preparing clone destination",
      useSandboxStaging
        ? "Allocating sandbox staging before cloning the repository."
        : "Preparing the destination folder for the repository clone.",
    );
    let destParent: string;
    let destName: string | null;
    if (useSandboxStaging) {
      const staging = await repoStagingPath();
      destParent = staging.path;
      destName = deriveRepoNameFromUrl(repoUrl) || workspaceName.trim() || null;
      if (!destName) throw new Error("Could not derive repo name from URL.");
    } else {
      const dest = parseCloneDestPath(sourcePath);
      if (!dest) throw new Error("Destination must be an absolute path (e.g. /home/you/projects/ or /home/you/projects/repo-name).");
      destParent = dest.dest_parent;
      destName = dest.dest_name ?? null;
    }
    beginProvisioningPhase(
      "clone_repo",
      "Cloning repository",
      `Cloning ${repoUrl.trim()}${repoBranch.trim() ? ` (${repoBranch.trim()})` : ""}.`,
    );
    const response = await repoClone({
      repo_url: repoUrl.trim(),
      branch: repoBranch.trim() || null,
      dest_parent: destParent,
      dest_name: destName,
    });
    rootPath = response.path;
    const existingWorkspaceNames = await getExistingWorkspaceNamesForGenerated();
    name = resolveWorkspaceName({
      source: selections.source,
      workspaceName,
      repoUrl,
      destPath: useSandboxStaging ? null : sourcePath,
      useSandboxStaging,
      existingWorkspaceNames,
    });
  } else if (selections.source === "new") {
    beginProvisioningPhase(
      "init_repo",
      "Initializing repository",
      useSandboxStaging
        ? "Allocating sandbox staging before creating the new repository."
        : "Initializing a new Git repository for the workspace.",
    );
    let destPath: string;
    if (useSandboxStaging) {
      const staging = await repoStagingPath();
      destPath = staging.path;
    } else {
      destPath = sourcePath.trim().replace(/\/+$/, "");
      if (!destPath) throw new Error("Destination folder is required.");
    }
    const init = await repoInit({ path: destPath, allow_existing: true });
    rootPath = String(init.path ?? "").trim() || destPath;
    const existingWorkspaceNames = await getExistingWorkspaceNamesForGenerated();
    name = resolveWorkspaceName({
      source: selections.source,
      workspaceName,
      repoUrl,
      destPath,
      useSandboxStaging,
      existingWorkspaceNames,
    });
  } else {
    throw new Error("Choose a source option.");
  }

  return {
    rootPath,
    name,
    workspaceId,
  };
}
