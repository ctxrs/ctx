import React, { useEffect, useMemo, useState } from "react";
import { mintWebSessionStreamPath, type WebSessionInfo } from "../api/client";
import { useDaemonConnection } from "../api/useDaemonConnection";

type SessionSection = {
  key: string;
  label: string;
  sessions: WebSessionInfo[];
};

function sessionLabel(session: WebSessionInfo): string {
  try {
    const url = new URL(session.url);
    const path = url.pathname && url.pathname !== "/" ? url.pathname : "";
    const label = `${url.hostname}${path}`;
    return label.length > 32 ? `${label.slice(0, 28)}…` : label;
  } catch {
    return session.id.slice(0, 8);
  }
}

function buildStreamUrl(
  session: { stream_url?: string | null; stream_path?: string | null },
  baseUrl: string,
): string | null {
  if (session.stream_url) return session.stream_url;
  if (!session.stream_path) return null;
  const base = baseUrl.replace(/\/$/, "");
  const path = session.stream_path.startsWith("/") ? session.stream_path : `/${session.stream_path}`;
  return `${base}${path}`;
}

export function SessionsPane({
  sections,
  activeSection,
  onSectionChange,
  selectedSessionId,
  onSelectSession,
  daemonBaseUrl,
  loading,
}: {
  sections: SessionSection[];
  activeSection: string;
  onSectionChange: (key: string) => void;
  selectedSessionId: string | null;
  onSelectSession: (id: string) => void;
  daemonBaseUrl: string;
  loading?: boolean;
}) {
  const visibleSections = sections.filter((section) => section.sessions.length > 0);
  const active = visibleSections.find((section) => section.key === activeSection) ?? visibleSections[0] ?? null;
  const sessions = active?.sessions ?? [];
  const daemonAuthToken = useDaemonConnection().authToken;

  const selected = useMemo(() => {
    if (!sessions.length) return null;
    if (selectedSessionId) {
      const match = sessions.find((s) => s.id === selectedSessionId);
      if (match) return match;
    }
    return sessions[0];
  }, [sessions, selectedSessionId]);

  const [streamUrl, setStreamUrl] = useState<string | null>(null);

  useEffect(() => {
    if (!selected) {
      setStreamUrl(null);
      return;
    }
    let cancelled = false;
    setStreamUrl(null);
    void mintWebSessionStreamPath(selected.id)
      .then((stream) => {
        if (cancelled) return;
        setStreamUrl(buildStreamUrl(stream, daemonBaseUrl));
      })
      .catch(() => {
        if (cancelled) return;
        setStreamUrl(null);
      });
    return () => {
      cancelled = true;
    };
  }, [daemonAuthToken, daemonBaseUrl, selected?.id]);

  const hasTabs = sessions.length > 1;

  return (
    <div className="wb-sessions">
      <div className="wb-sessions-top">
        <div className="wb-sessions-title">Sessions</div>
        <div className="wb-sessions-meta">
          {visibleSections.length > 1 && (
            <div className="wb-sessions-sections">
              {visibleSections.map((section) => (
                <button
                  key={section.key}
                  type="button"
                  className={`wb-sessions-section ${
                    section.key === active?.key ? "wb-sessions-section-active" : ""
                  }`}
                  onClick={() => onSectionChange(section.key)}
                >
                  {section.label}
                </button>
              ))}
            </div>
          )}
          {active && (
            <div className="wb-sessions-count">
              {active.label} · {active.sessions.length}
            </div>
          )}
        </div>
      </div>

      {loading && sessions.length === 0 ? (
        <div className="wb-sessions-empty">Loading sessions…</div>
      ) : sessions.length === 0 ? (
        <div className="wb-sessions-empty">No sessions available for this run.</div>
      ) : (
        <>
          {hasTabs && (
            <div className="wb-sessions-tabs">
              {sessions.map((session) => (
                <button
                  key={session.id}
                  type="button"
                  className={`wb-sessions-tab ${
                    selected?.id === session.id ? "wb-sessions-tab-active" : ""
                  }`}
                  onClick={() => onSelectSession(session.id)}
                >
                  {sessionLabel(session)}
                </button>
              ))}
            </div>
          )}
          <div className="wb-sessions-body">
            {streamUrl ? (
              <iframe
                className="wb-sessions-frame"
                src={streamUrl}
                allow="autoplay; fullscreen; clipboard-read; clipboard-write"
                allowFullScreen
                title={`session-${selected?.id ?? "stream"}`}
              />
            ) : (
              <div className="wb-sessions-empty">Stream unavailable.</div>
            )}
          </div>
        </>
      )}
    </div>
  );
}
