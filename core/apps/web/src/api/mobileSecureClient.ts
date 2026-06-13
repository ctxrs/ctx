import { invoke } from "../utils/desktopCore";
import { createBrowserDaemonTargetScope } from "../state/scopeIdentity";
import {
  deriveDaemonWsBaseUrl,
  getDaemonConnection,
  isMobileSecureConnection,
  normalizeDaemonBaseUrl,
  setDaemonConnection,
} from "./daemonConnection";
import type { DaemonConnection, MobileSecureConnection } from "./daemonConnection.types";

export const PAIRING_REQUEST_ENCRYPTION =
  "x25519-hkdf-sha256-xchacha20poly1305-v1";

type MobilePairingQrPayload = {
  type: "context_mobile_e2ee";
  version: 1;
  tunnelId: string;
  baseUrl: string;
  pairingToken: string;
  daemonPublicKey: string;
  pairingRequestEncryption: typeof PAIRING_REQUEST_ENCRYPTION;
};

type TauriPairingEnvelope = {
  deviceId: string;
  publicKey: string;
  seq: number;
  nonce: string;
  ciphertext: string;
};

type ServerSecureEnvelope = {
  device_id: string;
  seq: number;
  nonce: string;
  ciphertext: string;
};

type TauriSecureEnvelope = {
  deviceId: string;
  seq: number;
  nonce: string;
  ciphertext: string;
};

type PreparePairingResponse = {
  deviceId: string;
  publicKey: string;
  envelope: TauriPairingEnvelope;
};

type PairingResponsePayload = {
  paired: boolean;
  deviceId: string;
  daemonPublicKey: string;
  pairedAt?: string | null;
};

type SecureRequestPayload = {
  method: string;
  path: string;
  query: string | null;
  headers: Array<[string, string]>;
  bodyB64: string;
};

type SecureResponsePayload = {
  status: number;
  headers: Array<[string, string]>;
  bodyB64: string;
};

type DeriveStreamTokenResponse = {
  deviceId: string;
  token: string;
};

type DecryptEnvelopeResponse = {
  plaintextB64: string;
};

export type DaemonRawResponse = {
  status: number;
  body: string;
  content_type: string;
};

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

const isRecord = (value: unknown): value is Record<string, unknown> =>
  Boolean(value) && typeof value === "object";

const asString = (value: unknown): string | null =>
  typeof value === "string" && value.trim() ? value.trim() : null;

const toBase64 = (input: string): string => {
  const bytes = textEncoder.encode(input);
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
};

const fromBase64 = (input: string): string => {
  const binary = atob(input);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return textDecoder.decode(bytes);
};

export const parseMobilePairingQrPayload = (input: string): MobilePairingQrPayload => {
  let parsed: unknown;
  try {
    parsed = JSON.parse(input);
  } catch {
    throw new Error("Paste a valid mobile pairing QR payload.");
  }
  if (!isRecord(parsed)) {
    throw new Error("Mobile pairing QR payload must be a JSON object.");
  }
  if (parsed.type !== "context_mobile_e2ee" || parsed.version !== 1) {
    throw new Error("Unsupported mobile pairing QR payload.");
  }
  const tunnelId = asString(parsed.tunnel_id);
  const baseUrl = normalizeDaemonBaseUrl(asString(parsed.base_url));
  const pairingToken = asString(parsed.pairing_token);
  const daemonPublicKey = asString(parsed.daemon_public_key);
  const pairingRequestEncryption = asString(parsed.pairing_request_encryption);
  if (!tunnelId || !baseUrl || !pairingToken || !daemonPublicKey) {
    throw new Error("Mobile pairing QR payload is missing required fields.");
  }
  if (new URL(baseUrl).protocol !== "https:") {
    throw new Error("Mobile pairing QR payload must use HTTPS.");
  }
  if (pairingRequestEncryption !== PAIRING_REQUEST_ENCRYPTION) {
    throw new Error("Unsupported mobile pairing encryption.");
  }
  return {
    type: "context_mobile_e2ee",
    version: 1,
    tunnelId,
    baseUrl,
    pairingToken,
    daemonPublicKey,
    pairingRequestEncryption,
  };
};

const toServerPairingEnvelope = (envelope: TauriPairingEnvelope): Record<string, unknown> => ({
  device_id: envelope.deviceId,
  public_key: envelope.publicKey,
  seq: envelope.seq,
  nonce: envelope.nonce,
  ciphertext: envelope.ciphertext,
});

const toTauriEnvelope = (envelope: ServerSecureEnvelope): TauriSecureEnvelope => ({
  deviceId: envelope.device_id,
  seq: envelope.seq,
  nonce: envelope.nonce,
  ciphertext: envelope.ciphertext,
});

const readServerEnvelope = (value: unknown): ServerSecureEnvelope => {
  if (!isRecord(value)) throw new Error("Expected secure envelope response.");
  const deviceId = asString(value.device_id);
  const nonce = asString(value.nonce);
  const ciphertext = asString(value.ciphertext);
  const seq = value.seq;
  if (!deviceId || !nonce || !ciphertext || typeof seq !== "number") {
    throw new Error("Invalid secure envelope response.");
  }
  return {
    device_id: deviceId,
    seq,
    nonce,
    ciphertext,
  };
};

const secureFetchUrl = (baseUrl: string, path: string): string =>
  `${baseUrl.replace(/\/+$/, "")}${path.startsWith("/") ? path : `/${path}`}`;

const requestBodyToString = async (body: BodyInit | null | undefined): Promise<string> => {
  if (body === undefined || body === null) return "";
  if (typeof body === "string") return body;
  if (body instanceof URLSearchParams) return body.toString();
  if (body instanceof Blob) return body.text();
  if (body instanceof ArrayBuffer) {
    return textDecoder.decode(new Uint8Array(body));
  }
  if (ArrayBuffer.isView(body)) {
    return textDecoder.decode(new Uint8Array(body.buffer, body.byteOffset, body.byteLength));
  }
  throw new Error("Managed mobile secure requests do not support streaming request bodies.");
};

const requestHeadersToPairs = (headers: HeadersInit | undefined): Array<[string, string]> => {
  const normalized = new Headers(headers);
  normalized.delete("authorization");
  return Array.from(normalized.entries());
};

const currentManagedConnection = (): {
  baseUrl: string;
  secure: MobileSecureConnection;
  connection: DaemonConnection;
} => {
  const connection = getDaemonConnection();
  if (!connection.baseUrl || !isMobileSecureConnection(connection.mobileSecure)) {
    throw new Error("Managed mobile tunnel connection is not configured.");
  }
  return {
    baseUrl: connection.baseUrl,
    secure: connection.mobileSecure,
    connection,
  };
};

const reserveSecureSequence = (
  connection: DaemonConnection,
  secure: MobileSecureConnection,
): number => {
  const seq = secure.nextSeq;
  setDaemonConnection(
    {
      mobileSecure: {
        ...secure,
        nextSeq: seq + 1,
      },
    },
    { persistAuthToken: true },
  );
  return seq;
};

export const pairManagedMobileQrPayload = async (
  input: string,
): Promise<DaemonConnection> => {
  const qr = parseMobilePairingQrPayload(input);
  const prepared = await invoke<PreparePairingResponse>("mobile_prepare_pairing_request", {
    req: {
      pairingToken: qr.pairingToken,
      daemonPublicKey: qr.daemonPublicKey,
      deviceLabel: "iPhone",
      platform: "ios",
      appVersion: null,
    },
  });
  const pairResp = await fetch(secureFetchUrl(qr.baseUrl, "/api/mobile/pair"), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(toServerPairingEnvelope(prepared.envelope)),
  });
  if (!pairResp.ok) {
    throw new Error((await pairResp.text()) || `Pairing failed with ${pairResp.status}.`);
  }
  const pairEnvelope = readServerEnvelope(await pairResp.json());
  const pairPayload = await invoke<PairingResponsePayload>("mobile_decrypt_pairing_response", {
    req: {
      daemonPublicKey: qr.daemonPublicKey,
      envelope: toTauriEnvelope(pairEnvelope),
    },
  });
  if (!pairPayload.paired || pairPayload.deviceId !== prepared.deviceId) {
    throw new Error("Pairing response did not confirm this device.");
  }
  if (pairPayload.daemonPublicKey !== qr.daemonPublicKey) {
    throw new Error("Pairing response daemon key does not match the QR payload.");
  }
  return setDaemonConnection(
    {
      baseUrl: qr.baseUrl,
      wsBaseUrl: deriveDaemonWsBaseUrl(qr.baseUrl),
      authToken: null,
      source: "mobile_managed_qr",
      targetScope: createBrowserDaemonTargetScope(qr.baseUrl),
      mobileSecure: {
        kind: "managed_tunnel",
        deviceId: prepared.deviceId,
        daemonPublicKey: qr.daemonPublicKey,
        pairingRequestEncryption: qr.pairingRequestEncryption,
        nextSeq: 1,
      },
    },
    { persistBaseUrl: true, persistAuthToken: true },
  );
};

export const mobileSecureFetchRaw = async (
  path: string,
  init?: RequestInit,
): Promise<DaemonRawResponse> => {
  const { baseUrl, secure, connection } = currentManagedConnection();
  const method = init?.method ? String(init.method) : "GET";
  const body = await requestBodyToString(init?.body);
  const seq = reserveSecureSequence(connection, secure);
  const payload: SecureRequestPayload = {
    method,
    path,
    query: null,
    headers: requestHeadersToPairs(init?.headers),
    bodyB64: toBase64(body),
  };
  const encrypted = await invoke<TauriSecureEnvelope>("mobile_encrypt_secure_request", {
    req: {
      daemonPublicKey: secure.daemonPublicKey,
      seq,
      payload,
    },
  });
  const resp = await fetch(secureFetchUrl(baseUrl, "/api/mobile/secure"), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      device_id: encrypted.deviceId,
      seq: encrypted.seq,
      nonce: encrypted.nonce,
      ciphertext: encrypted.ciphertext,
    }),
  });
  if (!resp.ok) {
    throw new Error((await resp.text()) || `Secure mobile request failed with ${resp.status}.`);
  }
  const envelope = readServerEnvelope(await resp.json());
  const decrypted = await invoke<SecureResponsePayload>("mobile_decrypt_secure_response", {
    req: {
      daemonPublicKey: secure.daemonPublicKey,
      envelope: toTauriEnvelope(envelope),
    },
  });
  const headers = new Headers(decrypted.headers);
  return {
    status: decrypted.status,
    body: fromBase64(decrypted.bodyB64),
    content_type: headers.get("content-type") ?? "",
  };
};

export const deriveManagedMobileStreamQuery = async (
  workspaceId: string,
): Promise<URLSearchParams | null> => {
  const connection = getDaemonConnection();
  if (!isMobileSecureConnection(connection.mobileSecure)) return null;
  const resp = await invoke<DeriveStreamTokenResponse>("mobile_derive_stream_token", {
    req: {
      daemonPublicKey: connection.mobileSecure.daemonPublicKey,
      workspaceId,
    },
  });
  const query = new URLSearchParams();
  query.set("device_id", resp.deviceId);
  query.set("token", resp.token);
  return query;
};

export const managedMobileStreamPath = (workspaceId: string): string =>
  `/api/mobile/secure/workspaces/${encodeURIComponent(workspaceId)}/stream`;

export const decryptManagedMobileStreamEnvelope = async (
  input: unknown,
): Promise<unknown> => {
  const connection = getDaemonConnection();
  if (!isMobileSecureConnection(connection.mobileSecure)) return input;
  const envelope = readServerEnvelope(input);
  const resp = await invoke<DecryptEnvelopeResponse>("mobile_decrypt_envelope", {
    req: {
      daemonPublicKey: connection.mobileSecure.daemonPublicKey,
      envelope: toTauriEnvelope(envelope),
    },
  });
  return JSON.parse(fromBase64(resp.plaintextB64)) as unknown;
};
