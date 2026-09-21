// Assertions shared by shell and PowerShell installer contract tests.
import {
  assert,
  existsSync,
  fileURLToPath,
  path,
  readFileSync,
} from "./cli-install-test-helpers.mjs";

export function assertRuntimeRepairUsesVerifiedMetadata(runtimeRepairLogPath, installerTmpRoot) {
  const [command, flag, channel, metadataUri, signatureUri] = readFileSync(
    runtimeRepairLogPath,
    "utf8",
  ).trim().split("|");
  assert.deepEqual([command, flag, channel], ["upgrade", "--channel", "stable"]);
  for (const [uri, basename] of [
    [metadataUri, "metadata.env"],
    [signatureUri, "metadata.env.sig"],
  ]) {
    assert.match(uri, /^file:\/\//);
    const localPath = fileURLToPath(uri);
    assert.equal(path.basename(localPath), basename);
    assert.equal(path.basename(path.dirname(localPath)), "final");
    assert.equal(path.dirname(path.dirname(path.dirname(localPath))), installerTmpRoot);
  }
}

export function readStageReports(filePath) {
  if (!existsSync(filePath)) return [];
  return readFileSync(filePath, "utf8")
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}
