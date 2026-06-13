import type { UpdateCheck } from "../../api/client";

const SEMVER_RE =
  /^v?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

type ParsedSemVer = {
  major: number;
  minor: number;
  patch: number;
  prerelease: string[];
};

export const normalizeOptionalString = (value: string | null | undefined): string =>
  String(value ?? "").trim();

export const messageFromUnknownError = (err: unknown, fallback: string): string => {
  if (err instanceof Error) {
    const message = String(err.message ?? "").trim();
    return message || fallback;
  }
  if (typeof err === "string") {
    const message = err.trim();
    return message || fallback;
  }
  if (err && typeof err === "object") {
    const withMessage = err as { message?: unknown };
    if (typeof withMessage.message === "string") {
      const message = withMessage.message.trim();
      if (message) return message;
    }
    try {
      const encoded = JSON.stringify(err);
      if (typeof encoded === "string" && encoded.trim()) return encoded;
    } catch {
      // ignore serialization failures
    }
  }
  return fallback;
};

export const deriveBaseUrlFromEndpoint = (endpoint: string): string => {
  const trimmed = String(endpoint ?? "").trim();
  if (!trimmed) return "";
  try {
    const url = new URL(trimmed);
    const marker = "/releases/";
    const idx = url.pathname.indexOf(marker);
    if (idx >= 0) {
      url.pathname = url.pathname.slice(0, idx);
    } else {
      url.pathname = "";
    }
    url.search = "";
    url.hash = "";
    return `${url.origin}${url.pathname}`.replace(/\/+$/, "");
  } catch {
    return "";
  }
};

export const areUpdateChecksEqual = (
  left: UpdateCheck | null,
  right: UpdateCheck | null,
): boolean => {
  if (left === right) return true;
  if (!left || !right) return false;
  return (
    left.channel === right.channel &&
    left.base_url === right.base_url &&
    normalizeOptionalString(left.platform) === normalizeOptionalString(right.platform) &&
    left.current_version === right.current_version &&
    normalizeOptionalString(left.latest_version) === normalizeOptionalString(right.latest_version) &&
    normalizeOptionalString(left.min_supported_version) ===
      normalizeOptionalString(right.min_supported_version) &&
    left.update_available === right.update_available &&
    (left.platform_supported ?? null) === (right.platform_supported ?? null) &&
    (left.in_place_update_supported ?? null) ===
      (right.in_place_update_supported ?? null) &&
    normalizeOptionalString(left.in_place_update_reason) ===
      normalizeOptionalString(right.in_place_update_reason)
  );
};

const parseSemVer = (value: string): ParsedSemVer | null => {
  const trimmed = String(value || "").trim();
  const match = trimmed.match(SEMVER_RE);
  if (!match) return null;
  const prerelease = match[4] ? match[4].split(".") : [];
  return {
    major: Number(match[1]),
    minor: Number(match[2]),
    patch: Number(match[3]),
    prerelease,
  };
};

export const getInPlaceCapability = (
  info: UpdateCheck | null,
): { supported: boolean; reason: string } => {
  if (!info) return { supported: false, reason: "" };
  const reason = normalizeOptionalString(info.in_place_update_reason);
  return {
    supported: info.in_place_update_supported === true,
    reason,
  };
};

const isNumericIdentifier = (value: string): boolean => /^[0-9]+$/.test(value);

const comparePrereleaseIdentifier = (left: string, right: string): number => {
  const leftNumeric = isNumericIdentifier(left);
  const rightNumeric = isNumericIdentifier(right);
  if (leftNumeric && rightNumeric) {
    const leftNum = Number(left);
    const rightNum = Number(right);
    if (leftNum < rightNum) return -1;
    if (leftNum > rightNum) return 1;
    return 0;
  }
  if (leftNumeric && !rightNumeric) return -1;
  if (!leftNumeric && rightNumeric) return 1;
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
};

const compareVersions = (left: string, right: string): number | null => {
  const leftVer = parseSemVer(left);
  const rightVer = parseSemVer(right);
  if (!leftVer || !rightVer) return null;
  if (leftVer.major !== rightVer.major) return leftVer.major < rightVer.major ? -1 : 1;
  if (leftVer.minor !== rightVer.minor) return leftVer.minor < rightVer.minor ? -1 : 1;
  if (leftVer.patch !== rightVer.patch) return leftVer.patch < rightVer.patch ? -1 : 1;

  const leftPre = leftVer.prerelease;
  const rightPre = rightVer.prerelease;
  if (leftPre.length === 0 && rightPre.length === 0) return 0;
  if (leftPre.length === 0) return 1;
  if (rightPre.length === 0) return -1;

  const len = Math.max(leftPre.length, rightPre.length);
  for (let i = 0; i < len; i += 1) {
    const a = leftPre[i];
    const b = rightPre[i];
    if (a === undefined) return -1;
    if (b === undefined) return 1;
    const cmp = comparePrereleaseIdentifier(a, b);
    if (cmp !== 0) return cmp;
  }
  return 0;
};

export const isForcedUpdate = (info: UpdateCheck | null): boolean => {
  if (!info) return false;
  if (!info.update_available) return false;
  if (info.platform_supported === false) return false;
  const current = String(info.current_version ?? "").trim();
  const minimum = String(info.min_supported_version ?? "").trim();
  const latest = String(info.latest_version ?? "").trim();
  if (!latest) return false;
  if (!current || !minimum) return false;
  return compareVersions(current, minimum) === -1;
};

export const isCurrentVersionAtOrAbove = (
  currentVersion: string,
  requiredVersion: string,
): boolean => {
  const cmp = compareVersions(currentVersion, requiredVersion);
  if (cmp === null) return currentVersion === requiredVersion;
  return cmp >= 0;
};
