import { useEffect, useRef, useState } from "react";

import { getWorktree, type Worktree } from "../../api/client";
import { deriveManagedWorktreeRoot } from "./WorkbenchPage.utils";

type WorkbenchWorktreeRootStore = {
  getWorktreeRoot: (worktreeId: string) => string | null | undefined;
};

type UseWorkbenchActiveWorktreeArgs = {
  activeTaskArchived: boolean;
  activeWorktreeId: string;
  daemonDataRoot: string | null;
  workspaceId: string;
  workspaceSnapshotStore: WorkbenchWorktreeRootStore;
};

export function useWorkbenchActiveWorktree({
  activeTaskArchived,
  activeWorktreeId,
  daemonDataRoot,
  workspaceId,
  workspaceSnapshotStore,
}: UseWorkbenchActiveWorktreeArgs): Worktree | null {
  const [activeWorktree, setActiveWorktree] = useState<Worktree | null>(null);
  const worktreeCacheRef = useRef<Map<string, Worktree>>(new Map());
  const worktreeFetchRef = useRef<Map<string, Promise<Worktree | null>>>(new Map());

  useEffect(() => {
    if (!activeWorktreeId) {
      setActiveWorktree(null);
      return;
    }

    const cached = worktreeCacheRef.current.get(activeWorktreeId);
    if (cached && (!activeTaskArchived || cached.base_commit_sha)) {
      setActiveWorktree(cached);
      return;
    }

    if (!activeTaskArchived) {
      const cachedRoot = workspaceSnapshotStore.getWorktreeRoot(activeWorktreeId);
      const derivedRoot = cachedRoot || deriveManagedWorktreeRoot(daemonDataRoot, workspaceId, activeWorktreeId);
      if (derivedRoot) {
        const derived: Worktree = {
          id: activeWorktreeId,
          workspace_id: workspaceId,
          root_path: derivedRoot,
          base_commit_sha: "",
          created_at: "",
        };
        worktreeCacheRef.current.set(activeWorktreeId, derived);
        setActiveWorktree(derived);
        return;
      } else {
        setActiveWorktree(null);
      }
    }

    let cancelled = false;
    const existing = worktreeFetchRef.current.get(activeWorktreeId);
    const fetchPromise =
      existing ??
      getWorktree(activeWorktreeId)
        .then((worktree) => {
          worktreeCacheRef.current.set(activeWorktreeId, worktree);
          return worktree;
        })
        .catch(() => null)
        .finally(() => {
          worktreeFetchRef.current.delete(activeWorktreeId);
        });
    worktreeFetchRef.current.set(activeWorktreeId, fetchPromise);
    fetchPromise
      .then((worktree) => {
        if (cancelled) return;
        setActiveWorktree(worktree);
      })
      .catch(() => {
        if (cancelled) return;
        setActiveWorktree(null);
      });

    return () => {
      cancelled = true;
    };
  }, [
    activeTaskArchived,
    activeWorktreeId,
    daemonDataRoot,
    workspaceId,
    workspaceSnapshotStore,
  ]);

  return activeWorktree;
}
