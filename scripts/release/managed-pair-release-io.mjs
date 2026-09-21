import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { TextDecoder } from "node:util";

export const TARGET_IDS = Object.freeze([
  "linux-arm64",
  "linux-x64",
  "macos-arm64",
  "macos-x64",
  "windows-x64",
]);
export const COMPONENT_KINDS = Object.freeze(["core", "companion"]);
export const MAX_COMPONENT_BYTES = 256 * 1024 * 1024;
export const MAX_AGGREGATE_COMPONENT_BYTES = 1024 * 1024 * 1024;
export const MAX_MANIFEST_BYTES = 1024 * 1024;
export const MAX_SIGNATURE_BYTES = 1024;
export const MIN_RSA_MODULUS_BITS = 2048;
export const MAX_RSA_MODULUS_BITS = 8192;
export const MAX_HISTORICAL_TARGET_MATRICES = 16;
export const FIXED_CONTRACT_FILE = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$/u;
export const MAX_ROLLBACK_GENERATION = 9_007_199_254_740_991;
// Released clients require this compatibility value. It describes their
// incoming transaction envelope, not an RPC implemented by the unified CLI.
export const CORE_CAPABILITY_PROTOCOL_FINGERPRINT =
  "4be5325aa95a6fdd22e59340abfbadefd8b73ffd2a1e3f55ea75deef9e956e34";
export const LEGACY_TARGET_MATRIX_SHA256 =
  "718d2f364e10f57e3a98228d8feaea59c955db0fd7da309a7b0479a6296e18ef";
export const SHA256 = /^[0-9a-f]{64}$/u;
export const COMMIT = /^[0-9a-f]{40}$/u;
export const NAME = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$/u;
export const RUST_TARGET = /^[a-z0-9_]+(?:-[a-z0-9_]+){2,5}$/u;
export const RELATIVE_FILE = /^(?:[A-Za-z0-9][A-Za-z0-9._+-]{0,127}\/){1,7}[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$/u;
export const RFC3339_SECONDS = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/u;
export const READ_BLOCK_BYTES = 1024 * 1024;

export function contractError(message) {
  throw new Error(message);
}
export function exactKeys(value, expected, label) {
  if (value === null || typeof value !== "object" || Array.isArray(value)
      || Object.keys(value).sort().join("\0") !== [...expected].sort().join("\0")) {
    contractError(`${label} has missing or unexpected fields`);
  }
  return value;
}

export function requireKeys(value, expected, label) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    contractError(`${label} must be an object`);
  }
  for (const key of expected) {
    if (!Object.hasOwn(value, key)) contractError(`${label} is missing ${key}`);
  }
  return value;
}

export function canonicalJson(value) {
  if (value === null || typeof value === "boolean" || typeof value === "string") {
    return JSON.stringify(value);
  }
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value)) contractError("canonical JSON contains an unsafe number");
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (typeof value !== "object") contractError("canonical JSON contains an unsupported value");
  return `{${Object.keys(value).sort().map((key) =>
    `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
}

export function canonicalJsonBytes(value) {
  return Buffer.from(canonicalJson(value), "utf8");
}

export function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

export function decodeJsonString(source, state) {
  const start = state.offset;
  state.offset += 1;
  let escaped = false;
  while (state.offset < source.length) {
    const code = source.charCodeAt(state.offset);
    const character = source[state.offset++];
    if (!escaped && character === '"') {
      try {
        return JSON.parse(source.slice(start, state.offset));
      } catch {
        contractError("JSON string is malformed");
      }
    }
    if (!escaped && code < 0x20) contractError("JSON string contains a control character");
    if (!escaped && character === "\\") escaped = true;
    else escaped = false;
  }
  contractError("JSON string is unterminated");
}

export function skipWhitespace(source, state) {
  while (state.offset < source.length && /[\u0009\u000a\u000d\u0020]/u.test(source[state.offset])) {
    state.offset += 1;
  }
}

export function parseJsonValue(source, state, label) {
  skipWhitespace(source, state);
  const character = source[state.offset];
  if (character === '"') return decodeJsonString(source, state);
  if (character === "{") {
    state.offset += 1;
    const result = {};
    const keys = new Set();
    skipWhitespace(source, state);
    if (source[state.offset] === "}") {
      state.offset += 1;
      return result;
    }
    for (;;) {
      skipWhitespace(source, state);
      if (source[state.offset] !== '"') contractError(`${label} object key is malformed`);
      const key = decodeJsonString(source, state);
      if (keys.has(key)) contractError(`${label} contains duplicate JSON key ${JSON.stringify(key)}`);
      keys.add(key);
      skipWhitespace(source, state);
      if (source[state.offset++] !== ":") contractError(`${label} object separator is malformed`);
      const value = parseJsonValue(source, state, label);
      Object.defineProperty(result, key, {
        configurable: true,
        enumerable: true,
        value,
        writable: true,
      });
      skipWhitespace(source, state);
      const separator = source[state.offset++];
      if (separator === "}") return result;
      if (separator !== ",") contractError(`${label} object is malformed`);
    }
  }
  if (character === "[") {
    state.offset += 1;
    const result = [];
    skipWhitespace(source, state);
    if (source[state.offset] === "]") {
      state.offset += 1;
      return result;
    }
    for (;;) {
      result.push(parseJsonValue(source, state, label));
      skipWhitespace(source, state);
      const separator = source[state.offset++];
      if (separator === "]") return result;
      if (separator !== ",") contractError(`${label} array is malformed`);
    }
  }
  for (const [literal, value] of [["true", true], ["false", false], ["null", null]]) {
    if (source.startsWith(literal, state.offset)) {
      state.offset += literal.length;
      return value;
    }
  }
  const number = source.slice(state.offset).match(/^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/u)?.[0];
  if (number != null) {
    state.offset += number.length;
    const value = Number(number);
    if (!Number.isFinite(value)) contractError(`${label} contains a non-finite number`);
    return value;
  }
  contractError(`${label} contains malformed JSON`);
}

export function parseJsonStrict(bytes, label) {
  let source;
  try {
    source = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    contractError(`${label} is not UTF-8`);
  }
  const state = { offset: 0 };
  const value = parseJsonValue(source, state, label);
  skipWhitespace(source, state);
  if (state.offset !== source.length) contractError(`${label} has trailing JSON data`);
  return value;
}

export function statIdentity(stat, includeContentIdentity) {
  const value = {
    dev: stat.dev,
    ino: stat.ino,
    mode: stat.mode,
  };
  if (includeContentIdentity) {
    value.size = stat.size;
    value.mtimeNs = stat.mtimeNs;
    value.ctimeNs = stat.ctimeNs;
  }
  return value;
}

export function sameIdentity(left, right) {
  return Object.keys(left).every((key) => left[key] === right[key]);
}

export function capturePath(file, label) {
  const absolute = path.resolve(file);
  const parsed = path.parse(absolute);
  let current = parsed.root;
  const identities = [];
  for (const part of absolute.slice(parsed.root.length).split(path.sep).filter(Boolean)) {
    current = path.join(current, part);
    let stat;
    try {
      stat = fs.lstatSync(current, { bigint: true });
    } catch {
      contractError(`${label} path component is unavailable`);
    }
    if (stat.isSymbolicLink()) contractError(`${label} path contains a symlink or reparse point`);
    identities.push({
      file: current,
      identity: statIdentity(stat, current === absolute),
      isDirectory: stat.isDirectory(),
      isFile: stat.isFile(),
    });
  }
  return { absolute, identities };
}

export function verifyCapturedPath(captured, label, includeLeafContentIdentity = true) {
  for (const expected of captured.identities) {
    let stat;
    try {
      stat = fs.lstatSync(expected.file, { bigint: true });
    } catch {
      contractError(`${label} path changed while it was being verified`);
    }
    const isLeaf = expected.file === captured.absolute;
    const expectedIdentity = isLeaf && !includeLeafContentIdentity
      ? statIdentity({ ...expected.identity }, false)
      : expected.identity;
    if (stat.isSymbolicLink()
        || stat.isDirectory() !== expected.isDirectory
        || stat.isFile() !== expected.isFile
        || !sameIdentity(
          expectedIdentity,
          statIdentity(stat, isLeaf && includeLeafContentIdentity),
        )) {
      contractError(`${label} path changed while it was being verified`);
    }
  }
}

export function openStableRegularFile(file, label, maximumBytes) {
  const captured = capturePath(file, label);
  const leaf = captured.identities.at(-1);
  if (leaf == null || !leaf.isFile) contractError(`${label} must be a regular file`);
  const flags = fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0);
  let descriptor;
  try {
    descriptor = fs.openSync(captured.absolute, flags);
  } catch {
    contractError(`${label} cannot be opened without following links`);
  }
  const stat = fs.fstatSync(descriptor, { bigint: true });
  if (!stat.isFile() || stat.size <= 0n || stat.size > BigInt(maximumBytes)
      || !sameIdentity(leaf.identity, statIdentity(stat, true))) {
    fs.closeSync(descriptor);
    contractError(`${label} must be a bounded stable regular file`);
  }
  return { captured, descriptor, stat };
}

export function finishStableRegularFile(opened, label, hook = undefined) {
  if (hook != null) hook();
  const after = fs.fstatSync(opened.descriptor, { bigint: true });
  if (!sameIdentity(statIdentity(opened.stat, true), statIdentity(after, true))) {
    contractError(`${label} changed while it was being read`);
  }
  verifyCapturedPath(opened.captured, label);
}

export function readStableRegularFile(file, label, maximumBytes = MAX_MANIFEST_BYTES, hook = undefined) {
  const opened = openStableRegularFile(file, label, maximumBytes);
  const chunks = [];
  let total = 0;
  const buffer = Buffer.allocUnsafe(Math.min(READ_BLOCK_BYTES, Number(opened.stat.size)));
  try {
    for (;;) {
      const count = fs.readSync(opened.descriptor, buffer, 0, buffer.length, null);
      if (count === 0) break;
      total += count;
      if (total > maximumBytes) contractError(`${label} exceeds its size bound`);
      chunks.push(Buffer.from(buffer.subarray(0, count)));
    }
    finishStableRegularFile(opened, label, hook);
    return Buffer.concat(chunks, total);
  } finally {
    buffer.fill(0);
    fs.closeSync(opened.descriptor);
  }
}

export function readJsonFile(file, label, maximumBytes = MAX_MANIFEST_BYTES) {
  const value = parseJsonStrict(readStableRegularFile(file, label, maximumBytes), label);
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    contractError(`${label} must contain an object`);
  }
  return value;
}

export function requireString(value, pattern, label) {
  if (typeof value !== "string" || !pattern.test(value)) contractError(`${label} is malformed`);
  return value;
}

export function requireSize(value, maximum, label) {
  if (!Number.isSafeInteger(value) || value < 1 || value > maximum) {
    contractError(`${label} must be a bounded positive integer`);
  }
  return value;
}

export function strictBase64(value, label, maximumBytes) {
  if (typeof value !== "string" || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u.test(value)) {
    contractError(`${label} is malformed`);
  }
  const bytes = Buffer.from(value, "base64");
  if (bytes.length === 0 || bytes.length > maximumBytes || bytes.toString("base64") !== value) {
    contractError(`${label} is not bounded canonical base64`);
  }
  return bytes;
}

export function requireSnapshot(value, label) {
  exactKeys(value, ["contract", "fingerprint"], label);
  if (value.contract !== "ctx-managed-pair-snapshot-v1") {
    contractError(`${label} has an unsupported contract`);
  }
  requireString(value.fingerprint, SHA256, `${label} fingerprint`);
}

export function requireCompatibility(value, label) {
  exactKeys(value, ["invocation_fingerprint", "core_capability_fingerprint"], label);
  requireString(value.invocation_fingerprint, SHA256, `${label} invocation fingerprint`);
  requireString(value.core_capability_fingerprint, SHA256, `${label} Core capability fingerprint`);
}

export function fixedContractEntry(value, label) {
  exactKeys(value, ["path", "sha256"], label);
  requireString(value.path, FIXED_CONTRACT_FILE, `${label} path`);
  requireString(value.sha256, SHA256, `${label} fingerprint`);
}

export function releaseVersionContract(value) {
  exactKeys(value, [
    "schema_version", "kind", "grammar", "component_max", "valid", "invalid", "ordering",
  ], "public release-version contract");
  if (value.schema_version !== 1 || value.kind !== "ctx-release-version-contract"
      || value.component_max !== "18446744073709551615"
      || !Array.isArray(value.valid) || !Array.isArray(value.invalid)
      || !Array.isArray(value.ordering)) {
    contractError("public release-version contract is invalid");
  }
  return value;
}

export function validReleaseName(value, versionContract) {
  if (typeof value !== "string" || !value.startsWith("v")) return false;
  const components = value.slice(1).split(".");
  if (components.length !== 3 || components.some((part) => !/^(?:0|[1-9][0-9]*)$/u.test(part))) {
    return false;
  }
  const maximum = BigInt(versionContract.component_max);
  return components.every((part) => BigInt(part) <= maximum);
}

export function loadInputAuthority() {
  const contract = (name) => {
    const file = fileURLToPath(new URL(`../../contracts/${name}`, import.meta.url));
    const bytes = readStableRegularFile(file, name, MAX_MANIFEST_BYTES);
    return Object.freeze({ bytes, digest: sha256(bytes), path: file });
  };
  const targetMatrix = contract("release-targets-v1.json");
  if (targetMatrix.digest !== LEGACY_TARGET_MATRIX_SHA256) {
    contractError("incoming updater target matrix must retain its released identity");
  }
  const releaseVersion = contract("release-version-v1.json");
  return Object.freeze({
    targetMatrix,
    targetMatrices: Object.freeze([targetMatrix]),
    releaseVersion: Object.freeze({ ...releaseVersion,
      value: releaseVersionContract(parseJsonStrict(releaseVersion.bytes, "release version")),
    }),
  });
}

export function targetMatrixFromBytes(bytes, digest) {
  const value = parseJsonStrict(bytes, "release target matrix");
  if (value?.schema_version !== 1
      || !Array.isArray(value.targets) || value.targets.length !== TARGET_IDS.length
      || value.targets.map((target) => target?.id).join("\0") !== TARGET_IDS.join("\0")) {
    contractError("release target matrix is not the exact five-target ordered matrix");
  }
  const targets = new Map();
  for (const target of value.targets) {
    for (const name of [
      "id", "os", "arch", "public_rust_target", "managed_pair_core_slot",
      "managed_pair_companion_slot", "public_artifact", "helper_artifact",
      "runtime_authority",
    ]) {
      if (typeof target[name] !== "string" || target[name].length === 0) {
        contractError(`release target matrix lacks ${name}`);
      }
    }
    if (!RUST_TARGET.test(target.public_rust_target) || targets.has(target.id)) {
      contractError("release target matrix has invalid compilation targets");
    }
    targets.set(target.id, Object.freeze({ ...target }));
  }
  return Object.freeze({ bytes, digest, targets, value });
}

export function loadTargetMatrix(file, expectedDigest = undefined, inputAuthority = undefined) {
  const bytes = readStableRegularFile(file, "release target matrix", MAX_MANIFEST_BYTES);
  const digest = sha256(bytes);
  if (expectedDigest !== undefined && digest !== expectedDigest) {
    contractError("release target matrix does not match the candidate fingerprint");
  }
  if (inputAuthority !== undefined && digest !== inputAuthority.targetMatrix.digest) {
    contractError("release target matrix differs from the fixed public contract");
  }
  return targetMatrixFromBytes(bytes, digest);
}

export function trustedTargetMatrix(inputAuthority, digest) {
  requireString(digest, SHA256, "historical target-matrix fingerprint");
  const authority = inputAuthority?.targetMatrices?.find((entry) => entry.digest === digest);
  if (authority == null) {
    contractError("existing release pointer names an untrusted target matrix");
  }
  return targetMatrixFromBytes(authority.bytes, authority.digest);
}

export function loadCandidate(file, inputAuthority = undefined) {
  const value = readJsonFile(file, "managed-pair release candidate", 64 * 1024);
  exactKeys(value, [
    "contract", "schema_version", "channel", "release_name",
    "target_matrix_sha256", "rollback_generation",
  ], "managed-pair release candidate");
  if (value.contract !== "ctx-managed-pair-release-candidate"
      || value.schema_version !== 1
      || !["stable", "staging"].includes(value.channel)) {
    contractError("managed-pair release candidate has an invalid envelope");
  }
  requireString(value.release_name, NAME, "managed-pair release name");
  requireString(value.target_matrix_sha256, SHA256, "candidate target-matrix fingerprint");
  requireSize(value.rollback_generation, MAX_ROLLBACK_GENERATION, "candidate rollback generation");
  if (inputAuthority !== undefined
      && (value.target_matrix_sha256 !== inputAuthority.targetMatrix.digest
        || !validReleaseName(value.release_name, inputAuthority.releaseVersion.value))) {
    contractError("candidate does not bind the fixed public contracts");
  }
  return Object.freeze({ ...value });
}

export async function readBoundedResponse(response, maximumBytes, capture, label) {
  const declared = response.headers.get("content-length");
  if (declared != null
      && (!/^(?:0|[1-9][0-9]*)$/u.test(declared)
        || BigInt(declared) > BigInt(maximumBytes))) {
    contractError(`${label} response exceeds its declared body bound`);
  }
  if (response.body == null) contractError(`${label} response body is unavailable`);
  const reader = response.body.getReader();
  const digest = crypto.createHash("sha256");
  const chunks = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      const chunk = Buffer.from(value);
      total += chunk.length;
      if (total > maximumBytes) {
        await reader.cancel();
        contractError(`${label} response exceeds its streamed body bound`);
      }
      digest.update(chunk);
      if (capture) chunks.push(chunk);
    }
  } finally {
    reader.releaseLock();
  }
  if (declared != null && Number(declared) !== total) {
    contractError(`${label} response body differs from content-length`);
  }
  return {
    body: capture ? Buffer.concat(chunks, total) : undefined,
    digest: digest.digest("hex"),
    size: total,
  };
}
