export const WORKER_AUTH_HEADER = "x-ctx-worker-auth";

export async function readWorkerAuthSecret() {
  const chunks = [];
  for await (const chunk of process.stdin) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
    if (Buffer.concat(chunks).includes(0x0a)) break;
  }
  const raw = Buffer.concat(chunks).toString("utf-8");
  const secret = raw.split(/\r?\n/, 1)[0]?.trim() ?? "";
  if (!secret) {
    throw new Error("missing worker auth secret on stdin");
  }
  return secret;
}

export function isWorkerAuthValid(headers, expectedSecret) {
  if (!expectedSecret) return false;
  const value = headers?.[WORKER_AUTH_HEADER];
  if (Array.isArray(value)) {
    return value.includes(expectedSecret);
  }
  return value === expectedSecret;
}
