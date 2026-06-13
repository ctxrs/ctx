import { useMemo, useState } from "react";
import { type SessionEvent, idToString } from "../../api/client";

export function SessionDebugPanel({ events }: { events: SessionEvent[] }) {
  const [open, setOpen] = useState(false);
  const kinds = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const event of events) {
      counts[event.event_type] = (counts[event.event_type] ?? 0) + 1;
    }
    return counts;
  }, [events]);

  return (
    <div className="debug card">
      <button type="button" className="debug-header" onClick={() => setOpen((value) => !value)}>
        <strong>Debug</strong>
        <span className="muted">
          {Object.entries(kinds)
            .map(([kind, count]) => `${kind}:${count}`)
            .join(" · ")}
        </span>
        <span className="thinking-chev">{open ? "▴" : "▾"}</span>
      </button>
      {open ? (
        <div className="debug-body">
          {events.map((event) => {
            const key = idToString(event.id);
            if (!key) {
              if (import.meta.env.DEV) {
                // eslint-disable-next-line no-console
                console.error("[DebugPanel] event missing id", {
                  event_type: event.event_type,
                  created_at: event.created_at,
                });
              }
              return null;
            }
            return (
              <details key={key} className="debug-event">
                <summary>
                  <span className="muted">{new Date(event.created_at).toLocaleTimeString()}</span>{" "}
                  <strong>{event.event_type}</strong>
                </summary>
                <pre className="json">{JSON.stringify(event.payload_json, null, 2)}</pre>
              </details>
            );
          })}
        </div>
      ) : null}
    </div>
  );
}
