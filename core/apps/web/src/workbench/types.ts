import type { MessageAttachment } from "../api/client";
import type { WorkbenchModeId } from "../components/WorkbenchComposer";

export type SplitDirection = "horizontal" | "vertical";

export type LayoutNode =
  | {
      kind: "split";
      id: string;
      direction: SplitDirection;
      ratio: number;
      first: LayoutNode;
      second: LayoutNode;
    }
  | {
      kind: "leaf";
      id: string;
      tabs: WorkbenchTab[];
      activeTabId: string;
    };

export type WorkbenchTab =
  | {
      id: string;
      kind: "new_task";
      titleOverride?: string;
      viewMode?: "compact" | "normal" | "verbose";
    }
  | {
      id: string;
      kind: "task";
      ref: {
        taskId: string;
        sessionId?: string | null;
      };
      titleOverride?: string;
      viewMode?: "compact" | "normal" | "verbose";
    };

export type WorkbenchDraft = {
  text: string;
  modeId: WorkbenchModeId;
  attachments: MessageAttachment[];
  updatedAtMs: number;
};

export type TerminalScope = "task" | "workspace";

export type TerminalLayoutNode =
  | {
      kind: "leaf";
      id: string;
      terminalId: string;
    }
  | {
      kind: "split";
      id: string;
      direction: SplitDirection;
      ratio: number;
      first: TerminalLayoutNode;
      second: TerminalLayoutNode;
    };

export type TerminalGroupState = {
  id: string;
  layout: TerminalLayoutNode;
  activeLeafId: string | null;
};

export type TerminalPanelScopeState = {
  groups: TerminalGroupState[];
  activeGroupId: string | null;
  tabOrder: string[];
};

export type PersistedWorkbenchTerminalLayoutV1 = {
  v: 1;
  scope: TerminalScope;
  scopes: {
    task: TerminalPanelScopeState;
    workspace: TerminalPanelScopeState;
  };
};

export type PersistedWorkbenchTerminalTitlesV1 = {
  v: 1;
  titles: Record<string, string>;
};

export type PersistedWorkbenchTerminalOpenV1 = {
  v: 1;
  open: boolean;
  height: number;
};

export type PersistedWorkbenchWindowV1 = {
  v: 1;
  layout: LayoutNode;
  focusedLeafId: string;
};

export type PersistedWorkbenchDraftV1 = {
  v: 1;
  key: string;
  draft: WorkbenchDraft;
};
