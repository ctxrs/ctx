import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { applyAppImageUpdate, downloadAppImageUpdate, type UpdateCheck } from "../../api/client";
import {
  desktopApplyAppUpdate,
  desktopRestartApp,
  isDesktopApp,
  type DesktopAppUpdateStateResp,
} from "../../utils/desktop";
import {
  DESKTOP_UPDATE_MENU_STATE_EVENT, REQUEST_UPDATE_CHECK_EVENT,
  REQUEST_UPDATE_RESTART_EVENT, type DesktopUpdateMenuStateDetail,
} from "../../utils/desktopMenuCommands";
import { deriveUpdateNoticeBannerState } from "../../utils/updateNoticeBannerModel";
import { refreshDesktopUpdateNoticeState } from "../../utils/updateNoticeBannerRefresh";
import { readCachedUpdateCheck, refreshUpdateCheck } from "../../utils/updateNotice";
import { useUpdateNoticeVersionState } from "../../utils/useUpdateNoticeVersionState";
import { UPDATER_REFRESH_BROADCAST_STORAGE_KEY, writeUpdaterRefreshBroadcast } from "../../utils/updaterEvents";
import { IDLE_UPDATE_VERSION_STORAGE_KEY, POLL_INTERVAL_MS, PROMPT_SNOOZE_STORAGE_KEY, RESTART_READY_MESSAGE } from "./constants";
import { initialNoticeUiState, noticeUiReducer, type UpdateApplySource } from "./state";
import {
  readIdleUpdateVersions,
  readPromptSnoozeByVersion,
  readRestartRequiredVersion,
  writeIdleUpdateVersions,
} from "./storage";
import type { UpdateNoticeBannerModel, UpdateNoticeBannerProps } from "./types";
import { useRestartReadyDismissal } from "./useRestartReadyDismissal";
import {
  areUpdateChecksEqual,
  getInPlaceCapability,
  isCurrentVersionAtOrAbove,
  isForcedUpdate,
  messageFromUnknownError,
  normalizeOptionalString,
} from "./version";

export type { UpdateNoticeBannerModel, UpdateNoticeBannerProps } from "./types";

export function useUpdateNoticeBanner({
  allTasksIdle = true,
}: UpdateNoticeBannerProps): UpdateNoticeBannerModel {
  const isDesktop = isDesktopApp();
  const [updateInfo, setUpdateInfo] = useState<UpdateCheck | null>(() =>
    isDesktop ? null : readCachedUpdateCheck(),
  );
  const [desktopNativeState, setDesktopNativeState] = useState<DesktopAppUpdateStateResp | null>(null);
  const {
    clearRestartRequiredVersionState,
    clearVersionFlags,
    idleUpdateVersions,
    promptSnoozeByVersion,
    scheduleVersionForNextIdle,
    setIdleUpdateVersions,
    setPromptSnoozeByVersion,
    setRestartRequiredVersionState,
    snoozeVersionPrompt,
  } = useUpdateNoticeVersionState();
  const [uiState, dispatchUi] = useReducer(noticeUiReducer, initialNoticeUiState);
  const [restartingApp, setRestartingApp] = useState(false);
  const [desktopRefreshGeneration, setDesktopRefreshGeneration] = useState(0);
  const applyInFlightRef = useRef(false);
  const manualCheckInFlightRef = useRef(false);
  const updateInfoRef = useRef<UpdateCheck | null>(updateInfo);
  const desktopNativeStateRef = useRef<DesktopAppUpdateStateResp | null>(desktopNativeState);
  const nativeStateSignatureRef = useRef("");

  useEffect(() => {
    updateInfoRef.current = updateInfo;
  }, [updateInfo]);

  useEffect(() => {
    desktopNativeStateRef.current = desktopNativeState;
  }, [desktopNativeState]);

  const {
    clearDismissedRestartReadyVersion,
    dismissedRestartReadyVersion,
    dismissRestartReady,
    restartReadyDismissKey,
  } = useRestartReadyDismissal({
    desktopStateHydrated: !isDesktop || desktopNativeState !== null,
    isDesktop,
    latestVersion: updateInfo?.latest_version,
    restartRequired: uiState.phase === "restart_required",
  });

  const {
    applyingUpdate,
    desktopStagedReady,
    desktopStaging,
    desktopUpdateMenuState,
    effectiveError,
    forcedUpdate,
    canDismissBanner,
    latest,
    latestKnownVersion,
    minimumSupportedVersion,
    releaseNotesUrl,
    restartRequired,
    shouldRenderBanner,
    showUpdateActions,
    snackbarTitle,
    updateActionDisabled,
    updateActionLabel,
  } = deriveUpdateNoticeBannerState({
    isDesktop,
    updateInfo,
    desktopNativeState,
    promptSnoozeByVersion,
    dismissedRestartReadyVersion,
    restartReadyDismissKey,
    restartingApp,
    uiState,
  });
  const showInfoModal = uiState.infoModalOpen;
  const updateError = uiState.error;
  const updateStatus = uiState.status;

  const dismissForLater = useCallback(() => {
    if (restartRequired) {
      dismissRestartReady();
      return;
    }
    if (!latestKnownVersion) return;
    snoozeVersionPrompt(latestKnownVersion);
    writeUpdaterRefreshBroadcast("dismiss-for-later");
  }, [
    latestKnownVersion,
    dismissRestartReady,
    restartRequired,
    snoozeVersionPrompt,
  ]);

  const reconcileRestartRequiredState = useCallback(
    (info: UpdateCheck | null): boolean => {
      const pendingRestartVersion = readRestartRequiredVersion();
      if (!pendingRestartVersion) return false;
      const currentVersion = String(info?.current_version ?? "").trim();
      if (
        currentVersion &&
        isCurrentVersionAtOrAbove(currentVersion, pendingRestartVersion)
      ) {
        clearRestartRequiredVersionState();
        dispatchUi({ type: "apply_completed" });
        return false;
      }
      dispatchUi({
        type: "restart_required",
        message: RESTART_READY_MESSAGE,
      });
      return true;
    },
    [clearRestartRequiredVersionState],
  );

  const refresh = useCallback(
    async (force = false): Promise<UpdateCheck | null> => {
      let info: UpdateCheck | null = null;
      if (isDesktop) {
        const result = await refreshDesktopUpdateNoticeState({
          force,
          previousInfo: updateInfoRef.current,
          previousNativeSignature: nativeStateSignatureRef.current,
        });
        desktopNativeStateRef.current = result.nativeState;
        setDesktopNativeState(result.nativeState);
        nativeStateSignatureRef.current = result.nativeSignature;
        if (result.nativeState) {
          setDesktopRefreshGeneration((prev) => prev + 1);
        }
        if (result.nativeStateChanged) {
          writeUpdaterRefreshBroadcast("native-state-change");
        }
        if (result.restartRequiredVersion) {
          setRestartRequiredVersionState(result.restartRequiredVersion);
        }
        dispatchUi(result.uiAction);
        info = result.info;
      } else {
        info = await refreshUpdateCheck(force ? { force: true } : undefined);
      }

      if (info) {
        updateInfoRef.current = info;
        setUpdateInfo((prev) => (areUpdateChecksEqual(prev, info) ? prev : info));
      }
      const effectiveInfo = info ?? updateInfoRef.current;
      if (!isDesktop) {
        reconcileRestartRequiredState(effectiveInfo);
      }
      return effectiveInfo;
    },
    [isDesktop, reconcileRestartRequiredState, setRestartRequiredVersionState],
  );

  const applyUpdateNow = useCallback(
    async (
      version: string,
      source: UpdateApplySource = "manual",
    ): Promise<boolean> => {
      if (!version || applyInFlightRef.current) return false;
      const recoverIdleFailure = () => {
        if (source === "idle") clearVersionFlags(version);
      };
      const markRestartRequired = (message: string | null | undefined) => {
        clearVersionFlags(version);
        const restartVersion = String(updateInfoRef.current?.latest_version ?? version).trim();
        if (restartVersion) setRestartRequiredVersionState(restartVersion);
        dispatchUi({
          type: "restart_required",
          message: message || RESTART_READY_MESSAGE,
        });
        writeUpdaterRefreshBroadcast("apply-needs-restart");
      };
      applyInFlightRef.current = true;
      dispatchUi({ type: "apply_started" });
      try {
        if (isDesktop) {
          const resp = await desktopApplyAppUpdate();
          if (resp.needs_restart) {
            markRestartRequired(resp.message);
            return true;
          }
          if (resp.applied || resp.up_to_date) {
            clearVersionFlags(version);
            clearRestartRequiredVersionState();
            await refresh(true);
            dispatchUi({ type: "apply_completed" });
            writeUpdaterRefreshBroadcast("apply-complete");
            return true;
          }
          dispatchUi({
            type: "apply_failed",
            message: resp.message || "Update did not apply.",
          });
          recoverIdleFailure();
          return false;
        }

        const capability = getInPlaceCapability(updateInfoRef.current);
        if (!capability.supported) {
          dispatchUi({
            type: "apply_failed",
            message:
              capability.reason ||
              "This install cannot update in place. Install the latest version from release notes, then relaunch.",
          });
          recoverIdleFailure();
          return false;
        }
        const updateChannel =
          normalizeOptionalString(updateInfoRef.current?.channel) || undefined;
        const downloadResp = await downloadAppImageUpdate(updateChannel);
        if (!downloadResp.can_apply_in_place) {
          dispatchUi({
            type: "apply_failed",
            message:
              "This install cannot update in place. Install the latest version from release notes, then relaunch.",
          });
          recoverIdleFailure();
          return false;
        }
        const resp = await applyAppImageUpdate(updateChannel);
        if (!resp.applied) {
          dispatchUi({
            type: "apply_failed",
            message: resp.message || "Update did not apply.",
          });
          recoverIdleFailure();
          return false;
        }
        markRestartRequired(resp.message);
        return true;
      } catch (err: unknown) {
        dispatchUi({
          type: "apply_failed",
          message: messageFromUnknownError(err, "Failed to apply update."),
        });
        recoverIdleFailure();
        return false;
      } finally {
        applyInFlightRef.current = false;
      }
    },
    [
      clearRestartRequiredVersionState,
      clearVersionFlags,
      isDesktop,
      refresh,
      setRestartRequiredVersionState,
    ],
  );

  useEffect(() => {
    let cancelled = false;
    const runInitialCheck = async () => {
      if (!isDesktop) {
        const pendingRestartVersion = readRestartRequiredVersion();
        if (pendingRestartVersion) {
          setRestartRequiredVersionState(pendingRestartVersion);
          dispatchUi({
            type: "restart_required",
            message: RESTART_READY_MESSAGE,
          });
        }
      }
      await refresh(true);
      if (cancelled) return;
    };
    void runInitialCheck();
    const intervalId = window.setInterval(() => {
      void refresh(true);
    }, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(intervalId);
    };
  }, [isDesktop, refresh, setRestartRequiredVersionState]);

  useEffect(() => {
    const onRequestUpdateCheck = () => {
      if (manualCheckInFlightRef.current) return;
      manualCheckInFlightRef.current = true;
      clearDismissedRestartReadyVersion();
      dispatchUi({ type: "manual_check_started" });
      void (async () => {
        try {
          const info = await refresh(true);
          const native = desktopNativeStateRef.current;
          if (native?.restart_required) return;
          const nativePhase = normalizeOptionalString(native?.phase).toLowerCase();
          const nativeError = normalizeOptionalString(native?.last_error || native?.message);
          if (native && !native.configured) {
            dispatchUi({
              type: "manual_failed",
              message: nativeError || "Native updater is not configured.",
            });
            return;
          }
          const refreshError =
            !native && !info?.update_available
              ? normalizeOptionalString(info?.in_place_update_reason)
              : "";
          if (refreshError) {
            dispatchUi({ type: "manual_failed", message: refreshError });
            return;
          }
          if (nativeError && nativePhase === "failed") {
            dispatchUi({ type: "manual_failed", message: nativeError });
            return;
          }
          if (
            nativePhase === "staging" ||
            nativePhase === "staged_ready" ||
            info?.update_available
          ) {
            dispatchUi({
              type: "manual_installing",
              message: "Update found. Installing in background...",
            });
            return;
          }
          dispatchUi({
            type: "manual_up_to_date",
            message: "You're up to date.",
          });
        } catch (err: unknown) {
          dispatchUi({
            type: "manual_failed",
            message: messageFromUnknownError(err, "Update check failed."),
          });
        } finally {
          manualCheckInFlightRef.current = false;
        }
      })();
    };
    window.addEventListener(
      REQUEST_UPDATE_CHECK_EVENT,
      onRequestUpdateCheck as EventListener,
    );
    return () => {
      window.removeEventListener(
        REQUEST_UPDATE_CHECK_EVENT,
        onRequestUpdateCheck as EventListener,
      );
    };
  }, [clearDismissedRestartReadyVersion, refresh]);

  useEffect(() => {
    if (uiState.phase !== "up_to_date" && uiState.phase !== "manual_failed") return;
    const timer = window.setTimeout(() => {
      dispatchUi({ type: "apply_completed" });
    }, 5000);
    return () => {
      window.clearTimeout(timer);
    };
  }, [uiState.phase]);

  useEffect(() => {
    if (!isDesktop) return;
    window.dispatchEvent(
      new CustomEvent<DesktopUpdateMenuStateDetail>(
        DESKTOP_UPDATE_MENU_STATE_EVENT,
        {
          detail: { state: desktopUpdateMenuState },
        },
      ),
    );
  }, [desktopUpdateMenuState, isDesktop]);

  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.storageArea !== window.localStorage) return;
      if (event.key === UPDATER_REFRESH_BROADCAST_STORAGE_KEY) {
        void refresh(true);
        return;
      }
      if (event.key === PROMPT_SNOOZE_STORAGE_KEY) {
        setPromptSnoozeByVersion(readPromptSnoozeByVersion());
        return;
      }
      if (event.key === IDLE_UPDATE_VERSION_STORAGE_KEY) {
        setIdleUpdateVersions(readIdleUpdateVersions());
      }
    };
    window.addEventListener("storage", onStorage);
    return () => {
      window.removeEventListener("storage", onStorage);
    };
  }, [refresh]);

  useEffect(() => {
    if (!isDesktop) return;
    if (restartRequired) return;
    if (!desktopStagedReady) return;
    if (!latestKnownVersion) return;
    if (isForcedUpdate(updateInfo)) return;
    if (readRestartRequiredVersion()) return;
    void applyUpdateNow(latestKnownVersion, "desktop_auto");
  }, [
    applyUpdateNow,
    desktopRefreshGeneration,
    desktopStagedReady,
    isDesktop,
    latestKnownVersion,
    restartRequired,
    updateInfo,
  ]);

  useEffect(() => {
    if (!isDesktop) return;
    if (!desktopStaging) return;
    const timer = window.setTimeout(() => {
      void refresh(true);
    }, 4000);
    return () => {
      window.clearTimeout(timer);
    };
  }, [desktopStaging, isDesktop, refresh]);

  useEffect(() => {
    if (forcedUpdate) return;
    if (!allTasksIdle) return;
    if (!latestKnownVersion) return;
    if (!updateInfo?.update_available) return;
    if (!idleUpdateVersions.has(latestKnownVersion)) return;
    void applyUpdateNow(latestKnownVersion, "idle");
  }, [
    allTasksIdle,
    applyUpdateNow,
    forcedUpdate,
    idleUpdateVersions,
    latestKnownVersion,
    updateInfo?.update_available,
  ]);

  const onUpdateNow = useCallback(() => {
    if (!latestKnownVersion) return;
    void applyUpdateNow(latestKnownVersion, "manual");
  }, [applyUpdateNow, latestKnownVersion]);

  const onRestartNow = useCallback(() => {
    if (!isDesktop || restartingApp) return;
    setRestartingApp(true);
    void desktopRestartApp()
      .catch((err: unknown) => {
        clearDismissedRestartReadyVersion();
        dispatchUi({
          type: "restart_failed",
          message: messageFromUnknownError(err, "Failed to restart app."),
        });
      })
      .finally(() => {
        setRestartingApp(false);
      });
  }, [clearDismissedRestartReadyVersion, isDesktop, restartingApp]);

  useEffect(() => {
    const onRequestUpdateRestart = () => {
      onRestartNow();
    };
    window.addEventListener(
      REQUEST_UPDATE_RESTART_EVENT,
      onRequestUpdateRestart as EventListener,
    );
    return () => {
      window.removeEventListener(
        REQUEST_UPDATE_RESTART_EVENT,
        onRequestUpdateRestart as EventListener,
      );
    };
  }, [onRestartNow]);

  useEffect(() => {
    if (!isDesktop) return;
    if (!restartRequired) return;
    if (!allTasksIdle) return;
    if (restartingApp) return;
    const restartVersion = latestKnownVersion || readRestartRequiredVersion();
    if (!restartVersion) return;
    if (!idleUpdateVersions.has(restartVersion)) return;
    setIdleUpdateVersions((prev) => {
      if (!prev.has(restartVersion)) return prev;
      const next = new Set(prev);
      next.delete(restartVersion);
      writeIdleUpdateVersions(next);
      return next;
    });
    void onRestartNow();
  }, [
    allTasksIdle,
    idleUpdateVersions,
    isDesktop,
    latestKnownVersion,
    onRestartNow,
    restartRequired,
    restartingApp,
  ]);

  const requestUpdateOnNextIdle = useCallback(() => {
    const version = latestKnownVersion || readRestartRequiredVersion();
    if (version) {
      scheduleVersionForNextIdle(version);
      if (!restartRequired) {
        snoozeVersionPrompt(version);
      }
      writeUpdaterRefreshBroadcast("schedule-next-idle");
    }
  }, [
    latestKnownVersion,
    restartRequired,
    scheduleVersionForNextIdle,
    snoozeVersionPrompt,
  ]);

  const onForcedUpdateNow = useCallback(() => {
    const version = latestKnownVersion || minimumSupportedVersion || latest;
    if (!version) return;
    void applyUpdateNow(version, "forced");
  }, [applyUpdateNow, latest, latestKnownVersion, minimumSupportedVersion]);

  return {
    applyingUpdate,
    canDismissBanner,
    effectiveError,
    forcedUpdate,
    latest,
    latestKnownVersion,
    minimumSupportedVersion,
    releaseNotesUrl,
    restartRequired,
    shouldRenderBanner,
    showInfoModal,
    showUpdateActions,
    snackbarTitle,
    updateActionDisabled,
    updateActionLabel,
    updateError,
    updateInfo,
    updateStatus,
    dismissForLater,
    onForcedUpdateNow,
    onRestartNow,
    onUpdateNow,
    openInfoModal: () => dispatchUi({ type: "info_opened" }),
    closeInfoModal: () => dispatchUi({ type: "info_closed" }),
    requestUpdateOnNextIdle,
  };
}
