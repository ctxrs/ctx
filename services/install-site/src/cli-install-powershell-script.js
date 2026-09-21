import {
  generateInstallAttemptId,
  normalizeEmbeddedInstallAttemptId,
} from "./install-attempt-id.js";
import { CLI_INSTALL_POWERSHELL_PLATFORM } from "./cli-install-powershell-platform.js";
import { renderCliInstallPowerShellRendering } from "./cli-install-powershell-rendering.js";
import { renderCliInstallPowerShellValidation } from "./cli-install-powershell-validation.js";
import { renderCliInstallPowerShellWorkflow } from "./cli-install-powershell-workflow.js";

const DEFAULT_FUNCTIONS_BASE = "https://cli.ctx.rs/functions/v2";
const DEFAULT_CHANNEL = "stable";
const DEFAULT_METADATA_PUBLIC_KEY_MODULUS_BASE64URL =
  "yBPNIx3H_NwWlN9CPHY5kOEe9kQEshOJEMpv3Atq086H1FWqliTm3BCWiO4s_89wNMn11Pla2JetCWNiWsbxm3BIxCd1o6cq8y9ur6Zk1RGOQBLQgqhFm5BpcTTavhtlc3FdV2KSm2UU1IEJAiFXJyMlbgmf3tXfO8Cji_3mG11rWCXfnEzXJmig5_WWA21ZgsafPJGH9ow7FsLok5G1kvOeVDXcv0gzmxWH-2O40kCGWo7BK7P_2DPD2GbXc81Mf6S7vWi7CeFiBeGH8EGZ6MgBM0UnAFEqtx_WvY47O-LHzFrGlJTpss3xlxsSQOTmXDJdOzmQVi04GkbOtBEl-dIyYsxZGusLBMGDqkZekO4Z5LvqA8zHt4JAElZCs8SGTlV70MSlnyZb5_rkKx9kMvb7YjuYbY6vnN5Pp3P7gMhOKehP-62U80cgyj1m6Sk5bByrs54ne2mM-cwNXXgKp5UntmkefDcfKP7MmISy93U_kg3fWojE_a-X6TNV_k5f";
const DEFAULT_METADATA_PUBLIC_KEY_EXPONENT_BASE64URL = "AQAB";

export function renderCliInstallPowerShellScript({
  functionsBase = DEFAULT_FUNCTIONS_BASE,
  channel = DEFAULT_CHANNEL,
  installAttemptId = generateInstallAttemptId(),
  metadataPublicKeyModulusBase64Url = DEFAULT_METADATA_PUBLIC_KEY_MODULUS_BASE64URL,
  metadataPublicKeyExponentBase64Url = DEFAULT_METADATA_PUBLIC_KEY_EXPONENT_BASE64URL,
} = {}) {
  const normalizedBase = String(functionsBase).replace(/\/+$/, "");
  const normalizedChannel = String(channel);
  const normalizedInstallAttemptId = normalizeEmbeddedInstallAttemptId(installAttemptId);
  const normalizedMetadataPublicKeyModulusBase64Url = String(
    metadataPublicKeyModulusBase64Url,
  ).trim();
  const normalizedMetadataPublicKeyExponentBase64Url = String(
    metadataPublicKeyExponentBase64Url,
  ).trim();

  return [
    renderCliInstallPowerShellRendering(),
    renderCliInstallPowerShellValidation({
      normalizedBase,
      normalizedChannel,
      normalizedInstallAttemptId,
      normalizedMetadataPublicKeyModulusBase64Url,
      normalizedMetadataPublicKeyExponentBase64Url,
    }),
    CLI_INSTALL_POWERSHELL_PLATFORM,
    renderCliInstallPowerShellWorkflow(),
  ].join("");
}
