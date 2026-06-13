import { idToString, type SessionEvent, type SessionTurn } from "../../api/client";
import { parseIsoMs } from "../sessionView/SessionPage.helpers";

export type AuthMethodOption = { id: string; name: string };
export type SessionErrorInfo = { message: string; provider?: string };
export type ProviderGuardNotice = {
  kind: "provider_guard_warning" | "provider_guard_kill";
  stage: string;
  provider?: string;
  message?: string;
  pid?: number;
  memoryMb?: number | null;
  limitHighMb?: number | null;
  limitMaxMb?: number | null;
  systemTotalMb?: number | null;
  systemUsedMb?: number | null;
  gracePeriodMs?: number | null;
  killAtMs?: number | null;
  createdAtMs?: number | null;
};

export type AuthUi = {
  status: "unknown" | "required" | "failed" | "authenticated";
  provider?: string;
  message?: string;
  methods: AuthMethodOption[];
};

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

function coerceNumber(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim().length > 0) {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

export function deriveAuthUi(events: SessionEvent[]): AuthUi {
  const fromMethodsValue = (value: unknown): AuthMethodOption[] => {
    const list = Array.isArray(value) ? value : [];
    return list
      .map((item): AuthMethodOption | null => {
        const method = asRecord(item);
        const id = readNonEmptyString(method.methodId ?? method.method_id ?? method.id);
        if (!id) return null;
        const name =
          readNonEmptyString(method.name ?? method.label ?? method.methodId ?? method.method_id ?? method.id) ?? id;
        return { id, name };
      })
      .filter((method): method is AuthMethodOption => method !== null);
  };

  let status: AuthUi["status"] = "unknown";
  let provider: string | undefined;
  let message: string | undefined;
  let methods: AuthMethodOption[] = [];

  const lastInit = [...events].reverse().find((e) => e.event_type === "init");
  const initMethods =
    lastInit?.payload_json?.auth_methods ??
    lastInit?.payload_json?.authMethods ??
    lastInit?.payload_json?.auth_methods;
  const initMethodOptions = fromMethodsValue(initMethods);

  for (const ev of events) {
    const payload = asRecord(ev.payload_json);
    if (ev.event_type === "auth_required") {
      status = "required";
      provider = readNonEmptyString(payload.provider) ?? undefined;
      message = readNonEmptyString(payload.message) ?? undefined;
      methods = fromMethodsValue(payload.auth_methods ?? payload.authMethods);
      continue;
    }

    if (ev.event_type !== "notice") continue;
    const kind = payload.kind;
    if (kind === "auth_required") {
      status = "required";
      provider = readNonEmptyString(payload.provider) ?? undefined;
      message = readNonEmptyString(payload.message) ?? undefined;
      methods = fromMethodsValue(payload.auth_methods ?? payload.authMethods);
    }
    if (kind === "auth_failed" || kind === "auth_error") {
      status = "failed";
      provider = readNonEmptyString(payload.provider) ?? undefined;
      message = readNonEmptyString(payload.message) ?? undefined;
    }
    if (
      kind === "auth_finished" ||
      kind === "auth_complete" ||
      kind === "auth_completed" ||
      kind === "auth_success" ||
      kind === "authenticated"
    ) {
      status = "authenticated";
      provider = readNonEmptyString(payload.provider) ?? undefined;
      message = undefined;
      methods = [];
    }
  }

  if ((status === "required" || status === "failed") && methods.length === 0) {
    methods = initMethodOptions;
  }

  return { status, provider, message, methods };
}

export function deriveProviderGuardNotice(events: SessionEvent[]): ProviderGuardNotice | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const ev = events[i];
    if (ev.event_type !== "notice") continue;
    const payload = ev.payload_json ?? {};
    const kind = String(payload.kind ?? "").trim();
    if (kind !== "provider_guard_warning" && kind !== "provider_guard_kill") continue;
    return {
      kind,
      stage: String(payload.stage ?? "").trim(),
      provider: typeof payload.provider === "string" ? payload.provider : undefined,
      message: typeof payload.message === "string" ? payload.message : undefined,
      pid: coerceNumber(payload.pid) ?? undefined,
      memoryMb: coerceNumber(payload.memory_mb),
      limitHighMb: coerceNumber(payload.limit_high_mb),
      limitMaxMb: coerceNumber(payload.limit_max_mb),
      systemTotalMb: coerceNumber(payload.system_total_mb),
      systemUsedMb: coerceNumber(payload.system_used_mb),
      gracePeriodMs: coerceNumber(payload.grace_period_ms),
      killAtMs: coerceNumber(payload.kill_at_ms),
      createdAtMs: parseIsoMs(ev.created_at),
    };
  }
  return null;
}

function readNonEmptyString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const text = value.trim();
  return text ? text : null;
}

function extractErrorDetails(payload: unknown): string | null {
  const record = asRecord(payload);
  if (!record) return null;
  const direct =
    readNonEmptyString(record.details) ??
    readNonEmptyString(record.detail) ??
    readNonEmptyString(record.additional_details) ??
    readNonEmptyString(record.additionalDetails);
  if (direct) return direct;

  const codexInfo = record.codex_error_info ?? record.codexErrorInfo;
  const codexText = extractErrorMessageFromObject(codexInfo);
  if (codexText) return codexText;

  const kind = readNonEmptyString(record.kind);
  return kind;
}

export function extractErrorMessage(payload: unknown): string | null {
  const payloadText = readNonEmptyString(payload);
  if (payloadText) return payloadText;

  const record = asRecord(payload);
  if (!record) return null;
  const details = extractErrorDetails(payload);
  const direct =
    readNonEmptyString(record.message) ??
    readNonEmptyString(record.error) ??
    readNonEmptyString(record.error_message) ??
    readNonEmptyString(record.errorMessage);
  if (direct) {
    if (details && !direct.includes(details)) {
      return `${direct}\nDetails: ${details}`;
    }
    return direct;
  }

  const update = record.update ?? record;
  const updateText = extractErrorMessageFromObject(update);
  if (updateText) {
    if (details && !updateText.includes(details)) {
      return `${updateText}\nDetails: ${details}`;
    }
    return updateText;
  }

  const updateRecord = asRecord(update);
  const meta = updateRecord?._meta ?? updateRecord?.meta ?? record._meta ?? record.meta ?? null;
  const metaRecord = asRecord(meta);
  const metaText =
    readNonEmptyString(metaRecord?.statusText) ??
    readNonEmptyString(metaRecord?.status_text) ??
    readNonEmptyString(metaRecord?.message) ??
    readNonEmptyString(metaRecord?.error);
  if (metaText) {
    if (details && !metaText.includes(details)) {
      return `${metaText}\nDetails: ${details}`;
    }
    return metaText;
  }
  return details;
}

function extractErrorMessageFromObject(value: unknown): string | null {
  if (!value) return null;
  if (typeof value === "string") return readNonEmptyString(value);
  const record = asRecord(value);
  if (!record) return null;

  const direct =
    readNonEmptyString(record.message) ??
    readNonEmptyString(record.error_message) ??
    readNonEmptyString(record.errorMessage);
  if (direct) return direct;

  const data = record.data ?? record.details ?? record.detail;
  const dataRecord = asRecord(data);
  const dataText =
    readNonEmptyString(data) ??
    readNonEmptyString(dataRecord?.message) ??
    readNonEmptyString(dataRecord?.error);
  if (dataText) return dataText;

  const nested = record.error ?? record.cause;
  const nestedText =
    typeof nested === "object"
      ? extractErrorMessageFromObject(nested)
      : readNonEmptyString(nested);
  if (nestedText) return nestedText;

  const meta = asRecord(record._meta ?? record.meta);
  return readNonEmptyString(meta?.statusText) ?? readNonEmptyString(meta?.status_text);
}

export function deriveSessionError(
  turns: SessionTurn[],
  events: SessionEvent[],
): SessionErrorInfo | null {
  if (turns.length === 0) return null;
  const lastTurn = turns[turns.length - 1];
  if (lastTurn.status !== "failed") return null;
  if (lastTurn.failure) {
    return {
      message: extractErrorMessage(lastTurn.failure) ?? "Harness error.",
      provider:
        readNonEmptyString(lastTurn.failure.provider) ??
        readNonEmptyString(lastTurn.failure.provider_id) ??
        undefined,
    };
  }
  const turnId = idToString(lastTurn.turn_id);
  let failureEvent: SessionEvent | null = null;
  for (let i = events.length - 1; i >= 0; i--) {
    const ev = events[i];
    if (ev.event_type !== "turn_finished") continue;
    if (turnId && idToString(ev.turn_id) !== turnId) continue;
    const status = readNonEmptyString(ev.payload_json?.status);
    if (status !== "failed") continue;
    failureEvent = ev;
    break;
  }
  if (!failureEvent) {
    return { message: "Harness error." };
  }
  const message = extractErrorMessage(failureEvent.payload_json) ?? "Harness error.";
  const provider =
    readNonEmptyString(failureEvent.payload_json?.provider) ??
    readNonEmptyString(failureEvent.payload_json?.provider_id) ??
    readNonEmptyString(failureEvent.payload_json?.providerId) ??
    undefined;
  return { message, provider };
}
