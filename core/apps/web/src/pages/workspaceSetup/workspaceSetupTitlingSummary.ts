import type {
  TitleGenerationLocalStatus,
  TitleGenerationSettings,
} from "../../api/client";
import type { SessionTitlingMode } from "./WorkspaceSetupPage.logic";

type BuildTitlingSummaryValueArgs = {
  titlingMode: SessionTitlingMode;
  titlingRemoteBaseUrl: string;
  titlingRemoteApiKey: string;
  titlingRemoteModel: string;
  titlingConfiguredReady: boolean;
  titlingLocalStatus: TitleGenerationLocalStatus | null;
  titlingExistingSettings: TitleGenerationSettings | null;
};

export function isTitlingRemoteValid(
  titlingRemoteBaseUrl: string,
  titlingRemoteApiKey: string,
  titlingRemoteModel: string,
): boolean {
  return titlingRemoteBaseUrl.trim() !== ""
    && titlingRemoteApiKey.trim() !== ""
    && titlingRemoteModel.trim() !== "";
}

export function buildTitlingSummaryValue({
  titlingMode,
  titlingRemoteModel,
  titlingConfiguredReady,
  titlingLocalStatus,
  titlingExistingSettings,
}: BuildTitlingSummaryValueArgs): string {
  if (titlingMode === "skip") {
    return "Skipped (fallback titles)";
  }
  if (titlingMode === "remote") {
    return `Configured remote (${titlingRemoteModel.trim() || "model pending"})`;
  }
  if (titlingMode === "local") {
    return titlingLocalStatus?.ready
      ? "Configured local (ready)"
      : "Configured local (install pending; fallback until ready)";
  }
  if (titlingConfiguredReady) {
    return titlingExistingSettings?.mode === "local"
      ? "Configured local (ready)"
      : "Configured remote";
  }
  return "Not configured";
}
