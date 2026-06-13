import { useCallback, useEffect, useState } from "react";
import {
  desktopRecordWorkspaceVisit,
  desktopSetWindowTitle,
  desktopStorageConsumeNotice,
  getDesktopPlatform,
  isDesktopApp,
  type DesktopPlatform,
  type DesktopStorageNotice,
} from "../../utils/desktop";
import { useWorkbenchDesktopMenu } from "./useWorkbenchDesktopMenu";

type UseWorkbenchChromeIntegrationArgs = Parameters<typeof useWorkbenchDesktopMenu>[0] & {
  workspaceId: string;
  workspaceName: string | null;
};

export function useWorkbenchChromeIntegration({
  enabled,
  state,
  handlers,
  workspaceId,
  workspaceName,
}: UseWorkbenchChromeIntegrationArgs) {
  const desktopUi = isDesktopApp();
  const [desktopPlatform, setDesktopPlatform] = useState<DesktopPlatform>(() => {
    if (!desktopUi) return "unknown";
    const platform = typeof navigator === "undefined" ? "" : navigator.platform;
    if (/mac/i.test(platform)) return "macos";
    if (/win/i.test(platform)) return "windows";
    if (/linux/i.test(platform)) return "linux";
    return "unknown";
  });
  const [desktopStorageNotice, setDesktopStorageNotice] = useState<DesktopStorageNotice | null>(null);

  useWorkbenchDesktopMenu({ enabled, state, handlers });

  useEffect(() => {
    if (!desktopUi) return;
    let cancelled = false;
    getDesktopPlatform()
      .then((platform) => {
        if (!cancelled) setDesktopPlatform(platform);
      })
      .catch(() => {
        if (!cancelled) setDesktopPlatform("unknown");
      });
    return () => {
      cancelled = true;
    };
  }, [desktopUi]);

  useEffect(() => {
    if (!desktopUi) return;
    let cancelled = false;
    desktopStorageConsumeNotice()
      .then((notice) => {
        if (!cancelled) {
          setDesktopStorageNotice(notice);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [desktopUi]);

  useEffect(() => {
    if (!desktopUi) return;
    const title = workspaceName ?? "";
    document.title = title;
    desktopSetWindowTitle(title).catch(() => {});
  }, [desktopUi, workspaceName]);

  useEffect(() => {
    if (!desktopUi) return;
    const workspaceIdValue = String(workspaceId || "").trim();
    if (!workspaceIdValue) return;
    const workspaceLabel = String(workspaceName || "").trim() || workspaceIdValue;
    void desktopRecordWorkspaceVisit(workspaceIdValue, workspaceLabel).catch(() => {});
  }, [desktopUi, workspaceId, workspaceName]);

  const useHtmlTopbar = !desktopUi || desktopPlatform !== "macos";
  const focusTaskSearch = useCallback(() => handlers.focusTaskSearch(), [handlers]);

  return {
    desktopUi,
    desktopPlatform,
    desktopStorageNotice,
    setDesktopStorageNotice,
    useHtmlTopbar,
    focusTaskSearch,
  };
}
