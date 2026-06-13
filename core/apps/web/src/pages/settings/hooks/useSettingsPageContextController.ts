import { useCallback, useEffect, useMemo, useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { idToString, listWorkspaces, type Workspace } from "../../../api/client";
import { SECTIONS } from "../SettingsPage.constants";
import type { SectionId, SettingsSectionMeta } from "../SettingsPage.types";
import { sectionFromHash } from "../SettingsPage.utils";

type SettingsBackLink = {
  to: string;
  label: string;
};

type SettingsPageContextController = {
  active: SectionId;
  query: string;
  setQuery: (value: string) => void;
  sidebarSections: SettingsSectionMeta[];
  headerLabel: string;
  onSectionChange: (nextSection: SectionId) => void;
  backLink: SettingsBackLink;
  workspaceId: string | null;
  workspaces: Workspace[];
  billingReturnPath: string;
  checkoutStatus: string | null;
  checkoutSessionId: string | null;
  clearCheckoutStatus: () => void;
  devToolsEnabled: boolean;
};

const firstWorkspaceId = (workspaces: Workspace[]): string | null => {
  const first = workspaces[0];
  if (!first) return null;
  return idToString((first as { id?: string | null }).id);
};

export function useSettingsPageContextController(): SettingsPageContextController {
  const location = useLocation();
  const navigate = useNavigate();
  const [active, setActive] = useState<SectionId>(() => sectionFromHash(window.location.hash) ?? "general");
  const [query, setQuery] = useState("");
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [workspaceId, setWorkspaceId] = useState<string | null>(null);
  const devToolsEnabled = import.meta.env.DEV;

  const checkoutStatus = useMemo(
    () => new URLSearchParams(location.search).get("checkout"),
    [location.search],
  );
  const checkoutSessionId = useMemo(
    () => new URLSearchParams(location.search).get("session_id"),
    [location.search],
  );

  const billingReturnPath = useMemo(() => {
    const params = new URLSearchParams(location.search);
    params.delete("checkout");
    params.delete("session_id");
    const search = params.toString();
    return `${location.pathname}${search ? `?${search}` : ""}#general`;
  }, [location.pathname, location.search]);

  const clearCheckoutStatus = useCallback(() => {
    const params = new URLSearchParams(location.search);
    if (!params.has("checkout") && !params.has("session_id")) return;
    params.delete("checkout");
    params.delete("session_id");
    const nextSearch = params.toString();
    navigate(
      {
        pathname: location.pathname,
        search: nextSearch ? `?${nextSearch}` : "",
        hash: location.hash,
      },
      { replace: true },
    );
  }, [location.hash, location.pathname, location.search, navigate]);

  useEffect(() => {
    const onHash = () => {
      const next = sectionFromHash(window.location.hash);
      if (next) setActive(next);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  useEffect(() => {
    let cancelled = false;

    listWorkspaces()
      .then((nextWorkspaces) => {
        if (cancelled) return;
        setWorkspaces(nextWorkspaces);
        setWorkspaceId((currentWorkspaceId) => currentWorkspaceId ?? firstWorkspaceId(nextWorkspaces));
      })
      .catch(() => {});

    return () => {
      cancelled = true;
    };
  }, []);

  const workspaceFromQuery = useMemo(() => {
    const value = new URLSearchParams(location.search).get("ws");
    if (!value) return null;
    const trimmed = value.trim();
    return trimmed ? trimmed : null;
  }, [location.search]);

  useEffect(() => {
    if (workspaceFromQuery) {
      if (workspaceId !== workspaceFromQuery) {
        setWorkspaceId(workspaceFromQuery);
      }
      return;
    }

    if (!workspaces.length) return;
    if (!workspaceId) {
      setWorkspaceId(firstWorkspaceId(workspaces));
      return;
    }

    const hasWorkspace = workspaces.some(
      (workspace) => idToString((workspace as { id?: string | null }).id) === workspaceId,
    );
    if (!hasWorkspace) {
      setWorkspaceId(firstWorkspaceId(workspaces));
    }
  }, [workspaceFromQuery, workspaceId, workspaces]);

  const sidebarSections = useMemo<SettingsSectionMeta[]>(() => {
    const normalizedQuery = query.trim().toLowerCase();
    const allSections = SECTIONS.filter(
      (section) => !section.navHidden && (devToolsEnabled || section.id !== "dev_tools"),
    );
    if (!normalizedQuery) return allSections;
    return allSections.filter((section) => section.label.toLowerCase().includes(normalizedQuery));
  }, [devToolsEnabled, query]);

  const headerLabel = useMemo(
    () => SECTIONS.find((section) => section.id === active)?.label ?? "Settings",
    [active],
  );

  const onSectionChange = useCallback(
    (nextSection: SectionId) => {
      if (nextSection === active) return;
      setActive(nextSection);
      window.location.hash = nextSection;
    },
    [active],
  );

  const backLink = useMemo<SettingsBackLink>(() => {
    const value = new URLSearchParams(location.search).get("ws");
    if (value && value.trim()) {
      const id = value.trim();
      return { to: `/workspaces/${encodeURIComponent(id)}`, label: "← Back to Workspace" };
    }
    return { to: "/", label: "← Back to Home" };
  }, [location.search]);

  return {
    active,
    query,
    setQuery,
    sidebarSections,
    headerLabel,
    onSectionChange,
    backLink,
    workspaceId,
    workspaces,
    billingReturnPath,
    checkoutStatus,
    checkoutSessionId,
    clearCheckoutStatus,
    devToolsEnabled,
  };
}

export type { SettingsBackLink, SettingsPageContextController };
