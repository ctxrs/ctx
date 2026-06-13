const TRUE_VALUES = new Set(["1", "true", "yes", "on"]);
const FALSE_VALUES = new Set(["0", "false", "no", "off"]);

export function parseBoolishString(value?: string | null): boolean | undefined {
  const normalized = String(value ?? "").trim().toLowerCase();
  if (!normalized) return undefined;
  if (TRUE_VALUES.has(normalized)) return true;
  if (FALSE_VALUES.has(normalized)) return false;
  return undefined;
}

export function readBoolish(value: unknown): boolean | undefined {
  if (typeof value === "boolean") return value;
  if (typeof value === "string") return parseBoolishString(value);
  return undefined;
}

export function providerDetailFlag(
  details: Record<string, string> | null | undefined,
  key: string,
): boolean {
  return parseBoolishString(details?.[key]) === true;
}
