import type { APIRequestContext } from "playwright/test";

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const readString = (value: unknown): string => (typeof value === "string" ? value : "");

const normalizeErrorMessage = (raw: string): string => raw.replace(/\s+/g, " ").trim();

const firstText = (...values: unknown[]): string => {
  for (const value of values) {
    const text = readString(value).trim();
    if (text) return text;
  }
  return "";
};

export async function ensureLocalLinuxSandboxPrepared(
  request: APIRequestContext,
): Promise<void> {
  const prepare = await request.post("/api/execution/linux_sandbox_runtime/prepare", {
    data: {
      activation_mode: "local",
      sudo_password: null,
    },
  });
  if (!prepare.ok()) {
    throw new Error(
      normalizeErrorMessage(
        `linux sandbox prepare failed (${prepare.status()}): ${await prepare.text().catch(() => "")}`,
      ),
    );
  }

  const result = asRecord(await prepare.json().catch(() => ({})));
  if (result.ready === true) {
    return;
  }
  if (result.needs_password === true) {
    throw new Error(
      "linux sandbox prepare requires admin password; CI/E2E runners must pre-provision or allow passwordless activation",
    );
  }

  const detail = normalizeErrorMessage(
    firstText(
      result.message,
      asRecord(result.status).message,
      "linux sandbox prepare did not report ready",
    ),
  );
  throw new Error(detail);
}
