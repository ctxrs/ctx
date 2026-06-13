import { useEffect, useRef, useState } from "react";
import { repoStatus, repoValidateDestination } from "../../api/client";
import { desktopPickFolder } from "../../utils/desktop";
import {
  deriveRepoNameFromUrl,
  parseCloneDestPath,
} from "./WorkspaceSetupPage.logic";
import type { RoutePlanInsertionStep } from "./workflowTypes";
import { messageFromError, type ImportInitDialogState } from "./wizardTypes";
import type { WizardSelections } from "./wizardFlowReducer";
import type { WizardStepKey } from "./wizardFlow";

type UseWorkspaceSetupSourcePreflightArgs = {
  currentStepKey: WizardStepKey;
  selections: WizardSelections;
  desktopApp: boolean;
  sourcePath: string;
  repoUrl: string;
  useSandboxStaging: boolean;
  setSourcePath: (value: string) => void;
  setImportRepoStatus: (status: "idle" | "checking" | "ok" | "error") => void;
  setImportRepoNote: (note: string | null) => void;
  setCreateError: (message: string | null) => void;
  connectDaemonForImport: (locationOverride?: "local" | "remote") => Promise<void>;
  ensureOnboardingAfterDaemonConnect: (options?: { allowTitlingInsertion?: boolean }) => Promise<{
    insertionStep: RoutePlanInsertionStep | null;
  } | null>;
  onOnboardingInsertionRequested: (stepKey: RoutePlanInsertionStep) => void;
};

export function useWorkspaceSetupSourcePreflight({
  currentStepKey,
  selections,
  desktopApp,
  sourcePath,
  repoUrl,
  useSandboxStaging,
  setSourcePath,
  setImportRepoStatus,
  setImportRepoNote,
  setCreateError,
  connectDaemonForImport,
  ensureOnboardingAfterDaemonConnect,
  onOnboardingInsertionRequested,
}: UseWorkspaceSetupSourcePreflightArgs) {
  const [importInitDialog, setImportInitDialog] = useState<ImportInitDialogState | null>(null);
  const importInitResolveRef = useRef<((confirmed: boolean) => void) | null>(null);

  useEffect(() => () => {
    const resolve = importInitResolveRef.current;
    importInitResolveRef.current = null;
    resolve?.(false);
  }, []);

  const onPickLocalFolder = async () => {
    if (!desktopApp) return;
    try {
      const picked = await desktopPickFolder();
      if (picked) {
        setCreateError(null);
        setSourcePath(picked);
      }
    } catch {
      // ignore
    }
  };

  const resolveImportInitDialog = (confirmed: boolean) => {
    const resolve = importInitResolveRef.current;
    importInitResolveRef.current = null;
    setImportInitDialog(null);
    resolve?.(confirmed);
  };

  const confirmInitImportFolder = (path: string) =>
    new Promise<boolean>((resolve) => {
      if (importInitResolveRef.current) {
        importInitResolveRef.current(false);
      }
      importInitResolveRef.current = resolve;
      setImportInitDialog({ path });
    });

  const preflightSourceStep = async (): Promise<boolean> => {
    if (currentStepKey !== "source") return true;

    if (desktopApp && selections.location === "local") {
      try {
        await connectDaemonForImport();
        const onboardingResult = await ensureOnboardingAfterDaemonConnect({ allowTitlingInsertion: true });
        if (onboardingResult?.insertionStep) {
          onOnboardingInsertionRequested(onboardingResult.insertionStep);
          return false;
        }
      } catch (error) {
        setCreateError(messageFromError(error));
        return false;
      }
    }

    if (selections.source === "clone" && !useSandboxStaging) {
      const dest = parseCloneDestPath(sourcePath);
      if (!dest) {
        setCreateError("Destination must be an absolute path (e.g. /home/you/projects/ or /home/you/projects/repo-name).");
        return false;
      }
      const destName = dest.dest_name ?? deriveRepoNameFromUrl(repoUrl);
      if (!destName) {
        setCreateError("Could not derive repo name from URL.");
        return false;
      }
      const normalizedParent = dest.dest_parent.replace(/\/+$/, "") || "/";
      const fullDestPath = normalizedParent === "/" ? `/${destName}` : `${normalizedParent}/${destName}`;
      if (selections.location === "remote") {
        return true;
      }
      try {
        await repoValidateDestination({ path: fullDestPath, must_not_exist: true });
      } catch (error) {
        setCreateError(messageFromError(error));
        return false;
      }
      return true;
    }

    if (selections.source === "new" && !useSandboxStaging) {
      const destPath = sourcePath.trim().replace(/\/+$/, "");
      if (!destPath) {
        setCreateError("Destination folder is required.");
        return false;
      }
      if (selections.location === "remote") {
        return true;
      }
      try {
        await repoValidateDestination({
          path: destPath,
          require_empty_if_exists: true,
        });
      } catch (error) {
        setCreateError(messageFromError(error));
        return false;
      }
      return true;
    }

    if (selections.source === "import") {
      const rootPath = sourcePath.trim().replace(/\/+$/, "");
      if (!rootPath) {
        setCreateError("Folder is required.");
        return false;
      }
      if (selections.location === "remote") {
        setImportRepoStatus("ok");
        setImportRepoNote("Remote folder checks run during Create so the remote daemon stays cold until launch.");
        return true;
      }
      setImportRepoStatus("checking");
      setImportRepoNote(null);
      try {
        const status = await repoStatus({ path: rootPath });
        if (status.is_repo) {
          setImportRepoStatus("ok");
          setImportRepoNote(null);
          return true;
        }
        const detail = String(status.error ?? "").trim();
        const note = detail
          ? `Not a git repo yet (${detail}). We'll offer to initialize it during Create.`
          : "Not a git repo yet. We'll offer to initialize it during Create.";
        setImportRepoStatus("ok");
        setImportRepoNote(note);
        return true;
      } catch (error) {
        const message = `Could not verify the selected folder. ${messageFromError(error)}`;
        setImportRepoStatus("error");
        setImportRepoNote(message);
        setCreateError(message);
        return false;
      }
    }

    return true;
  };

  return {
    importInitDialog,
    resolveImportInitDialog,
    confirmInitImportFolder,
    onPickLocalFolder,
    preflightSourceStep,
  };
}
