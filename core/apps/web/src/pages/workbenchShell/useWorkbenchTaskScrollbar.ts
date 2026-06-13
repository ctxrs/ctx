import { useCallback, useEffect, useRef, useState } from "react";

type UseWorkbenchTaskScrollbarArgs = {
  itemCount: number;
};

export function useWorkbenchTaskScrollbar({ itemCount }: UseWorkbenchTaskScrollbarArgs) {
  const taskScrollerRef = useRef<HTMLDivElement | null>(null);
  const [taskScrollerNode, setTaskScrollerNode] = useState<HTMLDivElement | null>(null);
  const taskScrollbarRafRef = useRef<number | null>(null);

  const updateTaskScrollbar = useCallback(() => {
    const scroller = taskScrollerRef.current;
    if (!scroller) return;
    // Keep this callback as the centralized place for future custom scrollbar updates.
    void scroller.scrollTop;
  }, []);

  const scheduleTaskScrollbarUpdate = useCallback(() => {
    if (taskScrollbarRafRef.current != null) return;
    taskScrollbarRafRef.current = window.requestAnimationFrame(() => {
      taskScrollbarRafRef.current = null;
      updateTaskScrollbar();
    });
  }, [updateTaskScrollbar]);

  const onTaskListScrollerChange = useCallback(
    (node: HTMLDivElement | null) => {
      taskScrollerRef.current = node;
      setTaskScrollerNode(node);
      scheduleTaskScrollbarUpdate();
    },
    [scheduleTaskScrollbarUpdate],
  );

  const onTaskListScroll = useCallback(() => {
    scheduleTaskScrollbarUpdate();
  }, [scheduleTaskScrollbarUpdate]);

  useEffect(() => {
    if (!taskScrollerNode) return;
    const observer = new ResizeObserver(() => scheduleTaskScrollbarUpdate());
    observer.observe(taskScrollerNode);
    return () => observer.disconnect();
  }, [scheduleTaskScrollbarUpdate, taskScrollerNode]);

  useEffect(() => {
    scheduleTaskScrollbarUpdate();
  }, [itemCount, scheduleTaskScrollbarUpdate]);

  useEffect(
    () => () => {
      if (taskScrollbarRafRef.current != null) window.cancelAnimationFrame(taskScrollbarRafRef.current);
    },
    [],
  );

  return {
    onTaskListScroll,
    onTaskListScrollerChange,
  };
}
