import React from "react";
import { ChevronDown, Plus, SplitSquareVertical, Terminal as TerminalIcon, X } from "lucide-react";
import type { TerminalSession } from "@ctx/types";
import { idToString } from "../api/client";
import { TextInput } from "./ui/text-input";
import type { TerminalScope } from "../workbench/types";

type TerminalTabsProps = {
  orderedTerminals: TerminalSession[];
  activeTerminalId: string | null;
  groupInfoByTerminal: Map<string, { position: "top" | "middle" | "bottom"; size: number }>;
  titleOverrides: Record<string, string>;
  renamingId: string | null;
  renameValue: string;
  renameInputRef: React.RefObject<HTMLInputElement | null>;
  scope: TerminalScope;
  scopeDisabled: boolean;
  activeTaskId: string | null;
  onNewTerminal: () => void;
  onRequestClose: () => void;
  onScopeChange: (scope: TerminalScope) => void;
  onSelectTerminal: (terminalId: string) => void;
  onRenameValueChange: (value: string) => void;
  onCommitRename: () => void;
  onCancelRename: () => void;
  onSplitTerminal: (terminalId: string) => void;
  onKillTerminal: (terminalId: string) => void;
  onOpenContextMenu: (terminalId: string, x: number, y: number) => void;
};

export function TerminalTabs({
  orderedTerminals,
  activeTerminalId,
  groupInfoByTerminal,
  titleOverrides,
  renamingId,
  renameValue,
  renameInputRef,
  scope,
  scopeDisabled,
  activeTaskId,
  onNewTerminal,
  onRequestClose,
  onScopeChange,
  onSelectTerminal,
  onRenameValueChange,
  onCommitRename,
  onCancelRename,
  onSplitTerminal,
  onKillTerminal,
  onOpenContextMenu,
}: TerminalTabsProps) {
  return (
    <div className="wb-terminal-tabs">
      <div className="wb-terminal-toolbar">
        <button type="button" className="wb-terminal-action" title="New terminal" onClick={onNewTerminal}>
          <Plus size={14} />
        </button>
        <div className="wb-terminal-toolbar-spacer" />
        <button type="button" className="wb-terminal-action" title="Hide terminal panel" onClick={onRequestClose}>
          <ChevronDown size={14} />
        </button>
      </div>
      <div className="wb-terminal-scope">
        <button
          type="button"
          className={scope === "task" ? "wb-terminal-scope-active" : ""}
          onClick={() => onScopeChange("task")}
          disabled={!activeTaskId}
        >
          Task
        </button>
        <button
          type="button"
          className={scope === "workspace" ? "wb-terminal-scope-active" : ""}
          onClick={() => onScopeChange("workspace")}
        >
          Workspace
        </button>
      </div>
      <div className="wb-terminal-tablist" aria-disabled={scopeDisabled}>
        {orderedTerminals.length === 0 && (
          <div className="wb-terminal-tab-empty">No terminals</div>
        )}
        {orderedTerminals.map((terminal) => {
          const id = idToString(terminal.id);
          if (!id) return null;
          const active = id === activeTerminalId;
          const exited = terminal.status === "exited";
          const groupInfo = groupInfoByTerminal.get(id) ?? null;
          const title = titleOverrides[id] ?? terminal.title;
          const isRenaming = renamingId === id;
          return (
            <div
              key={id}
              className={`wb-terminal-tab ${active ? "wb-terminal-tab-active" : ""} ${
                exited ? "wb-terminal-tab-exited" : ""
              }`}
              onClick={() => onSelectTerminal(id)}
              onContextMenu={(e) => {
                e.preventDefault();
                onOpenContextMenu(id, e.clientX, e.clientY);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onSelectTerminal(id);
                }
              }}
              role="button"
              tabIndex={0}
            >
              {groupInfo ? (
                <span
                  className={`wb-terminal-tab-split wb-terminal-tab-split-${groupInfo.position}`}
                  aria-hidden="true"
                />
              ) : (
                <TerminalIcon size={14} />
              )}
              {isRenaming ? (
                <TextInput
                  ref={renameInputRef}
                  className="wb-terminal-tab-input"
                  value={renameValue}
                  onChange={(e) => onRenameValueChange(e.target.value)}
                  onClick={(e) => e.stopPropagation()}
                  onMouseDown={(e) => e.stopPropagation()}
                  onBlur={onCommitRename}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      onCommitRename();
                    } else if (e.key === "Escape") {
                      e.preventDefault();
                      onCancelRename();
                    }
                  }}
                />
              ) : (
                <span className="wb-terminal-tab-title">{title}</span>
              )}
              {exited && <span className="wb-terminal-tab-exit">Exited</span>}
              <span className="wb-terminal-tab-spacer" />
              {!isRenaming && (
                <span className="wb-terminal-tab-actions">
                  <button
                    type="button"
                    className="wb-terminal-tab-action"
                    onClick={(e) => {
                      e.stopPropagation();
                      onSplitTerminal(id);
                    }}
                    title="Split terminal"
                    aria-label="Split terminal"
                  >
                    <SplitSquareVertical size={12} />
                  </button>
                  <button
                    type="button"
                    className="wb-terminal-tab-action wb-terminal-tab-close"
                    onClick={(e) => {
                      e.stopPropagation();
                      onKillTerminal(id);
                    }}
                    title="Kill terminal"
                    aria-label="Kill terminal"
                  >
                    <X size={12} />
                  </button>
                </span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
