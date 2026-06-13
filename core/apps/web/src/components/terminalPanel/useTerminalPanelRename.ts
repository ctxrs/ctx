import { useCallback, useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";
import type { TerminalSession } from "@ctx/types";

export function useTerminalPanelRename({
  terminalsById,
  titleOverrides,
  setTitleOverrides,
}: {
  terminalsById: Map<string, TerminalSession>;
  titleOverrides: Record<string, string>;
  setTitleOverrides: Dispatch<SetStateAction<Record<string, string>>>;
}) {
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const renameInputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (!renamingId) return;
    const frame = window.requestAnimationFrame(() => {
      renameInputRef.current?.focus();
      renameInputRef.current?.select();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [renamingId]);

  const beginRenameTerminal = useCallback(
    (terminalId: string) => {
      const current = titleOverrides[terminalId] ?? terminalsById.get(terminalId)?.title ?? "";
      setRenamingId(terminalId);
      setRenameValue(current);
    },
    [terminalsById, titleOverrides],
  );

  const cancelRenameTerminal = useCallback(() => {
    setRenamingId(null);
    setRenameValue("");
  }, []);

  const commitRenameTerminal = useCallback(() => {
    if (!renamingId) return;
    const terminal = terminalsById.get(renamingId);
    const fallback = terminal?.title ?? "";
    const trimmed = renameValue.trim();
    setTitleOverrides((prev) => {
      const next = { ...prev };
      if (!trimmed || trimmed === fallback) {
        delete next[renamingId];
      } else {
        next[renamingId] = trimmed;
      }
      return next;
    });
    setRenamingId(null);
    setRenameValue("");
  }, [renameValue, renamingId, setTitleOverrides, terminalsById]);

  return {
    renamingId,
    renameValue,
    renameInputRef,
    setRenameValue,
    beginRenameTerminal,
    cancelRenameTerminal,
    commitRenameTerminal,
  };
}
