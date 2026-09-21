import { ADE_FUNCTIONS_BASE, renderAdeInstallScript } from "./ade-install-script.js";
import { renderCliInstallScript } from "./cli-install-script.js";
import { renderCliInstallPowerShellScript } from "./cli-install-powershell-script.js";
import { renderCliUninstallPowerShellScript } from "./cli-uninstall-powershell-script.js";
import { renderUninstallScript } from "./uninstall-script.js";
import { generateInstallAttemptId } from "./install-attempt-id.js";

const DOWNLOAD_ID_PATTERN = /^[A-Za-z0-9._:-]{1,64}$/;
const ATTRIBUTION_VALUE_PATTERN = /[^A-Za-z0-9._:-]/g;
const MAX_ATTRIBUTION_VALUE_LENGTH = 120;
const MAX_STAGING_INSTALLER_BYTES = 1024 * 1024;
const MAX_STAGING_BUNDLE_OBJECT_BYTES = 64 * 1024 * 1024;
const STAGING_INSTALLER_OBJECT_KEY_PATTERN =
  /^installers\/dogfood-[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\/(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\/install\.sh$/u;
const STAGING_BUNDLE_OBJECT_PREFIX_PATTERN =
  /^bundles\/dogfood-[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\/(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\/$/u;
const STAGING_BUNDLE_RELATIVE_OBJECT_PATTERN =
  /^(?:metadata\.env(?:\.sig)?|managed-pair-envelope\.json|sha256\/[0-9a-f]{64}\/[A-Za-z0-9][A-Za-z0-9._+-]{0,127})$/u;

function shell(script, status = 200) {
  return new Response(script, {
    status,
    headers: {
      "content-type": "text/x-shellscript; charset=utf-8",
      "cache-control": "no-store",
    },
  });
}

function powershell(script, status = 200) {
  return new Response(script, {
    status,
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "cache-control": "no-store",
    },
  });
}

function html(body, status = 200) {
  return new Response(body, {
    status,
    headers: {
      "content-type": "text/html; charset=utf-8",
      "cache-control": "no-store",
    },
  });
}

function plain(body, status) {
  return new Response(body, {
    status,
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
    },
  });
}

const ADE_INSTALL_ROUTES = new Set(["/install", "/install.sh"]);
const CLI_INSTALL_ROUTES = new Set(["/install", "/install.sh", "/install/cli", "/install/cli.sh"]);
const CLI_POWERSHELL_INSTALL_ROUTES = new Set(["/install.ps1", "/install/cli.ps1"]);
const UNINSTALL_ROUTES = new Set(["/uninstall", "/uninstall.sh"]);
const POWERSHELL_UNINSTALL_ROUTES = new Set(["/uninstall.ps1", "/uninstall/cli.ps1"]);

function isCliInstallHost(hostname) {
  return hostname === "ctx.rs"
    || hostname === "cli.ctx.rs"
    || hostname === "localhost"
    || hostname === "127.0.0.1";
}

function isAdeInstallHost(hostname) {
  return hostname === "ade.ctx.rs";
}

function isWorkersDevHost(hostname) {
  return hostname.endsWith(".workers.dev");
}

function hasStagingInstallerConfiguration(env) {
  return env.STAGING_INSTALLER_OBJECT_KEY !== undefined
    || env.STAGING_RELEASES_BUCKET !== undefined;
}

export function isApprovedStagingInstallerObjectKey(value) {
  return typeof value === "string"
    && STAGING_INSTALLER_OBJECT_KEY_PATTERN.test(value);
}

export function isApprovedStagingBundleObjectPrefix(value) {
  return typeof value === "string"
    && STAGING_BUNDLE_OBJECT_PREFIX_PATTERN.test(value);
}

function isApprovedStagingBundleRelativeObject(value) {
  return STAGING_BUNDLE_RELATIVE_OBJECT_PATTERN.test(value);
}

async function stagingInstaller(env) {
  const key = env.STAGING_INSTALLER_OBJECT_KEY;
  const bucket = env.STAGING_RELEASES_BUCKET;
  if (
    !isApprovedStagingInstallerObjectKey(key)
    || bucket == null
    || typeof bucket.get !== "function"
  ) {
    return plain("Staging installer is unavailable.\n", 503);
  }

  let object;
  try {
    object = await bucket.get(key);
  } catch {
    return plain("Staging installer is unavailable.\n", 503);
  }
  if (object == null) {
    return plain("Staging installer was not found.\n", 404);
  }
  if (
    object.body == null
    || !Number.isSafeInteger(object.size)
    || object.size < 1
    || object.size > MAX_STAGING_INSTALLER_BYTES
  ) {
    return plain("Staging installer is unavailable.\n", 503);
  }

  return new Response(object.body, {
    status: 200,
    headers: {
      "content-type": "text/x-shellscript; charset=utf-8",
      "content-length": String(object.size),
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
    },
  });
}

async function stagingBundleObject(pathname, env) {
  const prefix = env.STAGING_BUNDLE_OBJECT_PREFIX;
  const bucket = env.STAGING_RELEASES_BUCKET;
  const relative = pathname.slice("/bundle/".length);
  if (
    !isApprovedStagingBundleObjectPrefix(prefix)
    || !isApprovedStagingBundleRelativeObject(relative)
    || bucket == null
    || typeof bucket.get !== "function"
  ) {
    return plain("Staging bundle object is unavailable.\n", 404);
  }

  let object;
  try {
    object = await bucket.get(`${prefix}${relative}`);
  } catch {
    return plain("Staging bundle object is unavailable.\n", 503);
  }
  if (object == null) {
    return plain("Staging bundle object was not found.\n", 404);
  }
  if (
    object.body == null
    || !Number.isSafeInteger(object.size)
    || object.size < 1
    || object.size > MAX_STAGING_BUNDLE_OBJECT_BYTES
  ) {
    return plain("Staging bundle object is unavailable.\n", 503);
  }

  const contentType = relative === "metadata.env" || relative === "metadata.env.sig"
    ? "text/plain; charset=utf-8"
    : relative === "managed-pair-envelope.json"
      ? "application/json"
      : "application/octet-stream";
  return new Response(object.body, {
    status: 200,
    headers: {
      "content-type": contentType,
      "content-length": String(object.size),
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
    },
  });
}

function normalizeDownloadId(raw) {
  if (!raw) return null;
  const trimmed = raw.trim();
  if (!DOWNLOAD_ID_PATTERN.test(trimmed)) return null;
  return trimmed;
}

function normalizeAttributionValue(raw) {
  if (!raw) return null;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const normalized = trimmed.replace(ATTRIBUTION_VALUE_PATTERN, "_").slice(0, MAX_ATTRIBUTION_VALUE_LENGTH);
  return normalized || null;
}

function normalizeReferrerDomain(raw) {
  if (!raw) return null;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  try {
    const parsed = /^[a-zA-Z][a-zA-Z\d+\-.]*:/.test(trimmed)
      ? new URL(trimmed)
      : new URL(`https://${trimmed}`);
    const hostname = parsed.hostname.trim().toLowerCase().replace(/\.+$/, "");
    return hostname || null;
  } catch {
    return null;
  }
}

export default {
  async fetch(request, env = {}) {
    const url = new URL(request.url);
    const pathname = url.pathname.replace(/\/+$/, "") || "/";
    const hostname = url.hostname.toLowerCase();

    if (
      request.method === "GET"
      && isWorkersDevHost(hostname)
      && hasStagingInstallerConfiguration(env)
      && pathname.startsWith("/bundle/")
    ) {
      return stagingBundleObject(pathname, env);
    }

    if (
      request.method === "GET"
      && isWorkersDevHost(hostname)
      && hasStagingInstallerConfiguration(env)
      && ADE_INSTALL_ROUTES.has(pathname)
    ) {
      return stagingInstaller(env);
    }

    if (request.method === "GET" && isCliInstallHost(hostname) && CLI_INSTALL_ROUTES.has(pathname)) {
      const installUrl = hostname === "cli.ctx.rs" ? "https://cli.ctx.rs/install" : "https://ctx.rs/install";
      return shell(renderCliInstallScript({
        installAttemptId: generateInstallAttemptId(),
        installUrl,
      }), 200);
    }

    if (request.method === "GET" && isCliInstallHost(hostname) && CLI_POWERSHELL_INSTALL_ROUTES.has(pathname)) {
      return powershell(renderCliInstallPowerShellScript({
        installAttemptId: generateInstallAttemptId(),
      }), 200);
    }

    if (request.method === "GET" && isAdeInstallHost(hostname) && ADE_INSTALL_ROUTES.has(pathname)) {
      return shell(renderAdeInstallScript({
        functionsBase: ADE_FUNCTIONS_BASE,
        downloadId: normalizeDownloadId(url.searchParams.get("ctx_download_id")) ?? crypto.randomUUID(),
        referrerDomain: normalizeReferrerDomain(url.searchParams.get("referrer_domain"))
          ?? normalizeReferrerDomain(request.headers.get("referer")),
        utmSource: normalizeAttributionValue(url.searchParams.get("utm_source")),
        utmMedium: normalizeAttributionValue(url.searchParams.get("utm_medium")),
        utmCampaign: normalizeAttributionValue(url.searchParams.get("utm_campaign")),
      }), 200);
    }

    if (request.method === "GET" && UNINSTALL_ROUTES.has(pathname)) {
      return shell(renderUninstallScript({
        installAttemptId: generateInstallAttemptId(),
      }), 200);
    }

    if (request.method === "GET" && isCliInstallHost(hostname) && POWERSHELL_UNINSTALL_ROUTES.has(pathname)) {
      return powershell(renderCliUninstallPowerShellScript({
        installAttemptId: generateInstallAttemptId(),
      }), 200);
    }

    if (request.method === "GET" && pathname === "/") {
      if (isCliInstallHost(hostname)) {
        return html(
          `<!doctype html><html><body><p>ctx CLI install endpoint</p><p>CLI: <code>curl -fsSL https://ctx.rs/install | sh</code></p></body></html>`,
        );
      }
      return html(
        `<!doctype html><html><body><p>ctx install endpoint</p><p>CLI: <code>curl -fsSL https://ctx.rs/install | sh</code></p><p>Uninstall: <code>curl -fsSL https://ctx.rs/uninstall | sh</code></p></body></html>`,
      );
    }

    return html("<!doctype html><html><body><h1>Not Found</h1></body></html>", 404);
  },
};
