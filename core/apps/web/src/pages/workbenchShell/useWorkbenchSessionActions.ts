import { useCallback, useEffect, useMemo, useRef, useState, type MutableRefObject } from "react";
import type { MessageAttachment } from "../../api/client";
import { idToString } from "../../api/client";
import { buildWorkbenchThreadViewModel } from "../workbenchViewModel";
import type { SessionCacheEntry, SessionSupervisorSnapshot } from "../../state/sessionSupervisor";
import type { TerminalPanelHandle } from "../../components/TerminalPanel";
import { findHarnessCatalogEntry } from "../../utils/harnessCatalog";
import { describeClipboardCopyFailure, tryCopyTextToClipboard } from "../../utils/clipboard";
import { errorMessage } from "../../utils/errorMessage";
import { composeModelId, parseModelId } from "../../utils/modelEffort";
import {
  formatWorktreePath,
  sanitizeFileName,
  saveMarkdownExport,
  spinnerDelayForNow,
} from "./WorkbenchPage.utils";

type Params = {
  activeEntry: SessionCacheEntry | null;
  activeSessionId: string | null;
  activeTaskId: string | null;
  activeWorktreeId: string | null;
  singleSessionTitle: string | null;
  worktreePath: string;
  canCopyWorktree: boolean;
  canCopyTaskId: boolean;
  canOpenTerminal: boolean;
  terminalPanelRef: MutableRefObject<TerminalPanelHandle | null>;
  setTerminalOpen: (open: boolean) => void;
  getSupervisorSnapshot: () => SessionSupervisorSnapshot;
  loadMoreTurns: (sessionId: string) => Promise<void>;
};

type TranscriptHydrationResult = {
  ok: boolean;
  partial: boolean;
  error?: unknown;
};

const attachmentLine = (attachments: MessageAttachment[]): string => {
  const names = (attachments ?? [])
    .map((attachment) => String(attachment.name ?? ("blob_id" in attachment ? attachment.blob_id : attachment.kind) ?? "").trim())
    .filter(Boolean);
  if (names.length === 0) return "";
  return `Attachments: ${names.join(", ")}`;
};

export function useWorkbenchSessionActions({
  activeEntry,
  activeSessionId,
  activeTaskId,
  activeWorktreeId,
  singleSessionTitle,
  worktreePath,
  canCopyWorktree,
  canCopyTaskId,
  canOpenTerminal,
  terminalPanelRef,
  setTerminalOpen,
  getSupervisorSnapshot,
  loadMoreTurns,
}: Params) {
  const [transcriptNotice, setTranscriptNotice] = useState<string | null>(null);
  const [copyTranscriptBusy, setCopyTranscriptBusy] = useState(false);
  const copyTranscriptBusyRef = useRef(false);
  const transcriptSpinnerDelayMs = useRef<number>(spinnerDelayForNow());
  const [worktreeCopied, setWorktreeCopied] = useState(false);
  const worktreeCopiedTimerRef = useRef<number | null>(null);

  useEffect(() => {
    if (!worktreeCopied) return;
    if (worktreeCopiedTimerRef.current) {
      window.clearTimeout(worktreeCopiedTimerRef.current);
    }
    worktreeCopiedTimerRef.current = window.setTimeout(() => {
      setWorktreeCopied(false);
      worktreeCopiedTimerRef.current = null;
    }, 1100);
    return () => {
      if (worktreeCopiedTimerRef.current) {
        window.clearTimeout(worktreeCopiedTimerRef.current);
        worktreeCopiedTimerRef.current = null;
      }
    };
  }, [worktreeCopied]);

  const buildSessionLogExport = useCallback(() => {
    if (!activeEntry?.session) return null;
    const session = activeEntry.session;
    const harness =
      findHarnessCatalogEntry(session.provider_id)?.label ?? (session.provider_id ?? "Provider");
    const parsedModel = parseModelId(
      composeModelId(session.model_id ?? "", session.reasoning_effort ?? null),
    );
    const thread = buildWorkbenchThreadViewModel(
      activeEntry.turns ?? [],
      activeEntry.messages ?? [],
      activeEntry.turnToolsByTurnId ?? {},
      activeEntry.events ?? [],
      activeEntry.assistantStreamingByTurnId ?? activeEntry.threadProjection?.assistantStreamingByTurnId ?? {},
    );
    const title = singleSessionTitle ?? "Conversation";
    const lines: string[] = [];
    lines.push(`# ${title}`);
    lines.push("");
    lines.push(`- Exported: ${new Date().toISOString()}`);
    lines.push(`- Harness: ${harness}`);
    lines.push(`- Model: ${parsedModel.base || String(session.model_id ?? "")}`);
    if (parsedModel.effort) lines.push(`- Effort: ${parsedModel.effort}`);
    if (worktreePath) lines.push(`- Worktree: ${formatWorktreePath(worktreePath)}`);
    lines.push(`- Session ID: ${idToString(session.id)}`);
    lines.push("");
    lines.push("---");
    lines.push("");

    for (let index = 0; index < thread.groups.length; index += 1) {
      const group = thread.groups[index];
      if (!group) continue;
      lines.push(`## Turn ${index + 1}`);
      lines.push("");

      if (group.header) {
        lines.push(`### User (${String(group.header.created_at ?? "").trim() || "unknown time"})`);
        lines.push("");
        const attachments = attachmentLine(group.header.attachments ?? []);
        if (attachments) {
          lines.push(`_${attachments}_`);
          lines.push("");
        }
        lines.push(String(group.header.content ?? ""));
        lines.push("");
      }

      for (const item of group.items ?? []) {
        if (!item || item.kind === "spacer") continue;
        if (item.kind === "tool") {
          lines.push(`### Tool: ${String(item.title ?? "Tool")}${item.status ? ` (${item.status})` : ""}`);
          lines.push("");
          if (item.tool_kind) {
            lines.push(`**Kind:** \`${String(item.tool_kind)}\``);
            lines.push("");
          }
          if (item.input != null) {
            lines.push("**Input:**");
            lines.push("");
            lines.push("```json");
            try {
              lines.push(JSON.stringify(item.input, null, 2));
            } catch {
              lines.push(String(item.input));
            }
            lines.push("```");
            lines.push("");
          }
          const output = String(item.output_text ?? "").trim();
          if (output) {
            lines.push("**Output:**");
            lines.push("");
            lines.push("```");
            lines.push(output);
            lines.push("```");
            lines.push("");
          }
          continue;
        }
        if (item.kind === "assistant") {
          lines.push(`### Assistant (${String(item.created_at ?? "").trim() || "unknown time"})`);
          lines.push("");
          lines.push(String(item.content ?? ""));
          lines.push("");
        }
      }

      lines.push("---");
      lines.push("");
    }

    return { title, markdown: lines.join("\n") };
  }, [activeEntry, singleSessionTitle, worktreePath]);

  const buildTranscriptExportFromEntry = useCallback(
    (entry: SessionCacheEntry | null) => {
      if (!entry?.session) return null;
      const thread = buildWorkbenchThreadViewModel(
        entry.turns ?? [],
        entry.messages ?? [],
        entry.turnToolsByTurnId ?? {},
        entry.events ?? [],
        entry.assistantStreamingByTurnId ?? entry.threadProjection?.assistantStreamingByTurnId ?? {},
      );

      const title = singleSessionTitle ?? "Conversation";
      const lines: string[] = [`# ${title}`, ""];
      for (const group of thread.groups ?? []) {
        if (group?.header) {
          lines.push("User:");
          lines.push("");
          lines.push(String(group.header.content ?? ""));
          lines.push("");
        }
        for (const item of group?.items ?? []) {
          if (!item || item.kind !== "assistant") continue;
          lines.push("Assistant:");
          lines.push("");
          lines.push(String(item.content ?? ""));
          lines.push("");
        }
      }
      return { title, markdown: lines.join("\n") };
    },
    [singleSessionTitle],
  );

  const buildTranscriptExport = useCallback(() => {
    return buildTranscriptExportFromEntry(activeEntry ?? null);
  }, [activeEntry, buildTranscriptExportFromEntry]);

  const hydrateTranscriptHistory = useCallback(async (sessionId: string): Promise<TranscriptHydrationResult> => {
    let lastCursor: number | null = null;
    let stalledCount = 0;
    while (true) {
      const entry = getSupervisorSnapshot().sessions[String(sessionId)];
      if (!entry) return { ok: false, partial: true };
      if (!entry.hasMoreTurns) return { ok: true, partial: false };
      if (entry.fetching?.history) {
        await new Promise((resolve) => setTimeout(resolve, 40));
        continue;
      }
      const beforeCursor = entry.oldestTurnSeq ?? null;
      await loadMoreTurns(sessionId);
      const nextEntry = getSupervisorSnapshot().sessions[String(sessionId)];
      if (!nextEntry) return { ok: false, partial: true };
      if (!nextEntry.hasMoreTurns) return { ok: true, partial: false };
      const afterCursor = nextEntry.oldestTurnSeq ?? null;
      if (afterCursor === beforeCursor && afterCursor === lastCursor) {
        stalledCount += 1;
        if (stalledCount >= 2) return { ok: false, partial: true };
      } else {
        stalledCount = 0;
      }
      lastCursor = afterCursor;
    }
  }, [getSupervisorSnapshot, loadMoreTurns]);

  const exportSessionLog = useCallback(async () => {
    const payload = buildSessionLogExport();
    if (!payload) return;
    try {
      await saveMarkdownExport(`${sanitizeFileName(payload.title)}-session-log`, payload.markdown);
    } catch (error: unknown) {
      window.alert(errorMessage(error) || "Failed to export session log.");
    }
  }, [buildSessionLogExport]);

  const copySessionLog = useCallback(async () => {
    const payload = buildSessionLogExport();
    if (!payload) return;
    const result = await tryCopyTextToClipboard(payload.markdown);
    if (!result.ok) {
      window.alert(describeClipboardCopyFailure(result, { action: "copy the session log to the clipboard" }));
    }
  }, [buildSessionLogExport]);

  const exportTranscript = useCallback(async () => {
    const payload = buildTranscriptExport();
    if (!payload) return;
    try {
      await saveMarkdownExport(`${sanitizeFileName(payload.title)}-transcript`, payload.markdown);
    } catch (error: unknown) {
      window.alert(errorMessage(error) || "Failed to export transcript.");
    }
  }, [buildTranscriptExport]);

  const copyTranscript = useCallback(async () => {
    const sessionId = activeSessionId;
    if (!sessionId || copyTranscriptBusyRef.current) return;
    copyTranscriptBusyRef.current = true;
    setCopyTranscriptBusy(true);
    setTranscriptNotice(null);
    try {
      const entry = getSupervisorSnapshot().sessions[String(sessionId)] ?? null;
      const payload = buildTranscriptExportFromEntry(entry);
      if (!payload) return;
      const copyResult = await tryCopyTextToClipboard(payload.markdown);
      if (!copyResult.ok) {
        setTranscriptNotice(describeClipboardCopyFailure(copyResult, { action: "copy transcript to the clipboard" }));
        return;
      }
      if (!entry?.hasMoreTurns) return;
      let hydrationResult: TranscriptHydrationResult = { ok: true, partial: false };
      try {
        hydrationResult = await hydrateTranscriptHistory(sessionId);
      } catch (error: unknown) {
        hydrationResult = { ok: false, partial: true, error };
      }
      if (!hydrationResult.ok || hydrationResult.partial) {
        setTranscriptNotice("Couldn't load full history. Copied what's already loaded.");
        return;
      }
      setTranscriptNotice("Copied what's already loaded. Earlier turns are ready if you copy again.");
    } catch (error: unknown) {
      setTranscriptNotice(errorMessage(error) || "Failed to copy transcript.");
    } finally {
      copyTranscriptBusyRef.current = false;
      setCopyTranscriptBusy(false);
    }
  }, [activeSessionId, buildTranscriptExportFromEntry, getSupervisorSnapshot, hydrateTranscriptHistory]);

  const copyWorktreeLocation = useCallback(async () => {
    const path = String(worktreePath).trim();
    if (!path || !canCopyWorktree) return;
    const result = await tryCopyTextToClipboard(path);
    if (!result.ok) {
      window.alert(describeClipboardCopyFailure(result, { action: "copy the worktree location to the clipboard" }));
      return;
    }
    setWorktreeCopied(true);
  }, [canCopyWorktree, worktreePath]);

  const copyTaskId = useCallback(async () => {
    const taskId = String(activeTaskId ?? "").trim();
    if (!taskId || !canCopyTaskId) return;
    const result = await tryCopyTextToClipboard(taskId);
    if (!result.ok) {
      window.alert(describeClipboardCopyFailure(result, { action: "copy the task ID to the clipboard" }));
    }
  }, [activeTaskId, canCopyTaskId]);

  const openWorktreeTerminal = useCallback(async () => {
    const path = String(worktreePath).trim();
    if (!path || !canOpenTerminal || !terminalPanelRef.current) return;
    terminalPanelRef.current.setScope("task");
    setTerminalOpen(true);
    const terminalId = await terminalPanelRef.current.createTerminal({
      cwd: path,
      taskId: activeTaskId ?? null,
      sessionId: activeSessionId ?? null,
      worktreeId: activeWorktreeId || null,
      scope: "task",
    });
    if (terminalId) terminalPanelRef.current.focusTerminal(terminalId);
  }, [activeSessionId, activeTaskId, activeWorktreeId, canOpenTerminal, setTerminalOpen, terminalPanelRef, worktreePath]);

  return {
    copyTranscriptBusy,
    transcriptNotice,
    setTranscriptNotice,
    transcriptSpinnerDelayMs: transcriptSpinnerDelayMs.current,
    worktreeCopied,
    copyWorktreeLocation,
    copyTaskId,
    openWorktreeTerminal,
    exportSessionLog,
    copySessionLog,
    exportTranscript,
    copyTranscript,
  };
}
