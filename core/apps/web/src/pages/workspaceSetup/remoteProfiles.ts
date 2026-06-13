export type SshRecent = {
  host: string;
  user?: string | null;
  updated_at_ms: number;
};

export type RemoteProfile = {
  host: string;
  user?: string | null;
  remote_port?: number | null;
  remote_data_dir?: string | null;
  updated_at_ms: number;
};

const SSH_RECENTS_KEY = "contextDesktopSshRecentsV1";
const REMOTE_PROFILES_KEY = "contextDesktopRemoteProfilesV1";

export const loadSshRecents = (): SshRecent[] => {
  try {
    const raw = localStorage.getItem(SSH_RECENTS_KEY);
    const parsed = raw ? JSON.parse(raw) : null;
    return Array.isArray(parsed) ? (parsed as SshRecent[]) : [];
  } catch {
    return [];
  }
};

export const saveSshRecents = (recents: SshRecent[]) => {
  try {
    localStorage.setItem(SSH_RECENTS_KEY, JSON.stringify(recents.slice(0, 50)));
  } catch {
    // ignore
  }
};

export const upsertSshRecent = (host: string, user?: string | null): SshRecent[] => {
  const recents = loadSshRecents();
  const key = `${user ?? ""}@${host}`;
  const next = [
    { host, user: user ?? null, updated_at_ms: Date.now() },
    ...recents.filter((entry) => `${entry.user ?? ""}@${entry.host}` !== key),
  ];
  saveSshRecents(next);
  return next;
};

export const remoteProfileKey = (host: string, user?: string | null) => `${user ?? ""}@${host}`;

export const loadRemoteProfiles = (): RemoteProfile[] => {
  try {
    const raw = localStorage.getItem(REMOTE_PROFILES_KEY);
    const parsed = raw ? JSON.parse(raw) : null;
    return Array.isArray(parsed) ? (parsed as RemoteProfile[]) : [];
  } catch {
    return [];
  }
};

export const saveRemoteProfiles = (profiles: RemoteProfile[]) => {
  try {
    localStorage.setItem(REMOTE_PROFILES_KEY, JSON.stringify(profiles.slice(0, 100)));
  } catch {
    // ignore
  }
};

export const upsertRemoteProfile = (
  host: string,
  user: string | null | undefined,
  fields: {
    remote_port?: number | null;
    remote_data_dir?: string | null;
  },
): RemoteProfile[] => {
  const profiles = loadRemoteProfiles();
  const key = remoteProfileKey(host, user ?? null);
  const nextEntry: RemoteProfile = {
    host,
    user: user ?? null,
    remote_port: fields.remote_port ?? null,
    remote_data_dir: fields.remote_data_dir ?? null,
    updated_at_ms: Date.now(),
  };
  const next = [nextEntry, ...profiles.filter((entry) => remoteProfileKey(entry.host, entry.user) !== key)];
  saveRemoteProfiles(next);
  return next;
};

export const parseUserHost = (raw: string): { host: string; user?: string | null } | null => {
  const trimmed = String(raw || "").trim();
  if (!trimmed) return null;
  const at = trimmed.lastIndexOf("@");
  if (at > 0) {
    const user = trimmed.slice(0, at).trim();
    const host = trimmed.slice(at + 1).trim();
    if (!host) return null;
    return { host, user: user || null };
  }
  return { host: trimmed };
};
