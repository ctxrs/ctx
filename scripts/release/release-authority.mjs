import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import {
  exactKeys, readJsonFile, sha256, loadInputAuthority, contractError,
  MIN_RSA_MODULUS_BITS, MAX_RSA_MODULUS_BITS,
} from "./managed-pair-release-io.mjs";

const REGISTRY = fileURLToPath(new URL(
  "../../contracts/ctx-managed-pair-release-authority-v1.json", import.meta.url,
));

// Public trust material is the sole authority. Signing keys are supplied only
// to the final signer; build/package consumers never contact a secret service.
export function loadReleaseAuthorities({ registryPath = REGISTRY } = {}) {
  const registry = readJsonFile(registryPath, "release authority", 64 * 1024);
  exactKeys(registry, ["contract", "schema_version", "channels"], "release authority");
  if (registry.contract !== "ctx-managed-pair-release-authority"
      || registry.schema_version !== 1 || !Array.isArray(registry.channels)
      || registry.channels.length !== 2) contractError("invalid release authority");
  const result = new Map();
  for (const channel of registry.channels) {
    exactKeys(channel, ["id", "key_id", "signature_algorithm",
      "public_key_der_sha256", "public_key_pem"], "release channel");
    if (!["stable", "staging"].includes(channel.id) || result.has(channel.id)
        || channel.signature_algorithm !== "rsa-pkcs1v15-sha256"
        || typeof channel.key_id !== "string" || !/^[a-z0-9][a-z0-9._-]{0,127}$/u.test(channel.key_id)
        || typeof channel.public_key_pem !== "string" || channel.public_key_pem.includes("PRIVATE KEY")) {
      contractError("invalid release channel");
    }
    const publicKey = crypto.createPublicKey(channel.public_key_pem);
    const bits = publicKey.asymmetricKeyDetails?.modulusLength;
    const publicKeyDigest = sha256(publicKey.export({ format: "der", type: "pkcs1" }));
    if (publicKey.asymmetricKeyType !== "rsa" || !Number.isInteger(bits) || bits < MIN_RSA_MODULUS_BITS
        || bits > MAX_RSA_MODULUS_BITS || publicKeyDigest !== channel.public_key_der_sha256) {
      contractError("release public key identity differs from its contract");
    }
    result.set(channel.id, Object.freeze({ channel: channel.id,
      publicKey, publicKeyDigest, publicKeyBytes: Buffer.from(channel.public_key_pem),
      releaseKeyId: channel.key_id,
    }));
  }
  return result;
}
export function releaseTrust(channel, authorities = loadReleaseAuthorities()) {
  const trust = authorities.get(channel);
  if (trust == null) contractError("unknown release channel");
  return trust;
}
export function releaseChannels() { return ["stable", "staging"]; }
export function loadOperationalManagedPairInputAuthority() { return loadInputAuthority(); }
export const STABLE_RELEASE_TRUST = releaseTrust("stable");
export const STAGING_RELEASE_TRUST = releaseTrust("staging");
