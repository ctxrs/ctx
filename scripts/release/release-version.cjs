#!/usr/bin/env node
"use strict";

const MAX_COMPONENT = 18446744073709551615n;
const MAX_VERSION_LENGTH = 62;
const CANONICAL_VERSION = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/u;

function parseReleaseVersion(value) {
  if (typeof value !== "string" || value.length > MAX_VERSION_LENGTH || !CANONICAL_VERSION.test(value)) {
    throw new Error("release version must be canonical MAJOR.MINOR.PATCH");
  }
  const parts = value.split(".").map((part) => BigInt(part));
  if (parts.some((part) => part > MAX_COMPONENT)) {
    throw new Error("release version component exceeds unsigned 64-bit range");
  }
  return parts;
}

function isReleaseVersion(value) {
  try {
    parseReleaseVersion(value);
    return true;
  } catch {
    return false;
  }
}

function compareReleaseVersions(left, right) {
  const leftParts = parseReleaseVersion(left);
  const rightParts = parseReleaseVersion(right);
  for (let index = 0; index < 3; index += 1) {
    if (leftParts[index] < rightParts[index]) return -1;
    if (leftParts[index] > rightParts[index]) return 1;
  }
  return 0;
}

function readCargoVersion(repo) {
  const fs = require("node:fs"), path = require("node:path");
  const packageText = fs.readFileSync(path.join(repo, "crates/ctx-cli/Cargo.toml"), "utf8");
  const packageSection = packageText.match(/^\[package\]\s*\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m)?.[1] ?? "";
  let version = packageSection.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version && /^\s*version\.workspace\s*=\s*true\s*$/m.test(packageSection)) {
    const workspace = fs.readFileSync(path.join(repo, "Cargo.toml"), "utf8");
    const section = workspace.match(/^\[workspace\.package\]\s*\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m)?.[1] ?? "";
    version = section.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1];
  }
  parseReleaseVersion(version);
  return version;
}

module.exports = {
  readCargoVersion,
  compareReleaseVersions,
  isReleaseVersion,
  parseReleaseVersion,
};

if (require.main === module) {
  try {
    if (process.argv.length !== 4 || process.argv[2] !== "check") {
      throw new Error("usage: release-version.cjs check MAJOR.MINOR.PATCH");
    }
    parseReleaseVersion(process.argv[3]);
  } catch (error) {
    console.error(`invalid release version: ${error.message}`);
    process.exit(1);
  }
}
