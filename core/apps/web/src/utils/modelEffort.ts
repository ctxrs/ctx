const PREFERRED_EFFORT_ORDER = ["none", "minimal", "low", "medium", "high", "xhigh"] as const;
export type PreferredEffortId = (typeof PREFERRED_EFFORT_ORDER)[number];

export type ParsedModelId = {
  full: string;
  base: string;
  effort: string | null;
};

function splitOnLastSlash(fullModelId: string): { full: string; base: string; suffix: string | null } {
  const full = String(fullModelId || "").trim();
  if (!full) return { full: "", base: "", suffix: null };
  const idx = full.lastIndexOf("/");
  if (idx <= 0) return { full, base: full, suffix: null };
  const base = full.slice(0, idx);
  const suffix = full.slice(idx + 1);
  return { full, base, suffix: suffix.trim() || null };
}

function normalizeEffortIdForCompare(value: string): string {
  return String(value || "").trim().toLowerCase();
}

function isPreferredEffortId(value: string): value is PreferredEffortId {
  return (PREFERRED_EFFORT_ORDER as readonly string[]).includes(normalizeEffortIdForCompare(value));
}

function hasTrailingParenSuffix(name: string, suffix: string): boolean {
  const trimmed = String(name || "").trim();
  const m = trimmed.match(/\(([^()]+)\)\s*$/);
  if (!m) return false;
  return normalizeEffortIdForCompare(m[1]) === normalizeEffortIdForCompare(suffix);
}

function stripTrailingParenIfEffort(name: string, effortIds: readonly string[]): string {
  const trimmed = String(name || "").trim();
  if (!trimmed) return trimmed;
  const m = trimmed.match(/^(.*)\s*\(([^()]+)\)\s*$/);
  if (!m) return trimmed;
  const suffix = normalizeEffortIdForCompare(m[2]);
  const isEffort = effortIds.some((e) => normalizeEffortIdForCompare(e) === suffix);
  return isEffort ? m[1].trim() : trimmed;
}

function orderEffortIds(efforts: Iterable<string>): string[] {
  const list = [...new Set([...efforts].map((e) => String(e || "").trim()).filter(Boolean))];
  const orderIndex = (v: string) => {
    const norm = normalizeEffortIdForCompare(v);
    const idx = (PREFERRED_EFFORT_ORDER as readonly string[]).indexOf(norm);
    return idx === -1 ? Number.POSITIVE_INFINITY : idx;
  };
  return list.sort((a, b) => {
    const ia = orderIndex(a);
    const ib = orderIndex(b);
    if (ia !== ib) return ia - ib;
    return a.localeCompare(b);
  });
}

export function parseModelId(
  fullModelId: string,
  catalog?: Pick<ModelCatalog, "effortsByBase" | "baseIds">,
): ParsedModelId {
  const { full, base, suffix } = splitOnLastSlash(fullModelId);
  if (!full) return { full: "", base: "", effort: null };
  if (!suffix) return { full, base: full, effort: null };

  if (catalog) {
    const options = catalog.effortsByBase[base] ?? [];
    if (options.includes(suffix)) return { full, base, effort: suffix };
    if (catalog.baseIds.includes(full)) return { full, base: full, effort: null };
  }

  // Conservative fallback: only treat a suffix as "effort" if it looks like a reasoning-effort id.
  if (isPreferredEffortId(suffix)) return { full, base, effort: suffix };
  return { full, base: full, effort: null };
}

export type ModelCatalog = {
  baseIds: string[];
  displayNameByBase: Record<string, string>;
  effortsByBase: Record<string, string[]>;
  fullIdByBaseEffort: Record<string, Record<string, string>>;
};

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

export function buildModelCatalog(
  models: Array<{ id: string; name?: string }> | string[],
): ModelCatalog {
  const list = Array.isArray(models)
    ? models.map((m) => {
      if (typeof m === "string") return { id: m, name: m };
      const rec = asRecord(m);
      return {
        id: String(rec.id ?? ""),
        name: typeof rec.name === "string" ? rec.name : undefined,
      };
    })
    : [];

  const baseIdsSet = new Set<string>();
  const rawEffortsByBase: Record<string, Set<string>> = {};
  const rawNamesByBase: Record<string, string[]> = {};
  const displayNameByBase: Record<string, string> = {};
  const fullIdByBaseEffort: Record<string, Record<string, string>> = {};

  for (const m of list) {
    const id = String(m.id || "").trim();
    if (!id) continue;
    const name = String(m.name ?? id);
    const { base: baseCandidate, suffix } = splitOnLastSlash(id);
    if (!baseCandidate) continue;

    const effort =
      suffix && (isPreferredEffortId(suffix) || hasTrailingParenSuffix(name, suffix)) ? suffix : null;
    const base = effort ? baseCandidate : id;

    baseIdsSet.add(base);
    (rawNamesByBase[base] ??= []).push(name);

    if (effort) {
      (rawEffortsByBase[base] ??= new Set<string>()).add(effort);
      (fullIdByBaseEffort[base] ??= {})[effort] = id;
    }
  }

  const baseIds = [...baseIdsSet].sort((a, b) => a.localeCompare(b));
  const effortsByBase: Record<string, string[]> = {};
  for (const b of baseIds) {
    const efforts = rawEffortsByBase[b] ? orderEffortIds(rawEffortsByBase[b]) : [];
    // Only treat `base/suffix` as an effort dimension if there are multiple variants.
    effortsByBase[b] = efforts.length >= 2 ? efforts : [];

    const names = rawNamesByBase[b] ?? [];
    const stripped = names
      .map((n) => stripTrailingParenIfEffort(n, effortsByBase[b]))
      .map((n) => n.trim())
      .filter(Boolean);
    displayNameByBase[b] = stripped[0] ?? b;
  }

  return { baseIds, displayNameByBase, effortsByBase, fullIdByBaseEffort };
}

export function composeModelId(base: string, effort: string | null): string {
  const b = String(base || "").trim();
  if (!b) return "";
  return effort ? `${b}/${effort}` : b;
}

export function formatEffortLabel(effort: string | null | undefined): string {
  const raw = String(effort ?? "").trim();
  if (!raw) return "";
  const norm = raw.toLowerCase();
  if (norm === "xhigh" || norm === "extra_high" || norm === "extra-high") return "Extra High";
  return norm.charAt(0).toUpperCase() + norm.slice(1);
}
