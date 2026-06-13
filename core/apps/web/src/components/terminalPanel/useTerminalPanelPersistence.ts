import { useEffect, useState, type Dispatch, type SetStateAction } from "react";
import type { PersistedWorkbenchTerminalLayoutV1 } from "../../workbench/types";
import {
  loadWorkbenchTerminalLayoutV1,
  loadWorkbenchTerminalTitlesV1,
  saveWorkbenchTerminalLayoutV1,
  saveWorkbenchTerminalTitlesV1,
} from "../../workbench/persistence";
import { defaultPanelState } from "../terminalLayout";

export type TerminalPanelPersistenceState = {
  panelState: PersistedWorkbenchTerminalLayoutV1;
  setPanelState: Dispatch<SetStateAction<PersistedWorkbenchTerminalLayoutV1>>;
  layoutHydrated: boolean;
  titleOverrides: Record<string, string>;
  setTitleOverrides: Dispatch<SetStateAction<Record<string, string>>>;
};

export function useTerminalPanelPersistence(workspaceId: string): TerminalPanelPersistenceState {
  const [panelState, setPanelState] = useState<PersistedWorkbenchTerminalLayoutV1>(defaultPanelState);
  const [layoutHydrated, setLayoutHydrated] = useState(false);
  const [titlesHydrated, setTitlesHydrated] = useState(false);
  const [titleOverrides, setTitleOverrides] = useState<Record<string, string>>({});

  useEffect(() => {
    let cancelled = false;
    if (!workspaceId) return;
    loadWorkbenchTerminalLayoutV1(workspaceId)
      .then((loaded) => {
        if (cancelled) return;
        if (loaded) setPanelState(loaded);
        else setPanelState(defaultPanelState());
        setLayoutHydrated(true);
      })
      .catch(() => {
        if (cancelled) return;
        setLayoutHydrated(true);
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId]);

  useEffect(() => {
    let cancelled = false;
    setTitlesHydrated(false);
    if (!workspaceId) {
      setTitleOverrides({});
      setTitlesHydrated(true);
      return;
    }
    loadWorkbenchTerminalTitlesV1(workspaceId)
      .then((loaded) => {
        if (cancelled) return;
        setTitleOverrides(loaded?.titles ?? {});
        setTitlesHydrated(true);
      })
      .catch(() => {
        if (cancelled) return;
        setTitleOverrides({});
        setTitlesHydrated(true);
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId]);

  useEffect(() => {
    if (!layoutHydrated) return;
    if (!workspaceId) return;
    saveWorkbenchTerminalLayoutV1(workspaceId, panelState).catch(() => {});
  }, [layoutHydrated, panelState, workspaceId]);

  useEffect(() => {
    if (!titlesHydrated) return;
    if (!workspaceId) return;
    saveWorkbenchTerminalTitlesV1(workspaceId, { v: 1, titles: titleOverrides }).catch(() => {});
  }, [titleOverrides, titlesHydrated, workspaceId]);

  return {
    panelState,
    setPanelState,
    layoutHydrated,
    titleOverrides,
    setTitleOverrides,
  };
}
