import { useMemo } from "react";
import type { TerminalSession } from "@ctx/types";
import { idToString } from "../../api/client";
import type { PersistedWorkbenchTerminalLayoutV1 } from "../../workbench/types";
import {
  findTerminalIdForLeaf,
  resolveActiveLeafId,
  terminalIdsInLayout,
} from "../terminalLayout";

export function useTerminalPanelScopeModel({
  terminals,
  activeTaskId,
  panelState,
}: {
  terminals: TerminalSession[];
  activeTaskId: string | null;
  panelState: PersistedWorkbenchTerminalLayoutV1;
}) {
  const workspaceTerminals = terminals;
  const taskTerminals = useMemo(() => {
    if (!activeTaskId) return [];
    return terminals.filter((terminal) => idToString(terminal.task_id) === activeTaskId);
  }, [activeTaskId, terminals]);
  const scopeTerminals = panelState.scope === "workspace" ? workspaceTerminals : taskTerminals;

  const terminalsById = useMemo(() => {
    const map = new Map<string, TerminalSession>();
    for (const terminal of terminals) {
      const id = idToString(terminal.id);
      if (id) map.set(id, terminal);
    }
    return map;
  }, [terminals]);

  const scopeState = panelState.scopes[panelState.scope];
  const activeGroup = useMemo(() => {
    if (scopeState.groups.length === 0) return null;
    const activeId = scopeState.activeGroupId ?? scopeState.groups[0].id;
    return scopeState.groups.find((group) => group.id === activeId) ?? scopeState.groups[0];
  }, [scopeState]);
  const activeGroupId = activeGroup?.id ?? null;
  const activeLayout = activeGroup?.layout ?? null;
  const activeLeafId = resolveActiveLeafId(activeLayout, activeGroup?.activeLeafId ?? null);
  const activeTerminalId = activeLayout ? findTerminalIdForLeaf(activeLayout, activeLeafId) : null;

  const orderedTerminals = useMemo(() => {
    const byId = new Map(scopeTerminals.map((terminal) => [idToString(terminal.id), terminal]));
    const ordered: TerminalSession[] = [];
    for (const id of scopeState.tabOrder) {
      const terminal = byId.get(id);
      if (terminal) ordered.push(terminal);
    }
    for (const terminal of scopeTerminals) {
      const id = idToString(terminal.id);
      if (id && !scopeState.tabOrder.includes(id)) ordered.push(terminal);
    }
    return ordered;
  }, [scopeState.tabOrder, scopeTerminals]);

  const groupInfoByTerminal = useMemo(() => {
    const info = new Map<string, { position: "top" | "middle" | "bottom"; size: number }>();
    scopeState.groups.forEach((group) => {
      const ids = terminalIdsInLayout(group.layout);
      if (ids.length <= 1) return;
      const groupSet = new Set(ids);
      const ordered = scopeState.tabOrder.filter((id) => groupSet.has(id));
      const orderedIds = ordered.length > 0 ? ordered : ids;
      orderedIds.forEach((id, idx) => {
        const position = idx === 0 ? "top" : idx === orderedIds.length - 1 ? "bottom" : "middle";
        info.set(id, { position, size: orderedIds.length });
      });
    });
    return info;
  }, [scopeState.groups, scopeState.tabOrder]);

  return {
    workspaceTerminals,
    taskTerminals,
    scopeTerminals,
    terminalsById,
    scopeState,
    activeGroupId,
    activeLeafId,
    activeTerminalId,
    orderedTerminals,
    groupInfoByTerminal,
    scopeDisabled: panelState.scope === "task" && !activeTaskId,
  };
}
