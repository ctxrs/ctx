import assert from "node:assert/strict";
import { renderCliInstallScript } from "../cli-install-script.js";
import { renderCliInstallPowerShellScript } from "../cli-install-powershell-script.js";

// Extract actual production functions; native tests use these same bodies.
export function shellAcquisitionFixture() {
  const body = renderCliInstallScript();
  const start = body.indexOf("write_bounded_file() {");
  const end = body.indexOf("write_metadata_public_key() {", start);
  assert.ok(start >= 0 && end > start);
  return body.slice(start, end);
}

export function powerShellAcquisitionFixture() {
  const body = renderCliInstallPowerShellScript();
  return ["Copy-LimitedStream", "Copy-LimitedFile", "Read-HttpsFile", "Read-Metadata",
    "Read-DetachedSignature", "Read-Artifact", "Expand-GzipFile"].map((name) => {
    const matches = [...body.matchAll(new RegExp(`^function ${name}\\b[^\\n]*[\\s\\S]*?^}`, "gm"))];
    assert.equal(matches.length, 1, `missing function ${name}`);
    return matches[0][0];
  }).join("\n");
}
