import { useCallback, useState } from "react";
import { PROMPT_SNOOZE_MS } from "../components/updateNotice/constants";
import {
  clearRestartRequiredVersion,
  readIdleUpdateVersions,
  readPromptSnoozeByVersion,
  writeIdleUpdateVersions,
  writePromptSnoozeByVersion,
  writeRestartRequiredVersion,
} from "../components/updateNotice/storage";

export const useUpdateNoticeVersionState = () => {
  const [promptSnoozeByVersion, setPromptSnoozeByVersion] = useState<
    Record<string, number>
  >(() => readPromptSnoozeByVersion());
  const [idleUpdateVersions, setIdleUpdateVersions] = useState<Set<string>>(() =>
    readIdleUpdateVersions(),
  );

  const snoozeVersionPrompt = useCallback((version: string) => {
    if (!version) return;
    setPromptSnoozeByVersion((prev) => {
      const next = {
        ...prev,
        [version]: Date.now() + PROMPT_SNOOZE_MS,
      };
      writePromptSnoozeByVersion(next);
      return next;
    });
  }, []);

  const clearVersionFlags = useCallback((version: string) => {
    if (!version) return;
    setPromptSnoozeByVersion((prev) => {
      if (!(version in prev)) return prev;
      const next = { ...prev };
      delete next[version];
      writePromptSnoozeByVersion(next);
      return next;
    });
    setIdleUpdateVersions((prev) => {
      if (!prev.has(version)) return prev;
      const next = new Set(prev);
      next.delete(version);
      writeIdleUpdateVersions(next);
      return next;
    });
  }, []);

  const setRestartRequiredVersionState = useCallback((version: string) => {
    if (!version) return;
    writeRestartRequiredVersion(version);
  }, []);

  const clearRestartRequiredVersionState = useCallback(() => {
    clearRestartRequiredVersion();
  }, []);

  const scheduleVersionForNextIdle = useCallback((version: string) => {
    if (!version) return;
    setIdleUpdateVersions((prev) => {
      const next = new Set(prev);
      next.add(version);
      writeIdleUpdateVersions(next);
      return next;
    });
  }, []);

  return {
    clearRestartRequiredVersionState,
    clearVersionFlags,
    idleUpdateVersions,
    promptSnoozeByVersion,
    scheduleVersionForNextIdle,
    setIdleUpdateVersions,
    setPromptSnoozeByVersion,
    setRestartRequiredVersionState,
    snoozeVersionPrompt,
  };
};
