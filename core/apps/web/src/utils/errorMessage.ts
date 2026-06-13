export function errorMessage(value: unknown): string {
  if (typeof value === "string") return value;
  if (value instanceof Error) {
    const msg = String(value.message ?? "").trim();
    if (msg) return msg;
  }
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const rec = value as Record<string, unknown>;
    const msg = rec.message;
    if (typeof msg === "string" && msg.trim()) return msg;
    const err = rec.error;
    if (typeof err === "string" && err.trim()) return err;
  }
  try {
    const serialized = JSON.stringify(value);
    if (typeof serialized === "string") return serialized;
  } catch {
    // fall through
  }
  return String(value);
}
