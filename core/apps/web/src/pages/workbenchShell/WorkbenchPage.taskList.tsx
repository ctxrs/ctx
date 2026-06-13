import React, { useCallback } from "react";
import type { TaskListContext } from "./WorkbenchPage.types";

type TaskListScrollerProps = React.HTMLAttributes<HTMLDivElement> & {
  context?: TaskListContext;
};

const ARCHIVED_SCROLL_LOAD_THRESHOLD_PX = 120;

const normalizeFiniteLengthStyle = (
  value: React.CSSProperties["paddingBottom"],
): React.CSSProperties["paddingBottom"] =>
  typeof value === "number" && !Number.isFinite(value) ? 0 : value;

export const sanitizeTaskListStyle = (
  style: React.CSSProperties | undefined,
): React.CSSProperties | undefined => {
  if (!style) return style;
  return {
    ...style,
    marginTop: normalizeFiniteLengthStyle(style.marginTop),
    paddingTop: normalizeFiniteLengthStyle(style.paddingTop),
    paddingBottom: normalizeFiniteLengthStyle(style.paddingBottom),
  };
};

const TaskListScroller = React.forwardRef<HTMLDivElement, TaskListScrollerProps>((props, ref) => {
  const { context, onScroll, ...rest } = props;
  const handleRef = useCallback(
    (node: HTMLDivElement | null) => {
      if (typeof ref === "function") {
        ref(node);
      } else if (ref) {
        ref.current = node;
      }
      context?.onScrollerChange?.(node);
    },
    [context, ref],
  );
  const handleScroll = useCallback(
    (event: React.UIEvent<HTMLDivElement>) => {
      onScroll?.(event);
      context?.onScroll?.(event);
      if (!context) return;
      if (context.archivedCollapsed) return;
      if (!context.hasMoreArchived) return;
      if (context.archivedFetchState === "loading") return;
      const target = event.currentTarget;
      if (!target) return;
      if (target.scrollTop + target.clientHeight < target.scrollHeight - ARCHIVED_SCROLL_LOAD_THRESHOLD_PX) return;
      context.onLoadMoreArchived();
    },
    [context, onScroll],
  );

  return <div {...rest} ref={handleRef} className="wb-task-scroll" onScroll={handleScroll} />;
});

const TaskListContainer = React.forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement>>((props, ref) => {
  const { style, ...rest } = props;
  return (
    <div
      {...rest}
      ref={ref}
      className="wb-task-list"
      role="list"
      aria-label="Tasks"
      style={sanitizeTaskListStyle(style)}
    />
  );
});

const TaskListHeader = () => (
  <div className="wb-section-header">
    <div className="wb-section-title">Active Tasks</div>
  </div>
);

export const TASK_LIST_COMPONENTS = {
  Scroller: TaskListScroller,
  List: TaskListContainer,
  Header: TaskListHeader,
};
