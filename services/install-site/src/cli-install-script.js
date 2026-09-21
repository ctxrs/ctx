import {
  generateInstallAttemptId,
  normalizeEmbeddedInstallAttemptId,
} from "./install-attempt-id.js";
import { CLI_INSTALL_SHELL_INTEGRATIONS } from "./cli-install-shell-integrations.js";
import { CLI_INSTALL_SHELL_MAN_PAGES } from "./cli-install-shell-man-pages.js";
import { renderCliInstallShellPlatform } from "./cli-install-shell-platform.js";
import { renderCliInstallShellRendering } from "./cli-install-shell-rendering.js";
import { CLI_INSTALL_SHELL_VALIDATION } from "./cli-install-shell-validation.js";
import { renderCliInstallShellWorkflow } from "./cli-install-shell-workflow.js";

const DEFAULT_RELEASE_FUNCTIONS_BASE = "https://cli.ctx.rs/functions/v2";
const DEFAULT_INSTALL_TELEMETRY_ENDPOINT =
  "https://cli.ctx.rs/functions/v1/install-attempt";
const DEFAULT_CHANNEL = "stable";
const DEFAULT_INSTALL_URL = "https://ctx.rs/install";
const DEFAULT_METADATA_PUBLIC_KEY_PEM = `-----BEGIN PUBLIC KEY-----
MIIBojANBgkqhkiG9w0BAQEFAAOCAY8AMIIBigKCAYEAyBPNIx3H/NwWlN9CPHY5
kOEe9kQEshOJEMpv3Atq086H1FWqliTm3BCWiO4s/89wNMn11Pla2JetCWNiWsbx
m3BIxCd1o6cq8y9ur6Zk1RGOQBLQgqhFm5BpcTTavhtlc3FdV2KSm2UU1IEJAiFX
JyMlbgmf3tXfO8Cji/3mG11rWCXfnEzXJmig5/WWA21ZgsafPJGH9ow7FsLok5G1
kvOeVDXcv0gzmxWH+2O40kCGWo7BK7P/2DPD2GbXc81Mf6S7vWi7CeFiBeGH8EGZ
6MgBM0UnAFEqtx/WvY47O+LHzFrGlJTpss3xlxsSQOTmXDJdOzmQVi04GkbOtBEl
+dIyYsxZGusLBMGDqkZekO4Z5LvqA8zHt4JAElZCs8SGTlV70MSlnyZb5/rkKx9k
Mvb7YjuYbY6vnN5Pp3P7gMhOKehP+62U80cgyj1m6Sk5bByrs54ne2mM+cwNXXgK
p5UntmkefDcfKP7MmISy93U/kg3fWojE/a+X6TNV/k5fAgMBAAE=
-----END PUBLIC KEY-----`;
export const CLI_METADATA_PUBLIC_KEY_PEM = DEFAULT_METADATA_PUBLIC_KEY_PEM;

export function renderCliInstallScript({
  releaseFunctionsBase = DEFAULT_RELEASE_FUNCTIONS_BASE,
  installTelemetryEndpoint = DEFAULT_INSTALL_TELEMETRY_ENDPOINT,
  channel = DEFAULT_CHANNEL,
  installUrl = DEFAULT_INSTALL_URL,
  installAttemptId = generateInstallAttemptId(),
  metadataPublicKeyPem = CLI_METADATA_PUBLIC_KEY_PEM,
  stagingDogfood = false,
} = {}) {
  if (typeof stagingDogfood !== "boolean") {
    throw new TypeError("stagingDogfood must be a boolean");
  }
  const normalizedReleaseFunctionsBase =
    String(releaseFunctionsBase).replace(/\/+$/, "");
  const normalizedInstallTelemetryEndpoint =
    String(installTelemetryEndpoint).replace(/\/+$/, "");
  const normalizedChannel = String(channel);
  const normalizedInstallUrl = String(installUrl).replace(/\/+$/, "");
  const normalizedInstallAttemptId = normalizeEmbeddedInstallAttemptId(installAttemptId);
  const normalizedMetadataPublicKeyPem = String(metadataPublicKeyPem).trim();
  const stagingDogfoodMarker = stagingDogfood
    ? '\n  "staging_dogfood": true,'
    : "";
  return [
    renderCliInstallShellRendering({
      normalizedReleaseFunctionsBase,
      normalizedInstallTelemetryEndpoint,
      normalizedChannel,
      normalizedInstallUrl,
      normalizedInstallAttemptId,
    }),
    renderCliInstallShellPlatform({ normalizedMetadataPublicKeyPem }),
    CLI_INSTALL_SHELL_VALIDATION,
    CLI_INSTALL_SHELL_INTEGRATIONS,
    CLI_INSTALL_SHELL_MAN_PAGES,
    renderCliInstallShellWorkflow({
      stagingDogfood,
      stagingDogfoodMarker,
    }),
  ].join("");
}
