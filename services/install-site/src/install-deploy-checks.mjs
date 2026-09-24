import { createHash, verify } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { CLI_METADATA_PUBLIC_KEY_PEM } from "./cli-install-script.js";

export const UNIX_TARGETS = ["linux-x64", "linux-aarch64", "macos-x64", "macos-arm64"];
export const INSTALL_CHECKS = ["core_installed", "managed", "setup", "daemon", "path", "search"];
const SHA256 = /^[0-9a-f]{64}$/u;
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/u;
const MAX_BYTES = 1024 * 1024;

export function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

export function readBounded(file, maximum = MAX_BYTES) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size < 1 || stat.size > maximum) {
    throw new Error(`invalid or oversized gate input: ${file}`);
  }
  return fs.readFileSync(file);
}

export function readReport(file) {
  return JSON.parse(readBounded(file, 64 * 1024));
}

export function approvedRelease(file) {
  const evidence = readReport(file);
  const currentInstallerFeed = evidence?.kind === "public-cli-current-installer-feed";
  const mainChecked = evidence?.public_source?.remote_main_checked === true;
  const tagChecked = evidence?.public_source?.release_tag_checked === true
    && evidence.public_source.release_tag === `v${evidence.release?.version}`;
  if (evidence?.schema_version !== 1 || (!currentInstallerFeed && evidence.kind !== "public-cli-release-contract")
      || evidence.status !== "passed" || evidence.release?.channel !== "stable"
      || !VERSION.test(evidence.release.version ?? "")
      || !/^[0-9a-f]{40}$/u.test(evidence.release.source_commit ?? "")
      || evidence.public_source?.commit !== evidence.release.source_commit
      || evidence.public_source?.worktree_clean_checked !== true
      || (!mainChecked && !tagChecked)
      || evidence.metadata?.stable?.signature_verified !== true
      || evidence.metadata?.versioned?.signature_verified !== true
      || !SHA256.test(evidence.metadata.stable.sha256 ?? "")
      || !SHA256.test(evidence.metadata.versioned.sha256 ?? "")
      || (currentInstallerFeed && (!tagChecked || evidence.candidate_manifests !== undefined
        || evidence.validation?.construction !== "not_run"
        || evidence.validation?.publication_readback !== "passed"
        || !SHA256.test(evidence.current_feed_asset_sha256s?.sha256 ?? "")))) {
    throw new Error("a passed approved public release-contract result is required");
  }
  const pairs = evidence.metadata.managed_pair;
  for (const platform of [...UNIX_TARGETS, "windows-x64"]) {
    if (!SHA256.test(pairs?.[platform]?.core_sha256 ?? "")
        || !SHA256.test(pairs?.[platform]?.pro_sha256 ?? "")) {
      throw new Error(`approved release is missing the signed pair for ${platform}`);
    }
  }
  return { version: evidence.release.version, source_commit: evidence.release.source_commit, pairs,
    metadata_sha256: evidence.metadata.stable.sha256 };
}

// Only the request correlation assignment differs between equivalent responses.
// Do not normalize command bodies, URLs, whitespace, or arbitrary ia_* strings.
export function installerBody(bytes, powershell = false) {
  const body = Buffer.from(bytes).toString("utf8");
  const pattern = powershell
    ? /^\$installAttemptId = "ia_[A-Za-z0-9_-]{8,128}"$/gmu
    : /^install_attempt_id="ia_[A-Za-z0-9_-]{8,128}"$/gmu;
  if ([...body.matchAll(pattern)].length !== 1) {
    throw new Error("installer must contain exactly one embedded attempt identity");
  }
  return body.replace(pattern, powershell
    ? '$installAttemptId = "ia_DEPLOYMENT_CHECK"'
    : 'install_attempt_id="ia_DEPLOYMENT_CHECK"');
}

export function assertSameInstaller(actual, expected, powershell = false) {
  if (installerBody(actual, powershell) !== installerBody(expected, powershell)) {
    throw new Error("served installer differs from the tested production candidate");
  }
}

export function sourceIdentity(serviceRoot) {
  const files = [];
  function visit(directory) {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) visit(file);
      else if (entry.isFile()) files.push(file);
      else throw new Error(`unsupported source input: ${file}`);
    }
  }
  visit(path.join(serviceRoot, "src"));
  visit(path.join(serviceRoot, "tests"));
  files.push(...["deploy.mjs", "package.json", "package-lock.json", "wrangler.toml",
    "BUILD.bazel", "typecheck-managed-pair.mjs", "tsconfig.managed-pair.json"]
    .map((file) => path.join(serviceRoot, file)));
  const hash = createHash("sha256");
  for (const file of files.sort()) {
    hash.update(path.relative(serviceRoot, file));
    hash.update("\0");
    hash.update(sha256(fs.readFileSync(file)));
    hash.update("\n");
  }
  return hash.digest("hex");
}

export function validateUnixResult(result, { platform, installerSha256, version, coreSha256, proSha256 }) {
  if (result?.schema_version !== 1 || result.status !== "passed"
      || result.platform !== platform || result.installer_sha256 !== installerSha256
      || !VERSION.test(result.version ?? "") || (version && result.version !== version)
      || !SHA256.test(result.core_sha256 ?? "")
      || (isUnifiedVersion(result.version)
        ? result.pro_sha256 !== null || result.checks?.single_binary !== true
        : !SHA256.test(result.pro_sha256 ?? "") || result.checks?.pro_installed !== true)
      || (coreSha256 && result.core_sha256 !== coreSha256)
      || (proSha256 && result.pro_sha256 !== proSha256)
      || !Number.isFinite(result.elapsed_seconds) || result.elapsed_seconds < 0
      || INSTALL_CHECKS.some((check) => result.checks?.[check] !== true)) {
    throw new Error(`missing, failed, or mismatched real installation checks for ${platform}`);
  }
  return result;
}

export function validateWindowsResult(result, { installerSha256, version, coreSha256, proSha256 }) {
  if (result?.stage !== "windows_live_installer" || result.status !== "passed"
      || result.release_authority !== false || result.installer_sha256 !== installerSha256
      || result.version !== version || !Array.isArray(result.shells) || result.shells.length !== 2) {
    throw new Error("missing or mismatched real Windows installation result");
  }
  const editions = new Set();
  for (const shell of result.shells) {
    const edition = /^5\.1\./u.test(shell.powershell ?? "") ? "5.1"
      : /^7\.\d+\.\d+$/u.test(shell.powershell ?? "") ? "7" : null;
    if (!edition || editions.has(edition) || shell.loaded_profile !== true
        || shell.version !== version || !Array.isArray(shell.passed)
        || shell.passed.length !== 2) throw new Error("Windows must pass both required PowerShell editions");
    editions.add(edition);
    for (const [index, phase] of shell.passed.entries()) {
      if (phase.phase !== ["fresh", "managed-reinstall"][index] || phase.exit_code !== 0
          || !SHA256.test(phase.core_sha256 ?? "")
          || (isUnifiedVersion(version)
            ? phase.pro_sha256 !== null || phase.single_binary !== true
            : !SHA256.test(phase.pro_sha256 ?? ""))
          || (coreSha256 && phase.core_sha256 !== coreSha256)
          || (proSha256 && phase.pro_sha256 !== proSha256)) {
        throw new Error("Windows fresh installation and managed reinstall must both pass");
      }
      if (index && (phase.core_sha256 !== shell.passed[0].core_sha256
          || phase.pro_sha256 !== shell.passed[0].pro_sha256)) {
        throw new Error("Windows reinstall changed the expected pair");
      }
    }
    if (shell.passed[0].core_sha256 !== result.shells[0].passed[0].core_sha256
        || shell.passed[0].pro_sha256 !== result.shells[0].passed[0].pro_sha256) {
      throw new Error("PowerShell editions installed different pairs");
    }
  }
  return result;
}

export function validateNativeResults(directory, candidate, localResult) {
  const { version, pairs } = candidate.release;
  const expectedPro = (platform) => isUnifiedVersion(version) ? undefined : pairs[platform].pro_sha256;
  const results = {};
  // Linux installation is required; Windows evidence is optional, but any
  // explicitly supplied report must pass the complete existing validation.
  results["linux-x64"] = validateUnixResult(localResult, {
    platform: "linux-x64", installerSha256: candidate.shell_sha256, version,
    coreSha256: pairs["linux-x64"].core_sha256, proSha256: expectedPro("linux-x64"),
  });
  results["windows-x64"] = directory == null ? { status: "not_run", reason: "not_supplied" } : validateWindowsResult(
    readReport(path.join(directory, "windows-x64.json")),
    { installerSha256: candidate.powershell_sha256, version,
      coreSha256: pairs["windows-x64"].core_sha256, proSha256: expectedPro("windows-x64") },
  );
  return results;
}

export function isUnifiedVersion(version) {
  if (!VERSION.test(version ?? "")) throw new Error("invalid release version");
  const [major, minor] = version.split(".").map(Number);
  return major > 1 || (major === 1 && minor >= 5);
}

// This observation is of the CURRENT public feed, independent of the proposed
// installer source identity and the separate, unpublished 1.5 fixture evidence.
export async function verifyCurrentFeed(release, fetcher = fetch) {
  const url = "https://cli.ctx.rs/functions/v2/releases/stable/ctx-release-metadata.env";
  async function get(suffix, limit) {
    const response = await fetcher(url + suffix, { redirect: "error", signal: AbortSignal.timeout(30_000) });
    if (!response.ok) throw new Error(`current signed feed returned HTTP ${response.status}`);
    const chunks = []; let length = 0;
    for await (const chunk of response.body) {
      length += chunk.length;
      if (length > limit) throw new Error("current signed feed exceeds limit");
      chunks.push(chunk);
    }
    return Buffer.concat(chunks);
  }
  const metadata = await get("", 1024 * 1024);
  const signature = (await get(".sig", 65536)).toString("ascii").trim();
  if (!/^[A-Za-z0-9+/]+={0,2}$/u.test(signature) ||
      !verify("RSA-SHA256", metadata, CLI_METADATA_PUBLIC_KEY_PEM, Buffer.from(signature, "base64")) ||
      sha256(metadata) !== release.metadata_sha256) {
    throw new Error("current signed feed differs from approved release evidence");
  }
  const text = metadata.toString("utf8");
  for (const [key, value] of Object.entries({
    CTX_RELEASE_VERSION: release.version, CTX_RELEASE_CHANNEL: "stable",
    CTX_RELEASE_SOURCE_COMMIT: release.source_commit,
  })) {
    const entries = text.split(/\r?\n/u).filter((line) => line.startsWith(key + "="));
    if (entries.length !== 1 || entries[0] !== `${key}=${value}`) {
      throw new Error(`current signed feed ${key} differs from approved evidence`);
    }
  }
  return { version: release.version, metadata_sha256: sha256(metadata), signature_verified: true };
}
