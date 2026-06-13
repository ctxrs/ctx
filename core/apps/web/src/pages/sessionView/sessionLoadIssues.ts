type SessionLoadErrors = {
  state?: string | null;
  subagentInvocations?: string | null;
} | null | undefined;

export function collectSessionLoadIssues(loadErrors: SessionLoadErrors) {
  const issues: Array<{ key: "state" | "subagentInvocations"; message: string }> = [];
  if (loadErrors?.state) issues.push({ key: "state", message: loadErrors.state });
  if (loadErrors?.subagentInvocations) {
    issues.push({
      key: "subagentInvocations",
      message: loadErrors.subagentInvocations,
    });
  }
  return issues;
}
