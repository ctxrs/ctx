import { buildModelCatalog, parseModelId } from "../../utils/modelEffort";

export const SCROLLBACK_INCREASE_VIEWPORT_BY_PX = 240;

export function buildModelsFromAcpMeta(models: unknown): Array<{ id: string; name?: string }> {
  if (!models || typeof models !== "object") return [];
  const list = "models" in models ? models.models : undefined;
  if (!Array.isArray(list)) return [];
  const parsed: Array<{ id: string; name?: string }> = [];
  for (const item of list) {
    if (!item || typeof item !== "object") continue;
    const id = "id" in item && typeof item.id === "string" ? item.id : "";
    if (!id) continue;
    const name = "name" in item && typeof item.name === "string" ? item.name : undefined;
    parsed.push(name ? { id, name } : { id });
  }
  return parsed;
}

function humanizeConcreteClaudeModelBase(baseModelId: string): string | undefined {
  const trimmed = String(baseModelId || "").trim();
  if (!trimmed) return undefined;
  if (trimmed === "default") return "Default";
  const aliasMatch = trimmed.match(/^(opus|sonnet|haiku)$/i);
  if (aliasMatch) {
    const family = aliasMatch[1].toLowerCase();
    return family.charAt(0).toUpperCase() + family.slice(1);
  }
  const slugMatch = trimmed.match(/(?:^|\/)claude-(opus|sonnet|haiku)-(\d+)-(\d+)(?:[-@]\d+)?$/i);
  if (!slugMatch) return undefined;
  const family = slugMatch[1].toLowerCase();
  return `${family.charAt(0).toUpperCase() + family.slice(1)} ${slugMatch[2]}.${slugMatch[3]}`;
}

export function resolveModelDisplayLabel(
  models: Array<{ id: string; name?: string }>,
  candidates: Array<string | null | undefined>,
): string {
  const catalog = buildModelCatalog(models);
  for (const candidate of candidates) {
    const raw = String(candidate ?? "").trim();
    if (!raw) continue;
    const parsed = parseModelId(raw, catalog);
    const fromCatalog = catalog.displayNameByBase[parsed.base];
    if (fromCatalog) return fromCatalog;
    const humanized = humanizeConcreteClaudeModelBase(parsed.base || raw);
    if (humanized) return humanized;
    return parsed.base || raw;
  }
  return "";
}

export function formatMemoryMb(value?: number | null): string {
  if (!Number.isFinite(value)) return "—";
  const mb = value as number;
  const gb = mb / 1024;
  if (gb >= 1) {
    const precision = gb >= 10 ? 0 : 1;
    return `${gb.toFixed(precision)} GB`;
  }
  return `${Math.round(mb)} MB`;
}

export function setBooleanStateRef(
  ref: { current: boolean },
  setState: (next: boolean) => void,
  next: boolean,
): void {
  ref.current = next;
  setState(next);
}
