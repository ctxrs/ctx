import type { WizardSelections } from "./wizardFlowReducer";
import type { ImportRepoStatus } from "./workflowTypes";
import type { WizardStepKey } from "./wizardFlow";

export type WorkspaceSetupCreateIntent = {
  selections: WizardSelections;
  sourcePath: string;
  repoUrl: string;
  repoBranch: string;
  workspaceName: string;
  networkAllowlist: string;
  useSandboxStaging: boolean;
  importRepoStatus: ImportRepoStatus;
  importRepoNote: string | null;
  targetBranch: string;
  verifyCommand: string;
  mergeQueueSkipped: boolean;
  pushOnSuccess: boolean;
  pushRemote: string;
  pushBranch: string;
  setupHook: string;
  titlingStepVisible: boolean;
  titlingMode: "unset" | "remote" | "local" | "skip";
  titlingRemoteValid: boolean;
  titlingPersistError: string | null;
};

export const buildWorkspaceSetupCreateIntent = (
  intent: WorkspaceSetupCreateIntent,
): WorkspaceSetupCreateIntent => ({
  ...intent,
});

export const resolveCreateErrorStepKey = (value: string): WizardStepKey | null => {
  if (value.includes("Remote host is required")) return "location";
  if (value.includes("session titling") || value.includes("title generation")) return "session-titling";
  if (
    value.includes("repo_url")
    || value.includes("Destination")
    || value.includes("Folder")
    || value.includes("git clone")
    || value.includes("git init")
    || value.includes("root_path")
    || value.includes("not a repo")
  ) {
    return "source";
  }
  return null;
};

export const parseNetworkAllowlist = (raw: string): string[] =>
  raw
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
