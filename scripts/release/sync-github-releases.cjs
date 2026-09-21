#!/usr/bin/env node
"use strict";

const childProcess = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { compareReleaseVersions, parseReleaseVersion } = require("./release-version.cjs");

const ROOT = path.resolve(__dirname, "../..");
const RELEASE_PAYLOAD_NAMES = Object.freeze([
  "ctx-linux-aarch64",
  "ctx-linux-aarch64.cdx.json",
  "ctx-linux-aarch64.third-party-notices.txt",
  "ctx-linux-x64",
  "ctx-linux-x64.cdx.json",
  "ctx-linux-x64.third-party-notices.txt",
  "ctx-macos-arm64",
  "ctx-macos-arm64.cdx.json",
  "ctx-macos-arm64.third-party-notices.txt",
  "ctx-macos-x64",
  "ctx-macos-x64.cdx.json",
  "ctx-macos-x64.third-party-notices.txt",
  "ctx-onnxruntime-linux-aarch64.tar.gz",
  "ctx-onnxruntime-linux-x64.tar.gz",
  "ctx-onnxruntime-macos-arm64.tar.gz",
  "ctx-onnxruntime-macos-x64.tar.gz",
  "ctx-onnxruntime-windows-x64.zip",
  "ctx-windows-x64.exe",
  "ctx-windows-x64.exe.cdx.json",
  "ctx-windows-x64.exe.third-party-notices.txt",
]);
const RELEASE_ASSET_NAMES = Object.freeze([...RELEASE_PAYLOAD_NAMES, "SHA256SUMS"]);

function fail(message) {
  throw new Error(message);
}

function env(name, fallback = "") {
  const value = process.env[name];
  return value == null || value === "" ? fallback : value;
}

function run(command, args, options = {}) {
  const output = childProcess.execFileSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    ...options,
  });
  if (typeof output === "string") {
    return output.trim();
  }
  if (Buffer.isBuffer(output)) {
    return output.toString("utf8").trim();
  }
  return "";
}

function tryRun(command, args, options = {}) {
  const result = childProcess.spawnSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    ...options,
  });
  return {
    status: result.status ?? 1,
    stdout: (result.stdout || "").trim(),
    stderr: (result.stderr || "").trim(),
  };
}

function usage() {
  console.log(`usage: node scripts/release/sync-github-releases.cjs [options]

Checks or safely stages and promotes GitHub Releases for public ctx from the
ctx.rs changelog. Dry-run is the default; --apply is always required to write.

Options:
  --apply                  apply the explicitly selected write operation
  --stage-prerelease       create a new prerelease, never a final release
  --promote-final          promote an existing verified prerelease to final/latest
  --asset-dir PATH         directory containing the exact qualified release assets
  --update-existing        rewrite notes for existing releases when --apply is set
  --version VERSION        sync one version; may be repeated
  --from VERSION           include changelog entries at or after VERSION
  --to VERSION             include changelog entries at or before VERSION
  --repo OWNER/REPO        GitHub repo, default ctxrs/ctx
  --public-repo PATH       public ctx checkout; otherwise set CTX_PUBLIC_CTX_REPO
  --changelog PATH         changelog path, default docs-content/changelog.mdx
  -h, --help               show this help

Examples:
  node scripts/release/sync-github-releases.cjs --version 0.16.0
  node scripts/release/sync-github-releases.cjs --version 1.2.0 \\
    --stage-prerelease --asset-dir /path/to/factory-assets
  node scripts/release/sync-github-releases.cjs --version 1.2.0 \\
    --stage-prerelease --asset-dir /path/to/factory-assets --apply
  node scripts/release/sync-github-releases.cjs --version 1.2.0 \\
    --promote-final --asset-dir /path/to/factory-assets --apply
`);
}

function parseArgs(argv) {
  const options = {
    apply: false,
    stagePrerelease: false,
    promoteFinal: false,
    assetDir: "",
    updateExisting: false,
    versions: [],
    from: "",
    to: "",
    repo: env("CTX_PUBLIC_GITHUB_REPO", "ctxrs/ctx"),
    publicRepo: env("CTX_PUBLIC_CTX_REPO", ROOT),
    changelog: env("CTX_PUBLIC_CHANGELOG_PATH"),
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const value = () => {
      index += 1;
      if (index >= argv.length) {
        fail(`${arg} requires a value`);
      }
      return argv[index];
    };
    switch (arg) {
      case "--apply":
        options.apply = true;
        break;
      case "--stage-prerelease":
        options.stagePrerelease = true;
        break;
      case "--promote-final":
        options.promoteFinal = true;
        break;
      case "--asset-dir":
        options.assetDir = value();
        break;
      case "--update-existing":
        options.updateExisting = true;
        break;
      case "--version":
        options.versions.push(value());
        break;
      case "--from":
        options.from = value();
        break;
      case "--to":
        options.to = value();
        break;
      case "--repo":
        options.repo = value();
        break;
      case "--public-repo":
        options.publicRepo = value();
        break;
      case "--changelog":
        options.changelog = value();
        break;
      case "-h":
      case "--help":
        usage();
        process.exit(0);
        break;
      default:
        fail(`unknown argument: ${arg}`);
    }
  }

  if (!/^[^/\s]+\/[^/\s]+$/.test(options.repo)) {
    fail(`--repo must be OWNER/REPO, got ${options.repo}`);
  }
  if (options.stagePrerelease && options.promoteFinal) {
    fail("--stage-prerelease and --promote-final are mutually exclusive");
  }
  const releaseMode = options.stagePrerelease || options.promoteFinal;
  if (releaseMode) {
    if (options.versions.length !== 1 || options.from || options.to) {
      fail("release staging and promotion require exactly one explicit --version");
    }
    if (!options.assetDir) {
      fail("release staging and promotion require --asset-dir");
    }
    if (options.updateExisting) {
      fail("--update-existing is separate from release staging and promotion");
    }
  } else if (options.assetDir) {
    fail("--asset-dir requires --stage-prerelease or --promote-final");
  }
  if (options.apply && !releaseMode && !options.updateExisting) {
    fail("--apply requires --stage-prerelease, --promote-final, or --update-existing");
  }
  if (options.apply && options.repo !== "ctxrs/ctx") {
    fail("release mutation is restricted to ctxrs/ctx");
  }
  for (const version of [...options.versions, options.from, options.to].filter(Boolean)) {
    try {
      parseReleaseVersion(version);
    } catch {
      fail(`invalid release version: ${version}`);
    }
  }
  return options;
}

function publicRepo(configured) {
  if (!configured) {
    fail("set CTX_PUBLIC_CTX_REPO or pass --public-repo for the public ctx checkout");
  }
  const repo = path.resolve(configured);
  if (!fs.existsSync(path.join(repo, "Cargo.toml"))) {
    fail(`public ctx checkout does not contain Cargo.toml: ${repo}`);
  }
  return repo;
}

function parseCargoVersion(repo) {
  return require("./release-version.cjs").readCargoVersion(repo);
}

function parseChangelog(changelogPath) {
  const text = fs.readFileSync(changelogPath, "utf8");
  const headingPattern = /^## (\d+\.\d+\.\d+)(?: - (\d{4}-\d{2}-\d{2}))?\s*$/gm;
  const entries = [];
  let match;
  while ((match = headingPattern.exec(text)) !== null) {
    const [heading, version, headingDate] = match;
    try {
      parseReleaseVersion(version);
    } catch {
      fail(`${changelogPath}: release version is not canonical: ${version}`);
    }
    const start = match.index + heading.length;
    const next = text.slice(start).search(/^## \d+\.\d+\.\d+(?: - \d{4}-\d{2}-\d{2})?\s*$/m);
    const end = next === -1 ? text.length : start + next;
    let body = text.slice(start, end).trim();
    let date = headingDate;
    if (!date) {
      const bodyDate = body.match(/^(?:Released:\s*)?(\d{4}-\d{2}-\d{2})\s*\n+/);
      if (!bodyDate) {
        fail(`${changelogPath}: release ${version} is missing a release date`);
      }
      date = bodyDate[1];
      body = body.slice(bodyDate[0].length).trim();
    }
    const sourceCommit = body.match(
      /^- Source commit: \[[^\]]+\]\(https:\/\/github\.com\/ctxrs\/ctx\/commit\/([0-9a-f]{40})\)/m,
    );
    assertReleaseBody(version, body);
    entries.push({
      version,
      date,
      body,
      sourceCommit: sourceCommit ? sourceCommit[1] : "",
      tag: `v${version}`,
    });
  }
  if (entries.length === 0) {
    fail(`${changelogPath}: no release headings found`);
  }
  return entries.sort((left, right) => compareReleaseVersions(left.version, right.version));
}

function assertReleaseBody(version, body) {
  const forbidden = [
    /\bbefore release\b/i,
    /\bbumped the CLI\/runtime crates\b/i,
    /\bbumped the version\b/i,
  ];
  for (const pattern of forbidden) {
    if (pattern.test(body)) {
      fail(`release ${version} notes contain internal release-process phrasing: ${pattern}`);
    }
  }
  const sectionPattern = /^### ([^\n]+)\n/gm;
  let match;
  const sections = [];
  while ((match = sectionPattern.exec(body)) !== null) {
    sections.push({ title: match[1], headingStart: match.index, bodyStart: match.index + match[0].length });
  }
  for (let index = 0; index < sections.length; index += 1) {
    const section = sections[index];
    const end = index + 1 < sections.length ? sections[index + 1].headingStart : body.length;
    if (body.slice(section.bodyStart, end).trim() === "") {
      fail(`release ${version} notes contain an empty ${section.title} section`);
    }
  }
}

function selectEntries(entries, options, repo) {
  const requested = options.versions.length > 0
    ? new Set(options.versions)
    : new Set([parseCargoVersion(repo)]);
  let selected = entries.filter((entry) => requested.has(entry.version));
  if (options.from || options.to) {
    selected = entries.filter((entry) => {
      if (options.from && compareReleaseVersions(entry.version, options.from) < 0) {
        return false;
      }
      if (options.to && compareReleaseVersions(entry.version, options.to) > 0) {
        return false;
      }
      return true;
    });
  }
  const found = new Set(selected.map((entry) => entry.version));
  for (const version of requested) {
    if (!found.has(version) && !options.from && !options.to) {
      fail(`changelog source has no entry for ${version}`);
    }
  }
  if (selected.length === 0) {
    fail("no changelog entries matched the requested version range");
  }
  for (const entry of selected) {
    if (!entry.sourceCommit) {
      fail(`selected release ${entry.version} is missing a Source commit link`);
    }
  }
  return selected;
}

function writeNotes(entry, directory) {
  const notesPath = path.join(directory, `${entry.tag}.md`);
  fs.writeFileSync(notesPath, `Released: ${entry.date}\n\n${entry.body}\n`);
  return notesPath;
}

function assertCommitOnGitHub(repoName, commit) {
  run("gh", ["api", "--silent", `repos/${repoName}/commits/${commit}`]);
}

function assertLatestRelease(repoName, tag) {
  const latest = run("gh", ["api", `repos/${repoName}/releases/latest`, "--jq", ".tag_name"]);
  if (latest !== tag) {
    fail(`${tag} is final but GitHub latest is ${latest || "unavailable"}`);
  }
}

function releaseView(repoName, tag) {
  const result = tryRun("gh", [
    "release",
    "view",
    tag,
    "--repo",
    repoName,
    "--json",
    "assets,body,isDraft,isPrerelease,name,tagName,targetCommitish,url",
    "--jq",
    ".",
  ]);
  if (result.status !== 0) {
    return null;
  }
  return JSON.parse(result.stdout);
}

function requireRemoteAnnotatedTag(repoName, tag, expectedCommit) {
  const refResult = tryRun("gh", ["api", `repos/${repoName}/git/ref/tags/${tag}`]);
  if (refResult.status !== 0) {
    fail(`${tag} must already exist in ${repoName} before release publication`);
  }
  let ref;
  try {
    ref = JSON.parse(refResult.stdout);
  } catch {
    fail(`${tag} ref response from ${repoName} is invalid`);
  }
  if (ref?.object?.type !== "tag" || !/^[0-9a-f]{40}$/.test(ref.object.sha || "")) {
    fail(`${tag} must be an annotated tag in ${repoName}`);
  }
  const tagResult = tryRun("gh", ["api", `repos/${repoName}/git/tags/${ref.object.sha}`]);
  if (tagResult.status !== 0) {
    fail(`${tag} annotated tag object cannot be read from ${repoName}`);
  }
  let annotated;
  try {
    annotated = JSON.parse(tagResult.stdout);
  } catch {
    fail(`${tag} annotated tag response from ${repoName} is invalid`);
  }
  if (annotated?.object?.type !== "commit" || annotated.object.sha !== expectedCommit) {
    fail(`${tag} peels to ${annotated?.object?.sha || "an invalid object"}, expected ${expectedCommit}`);
  }
}

function sha256File(filePath) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(filePath));
  return hash.digest("hex");
}

function releaseAssets(configured) {
  const directory = path.resolve(configured);
  const directoryStat = fs.lstatSync(directory, { throwIfNoEntry: false });
  if (!directoryStat || !directoryStat.isDirectory() || directoryStat.isSymbolicLink()) {
    fail(`--asset-dir must be a non-symlink directory: ${directory}`);
  }
  const actualNames = fs.readdirSync(directory).sort();
  const expectedNames = [...RELEASE_ASSET_NAMES].sort();
  if (actualNames.join("\0") !== expectedNames.join("\0")) {
    fail(`release asset inventory must be exactly: ${expectedNames.join(", ")}`);
  }
  const assets = RELEASE_ASSET_NAMES.map((name) => {
    const filePath = path.join(directory, name);
    const stat = fs.lstatSync(filePath);
    if (!stat.isFile() || stat.isSymbolicLink()) {
      fail(`release asset must be a regular non-symlink file: ${filePath}`);
    }
    return { digest: sha256File(filePath), name, path: filePath, size: stat.size };
  });
  const expectedSums = new Map();
  for (const line of fs.readFileSync(path.join(directory, "SHA256SUMS"), "utf8").split(/\r?\n/)) {
    if (!line) {
      continue;
    }
    const match = line.match(/^([0-9a-f]{64})  ([^/]+)$/);
    if (!match || expectedSums.has(match[2])) {
      fail("SHA256SUMS must contain canonical, unique two-space entries");
    }
    expectedSums.set(match[2], match[1]);
  }
  if ([...expectedSums.keys()].sort().join("\0") !== [...RELEASE_PAYLOAD_NAMES].sort().join("\0")) {
    fail("SHA256SUMS inventory does not match the qualified release payloads");
  }
  for (const asset of assets.filter((candidate) => candidate.name !== "SHA256SUMS")) {
    if (expectedSums.get(asset.name) !== asset.digest) {
      fail(`SHA256SUMS digest mismatch for ${asset.name}`);
    }
  }
  return assets;
}

function assertReleaseIdentity(existing, entry, expectedPrerelease) {
  if (existing.tagName !== entry.tag || existing.name !== entry.tag || existing.isDraft) {
    fail(`${entry.tag} release metadata differs from the approved non-draft release`);
  }
  if (expectedPrerelease != null && existing.isPrerelease !== expectedPrerelease) {
    fail(`${entry.tag} release prerelease state differs from the expected state`);
  }
  const expectedBody = `Released: ${entry.date}\n\n${entry.body}\n`;
  if (existing.body !== expectedBody) {
    fail(`${entry.tag} release notes differ from the approved changelog entry`);
  }
}

function assertRemoteAssets(repoName, tag, existing, localAssets, allowMissing = false) {
  const remoteAssets = [...existing.assets].sort((left, right) => left.name.localeCompare(right.name));
  const localSorted = [...localAssets].sort((left, right) => left.name.localeCompare(right.name));
  const localByName = new Map(localSorted.map((asset) => [asset.name, asset]));
  const remoteNames = remoteAssets.map((asset) => asset.name);
  const remoteNameSet = new Set(remoteNames);
  if (remoteNameSet.size !== remoteNames.length || remoteNames.some((name) => !localByName.has(name))) {
    fail(`${tag} GitHub asset inventory differs from the qualified release assets`);
  }
  const missing = localSorted.filter((asset) => !remoteNameSet.has(asset.name));
  if (!allowMissing && missing.length > 0) {
    fail(`${tag} GitHub asset inventory differs from the qualified release assets`);
  }
  const remotePairs = remoteAssets.map((asset) => ({ remote: asset, local: localByName.get(asset.name) }));
  const downloadDir = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-github-release-assets-"));
  try {
    for (const { local } of remotePairs) {
      run("gh", [
        "release", "download", tag,
        "--repo", repoName,
        "--dir", downloadDir,
        "--pattern", local.name,
      ]);
      const downloaded = path.join(downloadDir, local.name);
      const stat = fs.lstatSync(downloaded, { throwIfNoEntry: false });
      if (!stat || !stat.isFile() || stat.isSymbolicLink() ||
          stat.size !== local.size || sha256File(downloaded) !== local.digest) {
        fail(`${tag} GitHub asset bytes differ for ${local.name}`);
      }
    }
  } finally {
    fs.rmSync(downloadDir, { recursive: true, force: true });
  }
  return missing;
}

function verifyRelease(repoName, entry, localAssets, expectedPrerelease = null) {
  requireRemoteAnnotatedTag(repoName, entry.tag, entry.sourceCommit);
  const existing = releaseView(repoName, entry.tag);
  if (!existing) {
    fail(`${entry.tag} GitHub Release does not exist`);
  }
  assertReleaseIdentity(existing, entry, expectedPrerelease);
  assertRemoteAssets(repoName, entry.tag, existing, localAssets);
  return existing;
}

function stagePrerelease(entry, options, notesPath, localAssets) {
  requireRemoteAnnotatedTag(options.repo, entry.tag, entry.sourceCommit);
  const existing = releaseView(options.repo, entry.tag);
  if (existing) {
    assertReleaseIdentity(existing, entry);
    if (!existing.isPrerelease) {
      assertRemoteAssets(options.repo, entry.tag, existing, localAssets);
      assertLatestRelease(options.repo, entry.tag);
    } else {
      const missing = assertRemoteAssets(options.repo, entry.tag, existing, localAssets, true);
      if (missing.length > 0) {
        if (!options.apply) {
          console.log(`would upload missing ${entry.tag} assets: ${missing.map((asset) => asset.name).join(", ")}`);
          return;
        }
        run("gh", [
          "release", "upload", entry.tag,
          "--repo", options.repo,
          ...missing.map((asset) => asset.path),
        ], { stdio: "inherit" });
        verifyRelease(options.repo, entry, localAssets, true);
      }
    }
    console.log(`${existing.isPrerelease ? "staged" : "final"} ${entry.tag} ${existing.url}`);
    return;
  }
  if (!options.apply) {
    console.log(`would stage prerelease ${entry.tag} -> ${entry.sourceCommit.slice(0, 8)}`);
    return;
  }
  run("gh", [
    "release", "create", entry.tag,
    "--repo", options.repo,
    "--verify-tag",
    "--title", entry.tag,
    "--notes-file", notesPath,
    "--prerelease",
  ], { stdio: "inherit" });
  const created = releaseView(options.repo, entry.tag);
  if (!created) {
    fail(`${entry.tag} prerelease creation did not read back`);
  }
  assertReleaseIdentity(created, entry, true);
  run("gh", [
    "release", "upload", entry.tag,
    "--repo", options.repo,
    ...localAssets.map((asset) => asset.path),
  ], { stdio: "inherit" });
  const verified = verifyRelease(options.repo, entry, localAssets, true);
  console.log(`staged ${entry.tag} ${verified.url}`);
}

function promoteFinal(entry, options, localAssets) {
  const existing = verifyRelease(options.repo, entry, localAssets);
  if (!existing.isPrerelease) {
    assertLatestRelease(options.repo, entry.tag);
    console.log(`final ${entry.tag} ${existing.url}`);
    return;
  }
  if (!options.apply) {
    console.log(`would promote ${entry.tag} to final/latest`);
    return;
  }
  run("gh", [
    "release", "edit", entry.tag,
    "--repo", options.repo,
    "--verify-tag",
    "--prerelease=false",
    "--latest",
  ], { stdio: "inherit" });
  const verified = verifyRelease(options.repo, entry, localAssets, false);
  assertLatestRelease(options.repo, entry.tag);
  console.log(`final ${entry.tag} ${verified.url}`);
}

function syncRelease(entry, options, notesPath, localAssets) {
  assertCommitOnGitHub(options.repo, entry.sourceCommit);
  if (options.stagePrerelease) {
    stagePrerelease(entry, options, notesPath, localAssets);
    return;
  }
  if (options.promoteFinal) {
    promoteFinal(entry, options, localAssets);
    return;
  }
  const existing = releaseView(options.repo, entry.tag);
  if (existing) {
    if (options.apply) {
      requireRemoteAnnotatedTag(options.repo, entry.tag, entry.sourceCommit);
    }
    if (!options.updateExisting) {
      console.log(`exists ${entry.tag} ${existing.url}`);
      return;
    }
    if (!options.apply) {
      console.log(`would update ${entry.tag} notes from ${notesPath}`);
      return;
    }
    run("gh", [
      "release",
      "edit",
      entry.tag,
      "--repo",
      options.repo,
      "--verify-tag",
      "--title",
      entry.tag,
      "--notes-file",
      notesPath,
    ], { stdio: "inherit" });
    console.log(`updated ${entry.tag}`);
    return;
  }
  console.log(`missing ${entry.tag}; use --stage-prerelease with the exact qualified release assets`);
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  run("gh", ["--version"]);
  const repo = publicRepo(options.publicRepo);
  const changelogPath = options.changelog
    ? path.resolve(options.changelog)
    : path.join(ROOT, "docs-content/changelog.mdx");
  const entries = selectEntries(parseChangelog(changelogPath), options, repo);
  const localAssets = options.assetDir ? releaseAssets(options.assetDir) : null;
  const notesDir = fs.mkdtempSync(path.join(os.tmpdir(), "ctx-github-release-notes-"));
  console.log(`${options.apply ? "apply" : "dry-run"} ${options.repo} from ${changelogPath}`);
  for (const entry of entries) {
    const notesPath = writeNotes(entry, notesDir);
    syncRelease(entry, options, notesPath, localAssets);
  }
  console.log(`notes: ${notesDir}`);
}

main();
