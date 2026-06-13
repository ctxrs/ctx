import type { UpdateCheck } from "../api/client";
import { RESTART_READY_MESSAGE } from "../components/updateNotice/constants";
import type { NoticeUiAction } from "../components/updateNotice/state";
import {
  deriveBaseUrlFromEndpoint,
  messageFromUnknownError,
  normalizeOptionalString,
} from "../components/updateNotice/version";
import {
  desktopGetAppUpdateState,
  type DesktopAppUpdateStateResp,
} from "./desktop";
import { refreshUpdateCheck } from "./updateNotice";

export type DesktopUpdateNoticeRefreshResult = {
  info: UpdateCheck;
  nativeState: DesktopAppUpdateStateResp | null;
  nativeSignature: string;
  nativeStateChanged: boolean;
  restartRequiredVersion: string | null;
  uiAction: NoticeUiAction;
};

const buildNativeStateSignature = (native: DesktopAppUpdateStateResp): string =>
  [
    normalizeOptionalString(native.current_version),
    normalizeOptionalString(native.latest_version),
    normalizeOptionalString(native.phase).toLowerCase(),
    native.restart_required ? "1" : "0",
    native.available ? "1" : "0",
    native.staged ? "1" : "0",
  ].join("|");

const buildDesktopUpdateInfo = (
  native: DesktopAppUpdateStateResp,
  daemonPolicy: UpdateCheck | null,
): UpdateCheck => ({
  channel: normalizeOptionalString(daemonPolicy?.channel) || "stable",
  base_url: deriveBaseUrlFromEndpoint(native.endpoint),
  platform: normalizeOptionalString(native.target) || null,
  current_version: normalizeOptionalString(native.current_version),
  latest_version: normalizeOptionalString(native.latest_version) || null,
  min_supported_version:
    normalizeOptionalString(daemonPolicy?.min_supported_version) || null,
  platform_supported: daemonPolicy?.platform_supported ?? true,
  in_place_update_supported: Boolean(native.configured),
  in_place_update_reason: native.configured
    ? normalizeOptionalString(native.last_error) || null
    : normalizeOptionalString(native.message) || "Native updater is not configured.",
  update_available: Boolean(
    native.available ||
      normalizeOptionalString(native.phase).toLowerCase() === "staging" ||
      normalizeOptionalString(native.phase).toLowerCase() === "staged_ready",
  ),
});

const buildDesktopFallbackInfo = (
  daemonPolicy: UpdateCheck | null,
  previousInfo: UpdateCheck | null,
  reason: string,
): UpdateCheck =>
  previousInfo
    ? {
        ...previousInfo,
        update_available: false,
        in_place_update_reason: reason,
      }
    : {
        channel: normalizeOptionalString(daemonPolicy?.channel) || "stable",
        base_url: normalizeOptionalString(daemonPolicy?.base_url),
        platform: normalizeOptionalString(daemonPolicy?.platform) || null,
        current_version: normalizeOptionalString(daemonPolicy?.current_version),
        latest_version: null,
        min_supported_version:
          normalizeOptionalString(daemonPolicy?.min_supported_version) || null,
        platform_supported: daemonPolicy?.platform_supported ?? true,
        in_place_update_supported: false,
        in_place_update_reason: reason,
        update_available: false,
      };

const buildDesktopUiAction = (
  native: DesktopAppUpdateStateResp,
): { restartRequiredVersion: string | null; uiAction: NoticeUiAction } => {
  const latestVersion = normalizeOptionalString(native.latest_version) || null;
  if (native.restart_required) {
    return {
      restartRequiredVersion: latestVersion,
      uiAction: {
        type: "restart_required",
        message: RESTART_READY_MESSAGE,
      },
    };
  }

  const nativePhase = normalizeOptionalString(native.phase).toLowerCase();
  const nativeLastError = normalizeOptionalString(native.last_error);
  const nativeMessage = normalizeOptionalString(native.message);
  if (nativeLastError) {
    return {
      restartRequiredVersion: null,
      uiAction: {
        type: "check_failed",
        message: nativeLastError,
      },
    };
  }
  if (nativePhase === "failed" && nativeMessage) {
    return {
      restartRequiredVersion: null,
      uiAction: {
        type: "check_failed",
        message: nativeMessage,
      },
    };
  }
  return {
    restartRequiredVersion: null,
    uiAction: { type: "check_recovered" },
  };
};

export const refreshDesktopUpdateNoticeState = async ({
  force,
  previousInfo,
  previousNativeSignature,
}: {
  force: boolean;
  previousInfo: UpdateCheck | null;
  previousNativeSignature: string;
}): Promise<DesktopUpdateNoticeRefreshResult> => {
  let daemonPolicy: UpdateCheck | null = null;
  try {
    daemonPolicy = await refreshUpdateCheck(force ? { force: true } : undefined);
  } catch {
    daemonPolicy = null;
  }

  try {
    const nativeState = await desktopGetAppUpdateState();
    const nativeSignature = buildNativeStateSignature(nativeState);
    const { restartRequiredVersion, uiAction } = buildDesktopUiAction(nativeState);
    return {
      info: buildDesktopUpdateInfo(nativeState, daemonPolicy),
      nativeState,
      nativeSignature,
      nativeStateChanged: nativeSignature !== previousNativeSignature,
      restartRequiredVersion,
      uiAction,
    };
  } catch (err) {
    const reason = messageFromUnknownError(err, "Desktop updater check failed.");
    return {
      info: buildDesktopFallbackInfo(daemonPolicy, previousInfo, reason),
      nativeState: null,
      nativeSignature: "",
      nativeStateChanged: previousNativeSignature !== "",
      restartRequiredVersion: null,
      uiAction: {
        type: "check_failed",
        message: reason,
      },
    };
  }
};
