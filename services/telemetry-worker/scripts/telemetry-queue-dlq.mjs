#!/usr/bin/env node

import {createHash} from "node:crypto";
import fs from "node:fs";
import {gunzipSync} from "node:zlib";

const MAX_COMPRESSED_BYTES = 127_800;
const MAX_UNCOMPRESSED_BYTES = 64 * 1024;
const REDRIVE_WRAPPER_KEYS = [
  "body_base64",
  "body_sha256",
  "content_encoding",
  "content_type",
  "format_version",
  "kind",
];
const MESSAGE_KINDS = new Map([
  ["telemetry_row", "row"],
  ["blame_product_receipt", "receipt"],
  ["install_stage_row", "row"],
]);

function usage() {
  return `Usage: node scripts/telemetry-queue-dlq.mjs --input FILE --environment staging|prod

Offline-only decoder for the JSON response captured from Cloudflare's Queue
messages/peek endpoint. It accepts normal gzip bytes and retained JSON redrive
wrappers. The output contains hashes and bounded envelope fields, never the
telemetry row or encoded body. Use --input - to read stdin.
`;
}

export function parseDlqArgs(argv) {
  const options = {input: "", environment: "", help: false};
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--help" || argument === "-h") options.help = true;
    else if (argument === "--input") options.input = requireValue(argv, ++index, argument);
    else if (argument === "--environment") options.environment = requireValue(argv, ++index, argument);
    else throw new Error(`unknown argument: ${argument}`);
  }
  if (!options.help && !options.input) throw new Error("--input is required");
  if (!options.help && !["staging", "prod"].includes(options.environment)) {
    throw new Error("--environment must be staging or prod");
  }
  return options;
}

function requireValue(argv, index, argument) {
  if (index >= argv.length || argv[index].startsWith("--")) throw new Error(`${argument} requires a value`);
  return argv[index];
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function hasExactKeys(value, expected) {
  return JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...expected].sort());
}

function decodeBase64(value, maximumBytes = MAX_COMPRESSED_BYTES) {
  if (
    typeof value !== "string"
    || value.length === 0
    || value.length % 4 !== 0
  ) {
    throw new Error("message body is not padded RFC 4648 base64");
  }
  if (value.length > Math.ceil(maximumBytes / 3) * 4) {
    throw new Error("message body exceeds its size limit");
  }
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u.test(value)) {
    throw new Error("message body is not RFC 4648 base64");
  }
  const decoded = Buffer.from(value, "base64");
  if (decoded.byteLength > maximumBytes) throw new Error("message body exceeds its size limit");
  return decoded;
}

function validateMessageEnvelope(value, expectedEnvironment) {
  if (!isRecord(value) || value.format_version !== 1) throw new Error("unsupported queue format version");
  const payloadField = MESSAGE_KINDS.get(value.kind);
  if (!payloadField || !isRecord(value[payloadField])) throw new Error("unsupported queue message kind");
  const keys = Object.keys(value).sort();
  const expectedKeys = ["collision_visibility", "format_version", "kind", payloadField].sort();
  if (JSON.stringify(keys) !== JSON.stringify(expectedKeys)) throw new Error("unexpected queue envelope fields");
  if (!isRecord(value.collision_visibility)) throw new Error("collision visibility is missing");
  const analyticsEnvironment = value.collision_visibility.analytics_environment;
  if (analyticsEnvironment !== expectedEnvironment) {
    throw new Error(`analytics environment mismatch: expected ${expectedEnvironment}`);
  }
  return {
    analytics_environment: analyticsEnvironment,
    endpoint: boundedString(value.collision_visibility.endpoint, "endpoint"),
    event_family: boundedString(value.collision_visibility.event_family, "event family"),
    format_version: value.format_version,
    kind: value.kind,
  };
}

function decodeRedriveWrapper(encoded, index) {
  let text;
  try {
    text = new TextDecoder("utf-8", {fatal: true}).decode(encoded);
  } catch {
    throw new Error(`message ${index} JSON wrapper is not UTF-8`);
  }
  let wrapper;
  try {
    wrapper = JSON.parse(text);
  } catch {
    throw new Error(`message ${index} JSON wrapper is invalid`);
  }
  if (
    !isRecord(wrapper)
    || !hasExactKeys(wrapper, REDRIVE_WRAPPER_KEYS)
    || wrapper.format_version !== 1
    || wrapper.kind !== "telemetry_queue_redrive"
    || wrapper.content_encoding !== "identity"
    || wrapper.content_type !== "application/json"
    || typeof wrapper.body_sha256 !== "string"
    || !/^[0-9a-f]{64}$/u.test(wrapper.body_sha256)
  ) throw new Error(`message ${index} redrive wrapper is invalid`);
  const normalized = decodeBase64(wrapper.body_base64, MAX_UNCOMPRESSED_BYTES);
  if (
    normalized.byteLength > MAX_UNCOMPRESSED_BYTES
    || sha256(normalized) !== wrapper.body_sha256
  ) throw new Error(`message ${index} redrive wrapper is invalid`);
  return normalized;
}

function decodeQueueBody(message, index) {
  const contentType = message.metadata?.["CF-Content-Type"];
  if (contentType === "bytes") {
    const encoded = decodeBase64(message.body);
    if (encoded.byteLength > MAX_COMPRESSED_BYTES) {
      throw new Error(`message ${index} exceeds compressed limit`);
    }
    let normalized;
    try {
      normalized = gunzipSync(encoded, {maxOutputLength: MAX_UNCOMPRESSED_BYTES});
    } catch {
      throw new Error(`message ${index} is not bounded gzip`);
    }
    return {contentType, encoded, normalized};
  }
  if (contentType === "json") {
    const encoded = decodeBase64(message.body);
    const normalized = decodeRedriveWrapper(encoded, index);
    return {contentType, encoded, normalized};
  }
  throw new Error(`message ${index} content type is unsupported`);
}

function boundedString(value, field) {
  if (typeof value !== "string" || value.length === 0 || value.length > 80) {
    throw new Error(`${field} is invalid`);
  }
  return value;
}

function messagesFromPeekResponse(payload) {
  if (!isRecord(payload) || payload.success !== true || !isRecord(payload.result)) {
    throw new Error("input is not a successful Cloudflare peek response");
  }
  if (!Array.isArray(payload.result.messages)) throw new Error("peek response has no message array");
  return payload.result.messages;
}

export function decodePeekResponse(payload, {environment, nowMs = Date.now()} = {}) {
  const expectedEnvironment = environment === "prod" ? "production" : environment;
  if (!expectedEnvironment || !["production", "staging"].includes(expectedEnvironment)) {
    throw new Error("environment must be staging or prod");
  }
  const messages = messagesFromPeekResponse(payload);
  const decoded = messages.map((message, index) => {
    if (!isRecord(message)) throw new Error(`message ${index} is not an object`);
    const {contentType, encoded, normalized} = decodeQueueBody(message, index);
    let value;
    try {
      const text = new TextDecoder("utf-8", {fatal: true}).decode(normalized);
      value = JSON.parse(text);
    } catch {
      throw new Error(`message ${index} is not UTF-8 JSON`);
    }
    const envelope = validateMessageEnvelope(value, expectedEnvironment);
    const timestampMs = Number(message.timestamp_ms ?? 0);
    return {
      index,
      message_id_sha256: sha256(String(message.id ?? "missing")),
      body_sha256: sha256(encoded),
      queue_content_type: contentType,
      attempts: Number.isInteger(message.attempts) ? message.attempts : null,
      timestamp_ms: Number.isFinite(timestampMs) && timestampMs > 0 ? timestampMs : null,
      age_seconds: Number.isFinite(timestampMs) && timestampMs > 0
        ? Math.max(0, Math.round((nowMs - timestampMs) / 1000))
        : null,
      encoded_bytes: encoded.byteLength,
      uncompressed_bytes: normalized.byteLength,
      ...envelope,
    };
  });
  return {
    schema_version: 2,
    offline: true,
    checked_at: new Date(nowMs).toISOString(),
    environment: expectedEnvironment,
    source_queue: `ctx-telemetry-ingest-${expectedEnvironment === "production" ? "prod" : "staging"}-dlq`,
    message_count: decoded.length,
    messages: decoded,
  };
}

export function decodePeekText(text, options) {
  let payload;
  try {
    payload = JSON.parse(text);
  } catch {
    throw new Error("input is not JSON");
  }
  return decodePeekResponse(payload, options);
}

function readInput(input) {
  return input === "-" ? fs.readFileSync(0, "utf8") : fs.readFileSync(input, "utf8");
}

function main() {
  try {
    const options = parseDlqArgs(process.argv.slice(2));
    if (options.help) {
      process.stdout.write(usage());
      return;
    }
    const receipt = decodePeekText(readInput(options.input), {environment: options.environment});
    process.stdout.write(`${JSON.stringify(receipt, null, 2)}\n`);
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  }
}

if (import.meta.url === `file://${process.argv[1]}`) main();
