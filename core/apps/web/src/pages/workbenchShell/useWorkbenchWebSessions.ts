import { useCallback, useEffect, useMemo, useState } from "react";

import { listWebSessions, type WebSessionInfo } from "../../api/client";
import { useDaemonBaseUrl } from "../../api/useDaemonConnection";

type SessionSection = {
  key: string;
  label: string;
  sessions: WebSessionInfo[];
};

export function useWorkbenchWebSessions(activeSessionId: string | null) {
  const daemonBaseUrl = useDaemonBaseUrl() ?? "";
  const [webSessions, setWebSessions] = useState<WebSessionInfo[]>([]);
  const [webSessionsLoading, setWebSessionsLoading] = useState(false);
  const [activeWebSessionId, setActiveWebSessionId] = useState<string | null>(null);
  const [activeSessionKind, setActiveSessionKind] = useState("web");
  const webSessionsEnabled = false;

  const refreshWebSessions = useCallback(async () => {
    if (!activeSessionId) {
      setWebSessions([]);
      setWebSessionsLoading(false);
      return;
    }
    setWebSessionsLoading(true);
    try {
      const sessions = await listWebSessions();
      const filtered = sessions.filter(
        (session) =>
          session.session_id === activeSessionId && String(session.status).toLowerCase() === "running",
      );
      setWebSessions(filtered);
    } catch {
      setWebSessions([]);
    } finally {
      setWebSessionsLoading(false);
    }
  }, [activeSessionId]);

  useEffect(() => {
    if (!webSessionsEnabled) return;
    let cancelled = false;
    const run = async () => {
      if (cancelled) return;
      await refreshWebSessions();
    };
    void run();
    if (!activeSessionId) return () => {};
    const timer = window.setInterval(() => void run(), 10000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [activeSessionId, refreshWebSessions, webSessionsEnabled]);

  useEffect(() => {
    if (webSessions.length === 0) {
      setActiveWebSessionId(null);
      return;
    }
    if (!activeWebSessionId || !webSessions.some((session) => session.id === activeWebSessionId)) {
      setActiveWebSessionId(webSessions[0].id);
    }
  }, [activeWebSessionId, webSessions]);

  const sessionSections = useMemo<SessionSection[]>(() => {
    if (!webSessionsEnabled) return [];
    return [
      {
        key: "web",
        label: "Web Sessions",
        sessions: webSessions,
      },
    ];
  }, [webSessions, webSessionsEnabled]);

  useEffect(() => {
    if (!sessionSections.length) return;
    if (!sessionSections.some((section) => section.key === activeSessionKind)) {
      setActiveSessionKind(sessionSections[0].key);
    }
  }, [activeSessionKind, sessionSections]);

  return {
    activeWebSessionId,
    setActiveWebSessionId,
    activeSessionKind,
    setActiveSessionKind,
    daemonBaseUrl,
    webSessionsEnabled,
    webSessionsLoading,
    sessionSections,
  };
}
