import { randomUuid } from "../randomUuid";

const INSTALL_ID_KEY = "ctx-install-id";

const buildFallbackInstallId = (): string => `anon-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;

export const getInstallId = (): string => {
  if (typeof window === "undefined") return "server";
  try {
    const existing = window.localStorage.getItem(INSTALL_ID_KEY);
    if (existing) return existing;
    const next = randomUuid();
    window.localStorage.setItem(INSTALL_ID_KEY, next);
    return next;
  } catch {
    try {
      return randomUuid();
    } catch {
      return buildFallbackInstallId();
    }
  }
};
