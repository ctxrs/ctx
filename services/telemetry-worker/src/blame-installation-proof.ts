export const BLAME_INSTALLATION_KEY_HEADER = "x-ctx-installation-key";
export const BLAME_PROOF_NONCE_HEADER = "x-ctx-proof-nonce";
export const BLAME_PROOF_TIME_HEADER = "x-ctx-proof-time";
export const BLAME_PROOF_SIGNATURE_HEADER = "x-ctx-proof-signature";
export const BLAME_PROOF_EVENT_ID_HEADER = "x-idempotency-key";

const KEY_PATTERN = /^[A-Za-z0-9_-]{43}$/u;
const SIGNATURE_PATTERN = /^[A-Za-z0-9_-]{86}$/u;
const TIME_PATTERN = /^(?:0|[1-9][0-9]{0,10})$/u;
const EVENT_ID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u;
const MAX_CLOCK_SKEW_SECONDS = 2 * 60;
const PROOF_DOMAIN = new TextEncoder().encode("ctx\0pro-commercial-request\0v1\0");
const COORDINATE_DOMAIN = new TextEncoder().encode("ctx-blame-analytics-installation-v1\0");

export type BlameInstallationProofInput = Readonly<{
  body: Uint8Array;
  headers: Headers;
  method: string;
  now: Date;
  path: string;
  payload: unknown;
  query: string;
}>;

/** Invalid or absent proof deliberately returns undefined without affecting admission. */
export async function verifiedBlameInstallationCoordinate(
  input: BlameInstallationProofInput,
): Promise<string | undefined> {
  try {
    const publicKeyText = input.headers.get(BLAME_INSTALLATION_KEY_HEADER);
    const nonce = input.headers.get(BLAME_PROOF_NONCE_HEADER);
    const proofTime = input.headers.get(BLAME_PROOF_TIME_HEADER);
    const signatureText = input.headers.get(BLAME_PROOF_SIGNATURE_HEADER);
    const eventId = input.headers.get(BLAME_PROOF_EVENT_ID_HEADER);
    if (
      publicKeyText == null || nonce == null || proofTime == null
      || signatureText == null || eventId == null
      || !KEY_PATTERN.test(publicKeyText) || !KEY_PATTERN.test(nonce)
      || !SIGNATURE_PATTERN.test(signatureText) || !TIME_PATTERN.test(proofTime)
      || !EVENT_ID_PATTERN.test(eventId) || !hasSoleEvent(payloadEvents(input.payload), eventId)
      || input.method !== "POST" || input.path !== "/functions/v1/analytics"
      || input.query !== "" || !Number.isFinite(input.now.getTime())
    ) return undefined;
    const seconds = Number(proofTime);
    const nowSeconds = Math.floor(input.now.getTime() / 1_000);
    if (!Number.isSafeInteger(seconds) || Math.abs(seconds - nowSeconds) > MAX_CLOCK_SKEW_SECONDS) {
      return undefined;
    }
    const publicKey = decodeBase64url(publicKeyText, 32);
    const signature = decodeBase64url(signatureText, 64);
    if (publicKey == null || decodeBase64url(nonce, 32) == null || signature == null) {
      return undefined;
    }
    const imported = await crypto.subtle.importKey(
      "raw",
      arrayBuffer(publicKey),
      { name: "Ed25519" },
      false,
      ["verify"],
    );
    const transcript = await blameInstallationProofTranscript({
      body: input.body,
      eventId,
      method: input.method,
      nonce,
      path: input.path,
      proofTime,
    });
    const valid = await crypto.subtle.verify(
      { name: "Ed25519" },
      imported,
      arrayBuffer(signature),
      arrayBuffer(transcript),
    );
    return valid ? deriveCoordinate(publicKey) : undefined;
  } catch {
    return undefined;
  }
}

export async function blameInstallationProofTranscript(input: Readonly<{
  body: Uint8Array;
  eventId: string;
  method: string;
  nonce: string;
  path: string;
  proofTime: string;
}>): Promise<Uint8Array> {
  const bodyDigest = encodeBase64url(new Uint8Array(
    await crypto.subtle.digest("SHA-256", arrayBuffer(input.body)),
  ));
  const emptyQueryDigest = encodeBase64url(new Uint8Array(
    await crypto.subtle.digest("SHA-256", new ArrayBuffer(0)),
  ));
  return framed([
    input.method,
    input.path,
    bodyDigest,
    emptyQueryDigest,
    input.eventId,
    input.proofTime,
    input.nonce,
  ]);
}

async function deriveCoordinate(publicKey: Uint8Array): Promise<string> {
  const input = new Uint8Array(COORDINATE_DOMAIN.length + publicKey.length);
  input.set(COORDINATE_DOMAIN);
  input.set(publicKey, COORDINATE_DOMAIN.length);
  return encodeBase64url(new Uint8Array(await crypto.subtle.digest("SHA-256", input)));
}

function framed(components: readonly string[]): Uint8Array {
  const encoded = components.map((value) => new TextEncoder().encode(value));
  const output = new Uint8Array(
    PROOF_DOMAIN.length + encoded.reduce((sum, value) => sum + value.length + 1, 0),
  );
  output.set(PROOF_DOMAIN);
  let offset = PROOF_DOMAIN.length;
  for (const value of encoded) {
    output.set(value, offset);
    offset += value.length + 1;
  }
  return output;
}

function payloadEvents(payload: unknown): unknown {
  return isRecord(payload) ? payload.events : undefined;
}

function hasSoleEvent(value: unknown, eventId: string): boolean {
  if (!Array.isArray(value) || value.length !== 1 || !isRecord(value[0])) return false;
  return value[0].event_id === eventId;
}

function decodeBase64url(value: string, length: number): Uint8Array | null {
  try {
    const standard = value.replaceAll("-", "+").replaceAll("_", "/");
    const bytes = Uint8Array.from(
      atob(standard.padEnd(Math.ceil(standard.length / 4) * 4, "=")),
      (character) => character.charCodeAt(0),
    );
    return bytes.length === length && encodeBase64url(bytes) === value ? bytes : null;
  } catch {
    return null;
  }
}

function encodeBase64url(value: Uint8Array): string {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/u, "");
}

function arrayBuffer(value: Uint8Array): ArrayBuffer {
  const output = new ArrayBuffer(value.byteLength);
  new Uint8Array(output).set(value);
  return output;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
