const INSTALL_ATTEMPT_ID_PREFIX = "ia_";
const INSTALL_ATTEMPT_ID_BYTES = 18;
const INSTALL_ATTEMPT_ID_PATTERN = /^ia_[A-Za-z0-9_-]{8,128}$/;

export function generateInstallAttemptId() {
  const bytes = new Uint8Array(INSTALL_ATTEMPT_ID_BYTES);
  crypto.getRandomValues(bytes);
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return `${INSTALL_ATTEMPT_ID_PREFIX}${btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "")}`;
}

export function normalizeEmbeddedInstallAttemptId(value) {
  const normalized = String(value).trim();
  return INSTALL_ATTEMPT_ID_PATTERN.test(normalized)
    ? normalized
    : generateInstallAttemptId();
}
