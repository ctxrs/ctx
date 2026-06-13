import { type SubagentInvocation, idToString } from "../../api/client";
import {
  formatSubagentChildMeta,
  humanToolStatus,
  subagentChildLabel,
} from "./SessionPage.helpers";

export function SessionSubagentInvocationsCard({
  subagentInvocations,
  onOpenChildSession,
}: {
  subagentInvocations: SubagentInvocation[];
  onOpenChildSession: (sessionId: string) => void;
}) {
  if (subagentInvocations.length === 0) return null;

  return (
    <div className="subagent-invocations card">
      <div className="row" style={{ justifyContent: "space-between" }}>
        <strong>Subagent invocations</strong>
        <span className="muted">{subagentInvocations.length}</span>
      </div>
      <div className="subagent-invocation-list">
        {subagentInvocations.map((invocation) => {
          const children = invocation.children ?? [];
          const countLabel = `${children.length}/${invocation.requested_count}`;
          return (
            <div key={invocation.id} className="subagent-invocation-row">
              <div className="row" style={{ justifyContent: "space-between", alignItems: "baseline" }}>
                <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
                  <span className="badge">{humanToolStatus(invocation.status)}</span>
                  <span className="muted">Subagents {countLabel}</span>
                </div>
              </div>
              {children.length > 0 ? (
                <ul className="sublist subagent-invocation-children">
                  {children.map((child) => {
                    const childId = idToString(child.child_session_id);
                    return (
                      <li key={`${invocation.id}:${childId || child.position}`} className="row subagent-child-row">
                        <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
                          <span className="badge">{humanToolStatus(child.status)}</span>
                          <button
                            type="button"
                            className="subagent-child-link"
                            onClick={() => childId && onOpenChildSession(childId)}
                            disabled={!childId}
                          >
                            {subagentChildLabel(child)}
                          </button>
                        </div>
                        <span className="muted">{formatSubagentChildMeta(child)}</span>
                      </li>
                    );
                  })}
                </ul>
              ) : (
                <div className="muted">No child sessions yet.</div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
