import React from "react";

export type WorkbenchSessionLoadIssue = {
  key: "state" | "subagentInvocations";
  message: string;
};

export function WorkbenchSessionLoadIssues({
  issues,
  onRetry,
}: {
  issues: WorkbenchSessionLoadIssue[];
  onRetry?: () => void;
}) {
  if (issues.length === 0) return null;

  return (
    <div className="banner wb-session-load-issues" role="alert" data-testid="workbench-session-load-issues">
      <div className="wb-session-load-issues-title">Some session details failed to load.</div>
      {issues.map((issue) => (
        <div key={issue.key}>{issue.message}</div>
      ))}
      {onRetry ? (
        <div>
          <button type="button" onClick={onRetry}>Retry</button>
        </div>
      ) : null}
    </div>
  );
}
