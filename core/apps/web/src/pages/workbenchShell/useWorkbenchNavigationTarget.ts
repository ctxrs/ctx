import { useEffect } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useWorkbenchStore } from "../../workbench/store";
import {
  readWorkbenchNavigationTarget,
  stripWorkbenchNavigationTarget,
} from "./workbenchNavigationQuery";

export function useWorkbenchNavigationTarget(): void {
  const location = useLocation();
  const navigate = useNavigate();
  const workbenchStore = useWorkbenchStore();

  useEffect(() => {
    const target = readWorkbenchNavigationTarget(location.search);
    if (!target) return;
    const navToken = workbenchStore.getNavToken();
    const didFocus = workbenchStore.focusTask(target.taskId, target.sessionId, {
      navToken,
      source: "system",
    });
    if (!didFocus) return;
    const nextSearch = stripWorkbenchNavigationTarget(location.search);
    navigate(
      {
        pathname: location.pathname,
        search: nextSearch ? `?${nextSearch}` : "",
        hash: location.hash,
      },
      { replace: true },
    );
  }, [location.hash, location.pathname, location.search, navigate, workbenchStore]);
}
