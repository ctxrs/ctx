import React, {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import type { TerminalSession } from "@ctx/types";
import {
  createWorkspaceTerminal,
  deleteTerminal,
  idToString,
  listWorkspaceTerminals,
  type CreateTerminalRequest,
} from "../api/client";
import type {
  TerminalGroupState,
  TerminalPanelScopeState,
  TerminalScope,
} from "../workbench/types";
import {
  createSingleTerminalGroup,
  findGroupForTerminal,
  findLeafIdForTerminal,
  firstLeafId,
  reconcileScopeState,
  removeTerminalFromLayout,
  resolveActiveLeafId,
  splitLeaf,
  terminalIdsInLayout,
  updateSplitRatio,
} from "./terminalLayout";
import { TerminalSplitView } from "./TerminalSplitView";
import { TerminalTabs } from "./TerminalTabs";
import {
  TerminalPanelContextMenu,
  type TerminalPanelContextMenuState,
} from "./terminalPanel/TerminalPanelContextMenu";
import { useTerminalPanelPersistence } from "./terminalPanel/useTerminalPanelPersistence";
import { useTerminalPanelRename } from "./terminalPanel/useTerminalPanelRename";
import { useTerminalPanelScopeModel } from "./terminalPanel/useTerminalPanelScopeModel";
import { useTerminalClients } from "./useTerminalClients";

export type TerminalPanelHandle = {
  createTerminal: (opts: CreateTerminalOptions) => Promise<string | null>;
  focusTerminal: (terminalId: string) => void;
  focusActive: () => void;
  setScope: (scope: TerminalScope) => void;
};

export type CreateTerminalOptions = {
  cwd?: string | null;
  taskId?: string | null;
  sessionId?: string | null;
  worktreeId?: string | null;
  scope?: TerminalScope;
};

type TerminalPanelProps = {
  workspaceId: string;
  activeTaskId: string | null;
  activeSessionId: string | null;
  open: boolean;
  height: number;
  onRequestClose: () => void;
};

export const TerminalPanel = forwardRef<TerminalPanelHandle, TerminalPanelProps>(function TerminalPanel(
  { workspaceId, activeTaskId, activeSessionId, open, height, onRequestClose },
  ref,
) {
  const [terminals, setTerminals] = useState<TerminalSession[]>([]);
  const {
    panelState,
    setPanelState,
    layoutHydrated,
    titleOverrides,
    setTitleOverrides,
  } = useTerminalPanelPersistence(workspaceId);
  const [contextMenu, setContextMenu] = useState<TerminalPanelContextMenuState | null>(null);
  const clientsRef = useTerminalClients(terminals, setTerminals, workspaceId);
  const resizeFrameRef = useRef<number | null>(null);
  const autoCreateWorkspaceRef = useRef(false);

  const refreshTerminals = useCallback(async () => {
    if (!workspaceId) return;
    const list = await listWorkspaceTerminals(workspaceId);
    setTerminals(list);
  }, [workspaceId]);

  useEffect(() => {
    refreshTerminals().catch(() => {});
  }, [refreshTerminals]);

  const {
    workspaceTerminals,
    taskTerminals,
    scopeState,
    activeGroupId,
    activeLeafId,
    activeTerminalId,
    orderedTerminals,
    groupInfoByTerminal,
    scopeDisabled,
    terminalsById,
  } = useTerminalPanelScopeModel({ terminals, activeTaskId, panelState });
  const {
    renamingId,
    renameValue,
    renameInputRef,
    setRenameValue,
    beginRenameTerminal,
    cancelRenameTerminal,
    commitRenameTerminal,
  } = useTerminalPanelRename({ terminalsById, titleOverrides, setTitleOverrides });

  useEffect(() => {
    if (!open) {
      autoCreateWorkspaceRef.current = false;
    }
  }, [open]);

  useEffect(() => {
    if (!layoutHydrated) return;
    setPanelState((prev) => {
      const next = { ...prev };
      const taskState = reconcileScopeState(prev.scopes.task, taskTerminals);
      const workspaceState = reconcileScopeState(prev.scopes.workspace, workspaceTerminals);
      next.scopes = {
        task: taskState,
        workspace: workspaceState,
      };
      if (prev.scope === "task" && !activeTaskId) {
        next.scope = "workspace";
      }
      return next;
    });
  }, [activeTaskId, layoutHydrated, taskTerminals, workspaceTerminals]);

  const updateScopeState = useCallback(
    (scope: TerminalScope, updater: (state: TerminalPanelScopeState) => TerminalPanelScopeState) => {
      setPanelState((prev) => ({
        ...prev,
        scopes: {
          ...prev.scopes,
          [scope]: updater(prev.scopes[scope]),
        },
      }));
    },
    [],
  );

  const focusTerminal = useCallback((terminalId: string) => {
    const client = clientsRef.current.get(terminalId);
    if (!client) return;
    client.focus();
  }, []);

  const scheduleFitAll = useCallback(() => {
    if (resizeFrameRef.current) window.cancelAnimationFrame(resizeFrameRef.current);
    resizeFrameRef.current = window.requestAnimationFrame(() => {
      for (const client of clientsRef.current.values()) {
        client.fit();
      }
    });
  }, []);

  const focusActive = useCallback(() => {
    if (!activeTerminalId) return;
    focusTerminal(activeTerminalId);
  }, [activeTerminalId, focusTerminal]);

  const setScope = useCallback(
    (nextScope: TerminalScope) => {
      setPanelState((prev) => {
        if (prev.scope === nextScope) return prev;
        if (nextScope === "task" && !activeTaskId) return prev;
        if (nextScope === "task") {
          autoCreateWorkspaceRef.current = true;
        }
        return { ...prev, scope: nextScope };
      });
    },
    [activeTaskId],
  );

  const createTerminalSession = useCallback(
    async (opts: CreateTerminalOptions, targetScope: TerminalScope, insertAfterId?: string | null) => {
      const effectiveScope = targetScope === "task" && !activeTaskId ? "workspace" : targetScope;
      if (!workspaceId) return null;
      const req: CreateTerminalRequest = {
        task_id: opts.taskId ?? null,
        session_id: opts.sessionId ?? null,
        worktree_id: opts.worktreeId ?? null,
        cwd: opts.cwd ?? null,
      };
      let created: TerminalSession;
      try {
        created = await createWorkspaceTerminal(workspaceId, req);
      } catch {
        return null;
      }
      setTerminals((prev) => [...prev, created]);
      const id = idToString(created.id);
      if (!id) return null;
      updateScopeState(effectiveScope, (state) => {
        let tabOrder = state.tabOrder;
        if (!tabOrder.includes(id)) {
          if (insertAfterId && tabOrder.includes(insertAfterId)) {
            const index = tabOrder.indexOf(insertAfterId);
            tabOrder = [...tabOrder.slice(0, index + 1), id, ...tabOrder.slice(index + 1)];
          } else {
            tabOrder = [...tabOrder, id];
          }
        }
        return { ...state, tabOrder };
      });
      if (panelState.scope !== effectiveScope) {
        if (effectiveScope === "task") {
          autoCreateWorkspaceRef.current = true;
        }
        setPanelState((prev) => ({ ...prev, scope: effectiveScope }));
      }
      return id;
    },
    [activeTaskId, panelState.scope, updateScopeState, workspaceId],
  );

  const createTerminalForScope = useCallback(
    async (opts: CreateTerminalOptions) => {
      const targetScope = opts.scope ?? panelState.scope;
      const effectiveScope = targetScope === "task" && !activeTaskId ? "workspace" : targetScope;
      const id = await createTerminalSession(opts, effectiveScope);
      if (!id) return null;
      updateScopeState(effectiveScope, (state) => {
        const nextGroup = createSingleTerminalGroup(id);
        return {
          ...state,
          groups: [...state.groups, nextGroup],
          activeGroupId: nextGroup.id,
        };
      });
      return id;
    },
    [activeTaskId, createTerminalSession, panelState.scope, updateScopeState],
  );

  useEffect(() => {
    if (!open || !layoutHydrated) return;
    if (panelState.scope !== "workspace") return;
    if (workspaceTerminals.length > 0) return;
    if (autoCreateWorkspaceRef.current) return;
    autoCreateWorkspaceRef.current = true;
    void createTerminalForScope({ scope: "workspace" });
  }, [createTerminalForScope, layoutHydrated, open, panelState.scope, workspaceTerminals.length]);

  useImperativeHandle(
    ref,
    () => ({
      createTerminal: async (opts) => {
        const terminal = await createTerminalForScope(opts);
        return terminal;
      },
      focusTerminal,
      focusActive,
      setScope,
    }),
    [createTerminalForScope, focusActive, focusTerminal, setScope],
  );

  const handleNewTerminal = useCallback(async () => {
    const scope = panelState.scope;
    const baseTaskId = scope === "task" ? activeTaskId : null;
    await createTerminalForScope({
      taskId: baseTaskId,
      sessionId: scope === "task" ? activeSessionId : null,
      scope,
    });
  }, [activeSessionId, activeTaskId, createTerminalForScope, panelState.scope]);

  const splitTerminalForId = useCallback(
    async (terminalId: string | null) => {
      if (!terminalId) return;
      const scope = panelState.scope;
      const baseTaskId = scope === "task" ? activeTaskId : null;
      const createdId = await createTerminalSession(
        {
          taskId: baseTaskId,
          sessionId: scope === "task" ? activeSessionId : null,
          scope,
        },
        scope,
        terminalId,
      );
      if (!createdId) return;
      updateScopeState(scope, (state) => {
        const match = findGroupForTerminal(state.groups, terminalId);
        if (!match) {
          const baseGroup = createSingleTerminalGroup(terminalId);
          const layout = splitLeaf(baseGroup.layout, baseGroup.activeLeafId ?? baseGroup.layout.id, createdId, "horizontal");
          const activeLeafId = findLeafIdForTerminal(layout, createdId) ?? firstLeafId(layout);
          return {
            ...state,
            groups: [...state.groups, { ...baseGroup, layout, activeLeafId }],
            activeGroupId: baseGroup.id,
          };
        }
        const groupIndex = state.groups.findIndex((group) => group.id === match.group.id);
        if (groupIndex < 0) return state;
        const layout = splitLeaf(match.group.layout, match.leafId, createdId, "horizontal");
        const activeLeafId = findLeafIdForTerminal(layout, createdId) ?? firstLeafId(layout);
        const nextGroups = [...state.groups];
        nextGroups[groupIndex] = {
          ...match.group,
          layout,
          activeLeafId: activeLeafId ?? match.group.activeLeafId,
        };
        return {
          ...state,
          groups: nextGroups,
          activeGroupId: match.group.id,
        };
      });
      focusTerminal(createdId);
      scheduleFitAll();
    },
    [
      activeSessionId,
      activeTaskId,
      createTerminalSession,
      focusTerminal,
      panelState.scope,
      scheduleFitAll,
      updateScopeState,
    ],
  );

  const handleUnsplitTerminal = useCallback(
    (terminalId: string | null) => {
      if (!terminalId) return;
      const scope = panelState.scope;
      updateScopeState(scope, (state) => {
        const match = findGroupForTerminal(state.groups, terminalId);
        if (!match) return state;
        const ids = terminalIdsInLayout(match.group.layout);
        if (ids.length < 2) return state;
        const updatedLayout = removeTerminalFromLayout(match.group.layout, terminalId);
        if (!updatedLayout) return state;
        const updatedGroup: TerminalGroupState = {
          ...match.group,
          layout: updatedLayout,
          activeLeafId: resolveActiveLeafId(updatedLayout, match.group.activeLeafId),
        };
        const newGroup = createSingleTerminalGroup(terminalId);
        const groups = [...state.groups];
        const index = groups.findIndex((group) => group.id === match.group.id);
        if (index >= 0) {
          groups.splice(index, 1, updatedGroup);
          groups.splice(index + 1, 0, newGroup);
        } else {
          groups.push(newGroup);
        }
        return {
          ...state,
          groups,
          activeGroupId: newGroup.id,
        };
      });
      scheduleFitAll();
    },
    [panelState.scope, scheduleFitAll, updateScopeState],
  );

  useEffect(() => {
    if (open) return;
    setContextMenu(null);
    cancelRenameTerminal();
  }, [cancelRenameTerminal, open]);

  const handleKillTerminal = useCallback(
    async (terminalId: string | null) => {
      if (!terminalId) return;
      try {
        await deleteTerminal(terminalId);
      } catch {
        // ignore failures for already-closed terminals
      }
      setTerminals((prev) => prev.filter((t) => idToString(t.id) !== terminalId));
      setTitleOverrides((prev) => {
        if (!(terminalId in prev)) return prev;
        const next = { ...prev };
        delete next[terminalId];
        return next;
      });
      if (renamingId === terminalId) {
        cancelRenameTerminal();
      }
      setPanelState((prev) => {
        const next = { ...prev };
        (Object.keys(prev.scopes) as TerminalScope[]).forEach((scope) => {
          const state = prev.scopes[scope];
          const tabOrder = state.tabOrder.filter((id) => id !== terminalId);
          let groups = state.groups
            .map((group) => {
              const layout = removeTerminalFromLayout(group.layout, terminalId);
              if (!layout) return null;
              const activeLeafId = resolveActiveLeafId(layout, group.activeLeafId);
              return { ...group, layout, activeLeafId };
            })
            .filter(Boolean) as TerminalGroupState[];
          if (groups.length === 0 && tabOrder.length > 0) {
            groups = [createSingleTerminalGroup(tabOrder[0])];
          }
          let activeGroupId = state.activeGroupId;
          if (activeGroupId && !groups.some((group) => group.id === activeGroupId)) {
            activeGroupId = groups[0]?.id ?? null;
          }
          next.scopes[scope] = { ...state, tabOrder, groups, activeGroupId };
        });
        return next;
      });
    },
    [cancelRenameTerminal, renamingId],
  );

  const handleSelectTerminal = useCallback(
    (terminalId: string) => {
      updateScopeState(panelState.scope, (state) => {
        const match = findGroupForTerminal(state.groups, terminalId);
        if (match) {
          const nextGroups = state.groups.map((group) =>
            group.id === match.group.id ? { ...group, activeLeafId: match.leafId } : group,
          );
          return { ...state, groups: nextGroups, activeGroupId: match.group.id };
        }
        const nextGroup = createSingleTerminalGroup(terminalId);
        return {
          ...state,
          groups: [...state.groups, nextGroup],
          activeGroupId: nextGroup.id,
        };
      });
      focusTerminal(terminalId);
    },
    [focusTerminal, panelState.scope, updateScopeState],
  );

  const handleScopeChange = useCallback(
    (next: TerminalScope) => {
      if (next === panelState.scope) return;
      setPanelState((prev) => ({ ...prev, scope: next }));
    },
    [panelState.scope],
  );

  const handleLayoutActivate = useCallback(
    (leafId: string, _terminalId: string) => {
      updateScopeState(panelState.scope, (state) => ({
        ...state,
        groups: state.groups.map((group) =>
          group.id === (state.activeGroupId ?? state.groups[0]?.id) ? { ...group, activeLeafId: leafId } : group,
        ),
        activeGroupId: state.activeGroupId ?? state.groups[0]?.id ?? null,
      }));
    },
    [panelState.scope, updateScopeState],
  );

  const handleSplitResize = useCallback(
    (splitId: string, ratio: number) => {
      updateScopeState(panelState.scope, (state) => {
        if (state.groups.length === 0) return state;
        const activeGroupId = state.activeGroupId ?? state.groups[0].id;
        const groups = state.groups.map((group) =>
          group.id === activeGroupId
            ? { ...group, layout: updateSplitRatio(group.layout, splitId, ratio) }
            : group,
        );
        return { ...state, groups, activeGroupId };
      });
      scheduleFitAll();
    },
    [panelState.scope, scheduleFitAll, updateScopeState],
  );

  useLayoutEffect(() => {
    if (!open) return;
    scheduleFitAll();
    return () => {
      if (resizeFrameRef.current) window.cancelAnimationFrame(resizeFrameRef.current);
      resizeFrameRef.current = null;
    };
  }, [height, open, scheduleFitAll]);

  useLayoutEffect(() => {
    if (!open) return;
    scheduleFitAll();
  }, [activeGroupId, open, scheduleFitAll]);

  useEffect(() => {
    if (!open) return;
    const onResize = () => {
      scheduleFitAll();
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [open, scheduleFitAll]);

  const contextTerminal = contextMenu ? terminalsById.get(contextMenu.terminalId) ?? null : null;
  const contextGroupInfo = contextMenu ? findGroupForTerminal(scopeState.groups, contextMenu.terminalId) : null;
  const contextGroupSize = contextGroupInfo ? terminalIdsInLayout(contextGroupInfo.group.layout).length : 0;
  const canUnsplit = contextGroupSize > 1;

  return (
    <div className="wb-terminal-panel-inner">
      <div className="wb-terminal-body" aria-hidden={!open}>
        <TerminalTabs
          orderedTerminals={orderedTerminals}
          activeTerminalId={activeTerminalId}
          groupInfoByTerminal={groupInfoByTerminal}
          titleOverrides={titleOverrides}
          renamingId={renamingId}
          renameValue={renameValue}
          renameInputRef={renameInputRef}
          scope={panelState.scope}
          scopeDisabled={scopeDisabled}
          activeTaskId={activeTaskId}
          onNewTerminal={handleNewTerminal}
          onRequestClose={onRequestClose}
          onScopeChange={handleScopeChange}
          onSelectTerminal={handleSelectTerminal}
          onRenameValueChange={(value) => setRenameValue(value)}
          onCommitRename={commitRenameTerminal}
          onCancelRename={cancelRenameTerminal}
          onSplitTerminal={(terminalId) => {
            void splitTerminalForId(terminalId);
          }}
          onKillTerminal={(terminalId) => {
            void handleKillTerminal(terminalId);
          }}
          onOpenContextMenu={(terminalId, x, y) => {
            setContextMenu({ terminalId, x, y });
          }}
        />
        <div className="wb-terminal-view">
          {scopeState.groups.length > 0 ? (
            scopeState.groups.map((group) => {
              const groupActiveLeafId = resolveActiveLeafId(group.layout, group.activeLeafId);
              const isActive = group.id === (activeGroupId ?? scopeState.groups[0].id);
              return (
                <div
                  key={group.id}
                  className={`wb-terminal-group ${isActive ? "wb-terminal-group-active" : "wb-terminal-group-hidden"}`}
                >
                  <TerminalSplitView
                    node={group.layout}
                    activeLeafId={groupActiveLeafId}
                    onActivate={handleLayoutActivate}
                    onResize={handleSplitResize}
                    clients={clientsRef}
                  />
                </div>
              );
            })
          ) : (
            <div className="wb-terminal-empty">No terminals yet.</div>
          )}
        </div>
      </div>
      {contextMenu && (
        <TerminalPanelContextMenu
          contextMenu={contextMenu}
          contextTerminal={contextTerminal}
          canUnsplit={canUnsplit}
          onClose={() => setContextMenu(null)}
          onRename={beginRenameTerminal}
          onSplit={(terminalId) => {
            void splitTerminalForId(terminalId);
          }}
          onUnsplit={handleUnsplitTerminal}
          onKill={(terminalId) => {
            void handleKillTerminal(terminalId);
          }}
        />
      )}
    </div>
  );

});
