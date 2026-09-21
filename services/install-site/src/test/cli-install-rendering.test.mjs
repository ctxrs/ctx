// Renderer-level contracts shared by the public shell families.
import {
  assert,
  assertLinesAtMost,
  renderCliInstallPowerShellScript,
  renderCliInstallScript,
  readFileSync,
  test,
} from "./cli-install-test-helpers.mjs";

test("hosted Windows marker carries pair provenance only for paired metadata", () => {
  const powershell = renderCliInstallPowerShellScript();
  const markerStart = powershell.indexOf("    $marker = [ordered]@{");
  const markerWrite = powershell.indexOf("    $markerJson = $marker | ConvertTo-Json", markerStart);
  assert.ok(markerStart >= 0 && markerWrite > markerStart);
  const marker = powershell.slice(markerStart, markerWrite);
  assert.match(marker, /if \(\$managedPair\) \{\s*\$marker\.managed_pair = \$true\s*\}/u);
  assert.equal(marker.match(/managed_pair/gu)?.length, 1);
});

test("complete rendered Windows installer has an ASCII saved-script contract", () => {
  const powershell = renderCliInstallPowerShellScript({
    installAttemptId: "ia_ascii_contract",
  });
  const bytes = Buffer.from(powershell, "utf8");
  const nonAsciiOffset = bytes.findIndex((byte) => byte > 0x7f);
  assert.equal(
    nonAsciiOffset,
    -1,
    nonAsciiOffset < 0
      ? ""
      : `rendered install.ps1 contains a non-ASCII byte at offset ${nonAsciiOffset}`,
  );

});
