import type { UpdateCheck } from "../api/client";
import { RESTART_READY_MESSAGE } from "../components/updateNotice/constants";
import type { NoticeUiState } from "../components/updateNotice/state";
import {
  getInPlaceCapability,
  isForcedUpdate,
  normalizeOptionalString,
} from "../components/updateNotice/version";
import type { DesktopAppUpdateStateResp } from "./desktop";
import type { DesktopUpdateMenuState } from "./desktopMenuCommands";

type DeriveUpdateNoticeBannerStateArgs = {
  isDesktop: boolean;
  updateInfo: UpdateCheck | null;
  desktopNativeState: DesktopAppUpdateStateResp | null;
  promptSnoozeByVersion: Record<string, number>;
  dismissedRestartReadyVersion: string;
  restartReadyDismissKey: string;
  uiState: NoticeUiState;
  restartingApp: boolean;
  nowMs?: number;
};

export type UpdateNoticeBannerDerivedState = {
  applyingUpdate: boolean;
  desktopStagedReady: boolean;
  desktopStaging: boolean;
  desktopUpdateMenuState: DesktopUpdateMenuState;
  effectiveError: string | null;
  forcedUpdate: boolean;
  forcedUpdateNeedsManualInstall: boolean;
  latest: string;
  latestKnownVersion: string;
  minimumSupportedVersion: string;
  releaseNotesUrl: string;
  restartRequired: boolean;
  canDismissBanner: boolean;
  shouldRenderBanner: boolean;
  showUpdateActions: boolean;
  snackbarTitle: string;
  updateActionDisabled: boolean;
  updateActionLabel: string;
};

const buildSnackbarTitle = (
  phase: NoticeUiState["phase"],
  restartRequired: boolean,
  latest: string,
): string => {
  if (restartRequired) {
    return `Ready to relaunch: ${latest}.`;
  }
  if (phase === "checking") {
    return "Checking for updates...";
  }
  if (phase === "manual_installing") {
    return "Update found. Installing in background...";
  }
  if (phase === "up_to_date") {
    return "You're up to date.";
  }
  if (phase === "manual_failed") {
    return "Update check failed.";
  }
  return `Update available: ${latest}.`;
};

export const deriveUpdateNoticeBannerState = ({
  isDesktop,
  updateInfo,
  desktopNativeState,
  promptSnoozeByVersion,
  dismissedRestartReadyVersion,
  restartReadyDismissKey,
  uiState,
  restartingApp,
  nowMs = Date.now(),
}: DeriveUpdateNoticeBannerStateArgs): UpdateNoticeBannerDerivedState => {
  const latest = normalizeOptionalString(updateInfo?.latest_version) || "unknown";
  const latestKnownVersion = normalizeOptionalString(updateInfo?.latest_version);
  const minimumSupportedVersion = normalizeOptionalString(updateInfo?.min_supported_version);
  const nextPromptAtMs = latestKnownVersion
    ? Number(promptSnoozeByVersion[latestKnownVersion] ?? 0)
    : 0;
  const desktopPhase = normalizeOptionalString(desktopNativeState?.phase).toLowerCase();
  const desktopStagedReady =
    isDesktop &&
    (desktopNativeState?.staged === true || desktopPhase === "staged_ready");
  const desktopStaging = isDesktop && desktopPhase === "staging";
  const manualTransient =
    uiState.phase === "checking" ||
    uiState.phase === "manual_installing" ||
    uiState.phase === "up_to_date" ||
    uiState.phase === "manual_failed";
  const inPlaceCapability = getInPlaceCapability(updateInfo);
  const canApplyFromCurrentClient = isDesktop || inPlaceCapability.supported;
  const forcedUpdateNeedsManualInstall =
    isForcedUpdate(updateInfo) && !canApplyFromCurrentClient;
  const normalizedRestartReadyDismissKey = normalizeOptionalString(restartReadyDismissKey);
  const restartReadyDismissed =
    isDesktop &&
    uiState.phase === "restart_required" &&
    Boolean(normalizedRestartReadyDismissKey) &&
    normalizeOptionalString(dismissedRestartReadyVersion) ===
      normalizedRestartReadyDismissKey;
  const shouldShow = isDesktop
    ? (uiState.phase === "restart_required" && !restartReadyDismissed) ||
      manualTransient
    : Boolean(updateInfo?.update_available) && nowMs >= nextPromptAtMs;
  const forcedUpdate = isForcedUpdate(updateInfo) && canApplyFromCurrentClient;
  const applyingUpdate = uiState.phase === "applying";
  const restartRequired = uiState.phase === "restart_required";
  const effectiveError =
    uiState.error ||
    (forcedUpdateNeedsManualInstall
      ? inPlaceCapability.reason ||
        "This version is no longer supported on this install path. Install the latest version from release notes."
      : null);
  const desktopUpdateMenuState: DesktopUpdateMenuState = restartRequired
    ? "restart"
    : desktopStaging ||
        (isDesktop &&
          (applyingUpdate ||
            uiState.phase === "checking" ||
            uiState.phase === "manual_installing"))
      ? "downloading"
      : "check";
  const shouldRenderBanner =
    forcedUpdateNeedsManualInstall ||
    shouldShow ||
    (!isDesktop && (applyingUpdate || restartRequired));
  const canDismissBanner =
    !applyingUpdate &&
    !forcedUpdateNeedsManualInstall &&
    shouldRenderBanner &&
    (restartRequired
      ? isDesktop && Boolean(normalizedRestartReadyDismissKey)
      : !isDesktop && Boolean(updateInfo?.update_available) && nowMs >= nextPromptAtMs);
  const restartActionEnabled = restartRequired && isDesktop;
  const updateActionDisabled =
    applyingUpdate || (restartRequired && (!restartActionEnabled || restartingApp));
  const updateActionLabel = restartRequired
    ? "Relaunch"
    : applyingUpdate
      ? "Updating..."
      : "Update Now";
  const snackbarTitle = buildSnackbarTitle(uiState.phase, restartRequired, latest);
  const showUpdateActions =
    restartRequired || (!isDesktop && Boolean(updateInfo?.update_available));

  return {
    applyingUpdate,
    desktopStagedReady,
    desktopStaging,
    desktopUpdateMenuState,
    effectiveError,
    forcedUpdate,
    forcedUpdateNeedsManualInstall,
    latest,
    latestKnownVersion,
    minimumSupportedVersion,
    releaseNotesUrl: `https://ctx.rs/release-notes/${encodeURIComponent(latest)}`,
    restartRequired,
    canDismissBanner,
    shouldRenderBanner,
    showUpdateActions,
    snackbarTitle,
    updateActionDisabled,
    updateActionLabel,
  };
};
