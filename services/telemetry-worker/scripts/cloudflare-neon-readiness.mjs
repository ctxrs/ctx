#!/usr/bin/env node

import childProcess from "node:child_process";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import {parseWranglerToml, readWranglerConfig} from "./wrangler-toml.mjs";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const PACKAGE_ROOT = path.resolve(SCRIPT_DIR, "..");
const REPO_ROOT = path.resolve(PACKAGE_ROOT, "..");
const DEFAULT_WRANGLER_CONFIG = path.join(PACKAGE_ROOT, "wrangler.toml");
const DEFAULT_INFISICAL_ENV = "prod";
const DEFAULT_INFISICAL_PATH = "/";
const CLOUDFLARE_API_BASE = "https://api.cloudflare.com/client/v4";
const NEON_API_BASE = "https://console.neon.tech/api/v2";

const COMMON_REQUIRED_ENV = [
  {
    group: "cloudflare_operator",
    name: "CLOUDFLARE_ACCOUNT_ID",
    aliases: ["CF_ACCOUNT_ID"],
    secret: false,
  },
  {
    group: "cloudflare_operator",
    name: "CLOUDFLARE_API_TOKEN",
    aliases: ["CF_API_TOKEN"],
    secret: true,
  },
  {
    group: "neon_operator",
    name: "NEON_API_KEY",
    aliases: ["CTX_NEON_API_KEY"],
    secret: true,
  },
  {
    group: "neon_operator",
    name: "NEON_PROJECT_ID",
    aliases: ["CTX_NEON_PROJECT_ID"],
    secret: false,
  },
];

const RELEASE_API_REQUIRED_ENV = [
  ...COMMON_REQUIRED_ENV,
  {
    group: "release_storage",
    name: "CTX_RELEASES_R2_BUCKET",
    aliases: ["RELEASE_STORAGE_BUCKET", "CTX_RELEASE_R2_BUCKET"],
    secret: false,
  },
  {
    group: "release_storage",
    name: "CTX_RELEASE_STAGING_R2_BUCKET",
    aliases: [],
    secret: false,
  },
  {
    group: "release_storage",
    name: "CTX_RELEASE_STORAGE_BACKEND",
    aliases: ["RELEASE_STORAGE_PROVIDER"],
    secret: false,
  },
];

const TELEMETRY_REQUIRED_ENV = [
  ...COMMON_REQUIRED_ENV,
  {
    group: "telemetry_storage",
    name: "TELEMETRY_DATABASE_URL",
    aliases: ["CTX_TELEMETRY_DATABASE_URL", "CTX_NEON_PROD_TELEMETRY_DATABASE_URL"],
    secret: true,
  },
  {
    group: "telemetry_retention",
    name: "TELEMETRY_RETENTION_DATABASE_URL",
    aliases: [],
    secret: true,
  },
  {
    group: "telemetry_storage",
    name: "TELEMETRY_READ_DATABASE_URL",
    aliases: ["CTX_TELEMETRY_READ_DATABASE_URL", "CTX_NEON_PROD_ANALYTICS_READONLY_DATABASE_URL"],
    secret: true,
  },
  {
    group: "telemetry_storage",
    name: "TELEMETRY_IDENTITY_HMAC_KEY",
    aliases: [],
    secret: true,
  },
  {
    group: "telemetry_queue_health",
    name: "TELEMETRY_QUEUE_HEALTH_API_TOKEN",
    aliases: [],
    secret: true,
  },
];

const RELEASE_API_REQUIRED_WORKER_VARS = [
  "RELEASE_ARTIFACT_REDIRECT_BASE_URL",
];

const TELEMETRY_REQUIRED_WORKER_VARS = [
  "TELEMETRY_ANALYTICS_ENVIRONMENT",
  "TELEMETRY_CLOUDFLARE_ACCOUNT_ID",
  "TELEMETRY_IDENTITY_KEY_VERSION",
];

const RELEASE_API_REQUIRED_WORKER_SECRETS = [];

const TELEMETRY_REQUIRED_WORKER_SECRETS = [
  "TELEMETRY_DATABASE_URL",
  "TELEMETRY_IDENTITY_HMAC_KEY",
  "TELEMETRY_QUEUE_HEALTH_API_TOKEN",
  "TELEMETRY_RETENTION_DATABASE_URL",
];

const TELEMETRY_REQUIRED_CRONS = ["17 3 * * *"];
const TELEMETRY_REQUIRED_RATE_LIMITERS = ["TELEMETRY_RATE_LIMITER"];
const TELEMETRY_REQUIRED_QUEUES = {
  staging: {
    producer: {
      binding: "TELEMETRY_INGEST_QUEUE",
      queue: "ctx-telemetry-ingest-staging",
    },
    consumer: {
      queue: "ctx-telemetry-ingest-staging",
      dead_letter_queue: "ctx-telemetry-ingest-staging-dlq",
      max_batch_size: "10",
      max_batch_timeout: "5",
      max_retries: "10",
      max_concurrency: "4",
    },
  },
  prod: {
    producer: {
      binding: "TELEMETRY_INGEST_QUEUE",
      queue: "ctx-telemetry-ingest-prod",
    },
    consumer: {
      queue: "ctx-telemetry-ingest-prod",
      dead_letter_queue: "ctx-telemetry-ingest-prod-dlq",
      max_batch_size: "10",
      max_batch_timeout: "5",
      max_retries: "10",
      max_concurrency: "4",
    },
  },
};
const TELEMETRY_REQUIRED_ENV_DISTINCTIONS = [
  ["TELEMETRY_DATABASE_URL", "TELEMETRY_RETENTION_DATABASE_URL"],
  ["TELEMETRY_READ_DATABASE_URL", "TELEMETRY_RETENTION_DATABASE_URL"],
];
const TELEMETRY_REQUIRED_DATABASE_ROLES = [
  ["TELEMETRY_DATABASE_URL", "ctx_telemetry_ingest"],
  ["TELEMETRY_READ_DATABASE_URL", "ctx_analytics_readonly"],
  ["TELEMETRY_RETENTION_DATABASE_URL", "ctx_telemetry_retention"],
];

function usage() {
  return `usage: node scripts/cloudflare-neon-readiness.mjs [options]

Safe Cloudflare + Neon readiness check for the public ctx telemetry Worker.

Default behavior is read-only:
  - verify required env/Infisical keys are present without printing values
  - parse wrangler.toml for required Worker vars
  - inspect Cloudflare Worker and script-level secret names when credentials exist
  - inspect Neon project metadata when credentials exist

Options:
  --environment <name>       Worker environment to evaluate (repeatable; default: staging, prod)
  --profile <name>           Readiness profile: telemetry or release-api (default: telemetry)
  --worker-name <name>       Inspect one explicit Worker instead of each selected wrangler environment
  --wrangler-config <path>   Wrangler config path (default: wrangler.toml from the current service root)
  --infisical-env <env>      Infisical environment for local fallback (default: INFISICAL_ENV or prod)
  --infisical-path <path>    Infisical path for local fallback (default: INFISICAL_PATH or /)
  --infisical-project-dir <path>
                             Directory containing .infisical.json (default: services/)
  --no-infisical             Do not query Infisical for missing env values
  --skip-cloudflare          Do not call Cloudflare APIs
  --skip-neon-api            Do not call Neon APIs
  --help                     Show this help
`;
}

function parseArgs(argv) {
  const options = {
    environments: [],
    profile: "telemetry",
    workerName: "",
    wranglerConfig: DEFAULT_WRANGLER_CONFIG,
    infisicalEnv: "",
    infisicalPath: "",
    infisicalProjectDir: REPO_ROOT,
    useInfisical: true,
    inspectCloudflare: true,
    inspectNeonApi: true,
    help: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--help" || arg === "-h") {
      options.help = true;
      continue;
    }
    if (arg === "--environment") {
      options.environments.push(requireArg(argv, ++index, arg));
      continue;
    }
    if (arg === "--profile") {
      options.profile = requireArg(argv, ++index, arg);
      continue;
    }
    if (arg === "--worker-name") {
      options.workerName = requireArg(argv, ++index, arg);
      continue;
    }
    if (arg === "--wrangler-config") {
      options.wranglerConfig = path.resolve(requireArg(argv, ++index, arg));
      continue;
    }
    if (arg === "--infisical-env") {
      options.infisicalEnv = requireArg(argv, ++index, arg);
      continue;
    }
    if (arg === "--infisical-path") {
      options.infisicalPath = requireArg(argv, ++index, arg);
      continue;
    }
    if (arg === "--infisical-project-dir") {
      options.infisicalProjectDir = path.resolve(requireArg(argv, ++index, arg));
      continue;
    }
    if (arg === "--no-infisical") {
      options.useInfisical = false;
      continue;
    }
    if (arg === "--skip-cloudflare") {
      options.inspectCloudflare = false;
      continue;
    }
    if (arg === "--skip-neon-api") {
      options.inspectNeonApi = false;
      continue;
    }
    throw new Error(`unsupported argument: ${arg}`);
  }

  options.environments = normalizeList(options.environments.length > 0 ? options.environments : ["staging", "prod"]);
  options.infisicalEnv = options.infisicalEnv || process.env.INFISICAL_ENV || DEFAULT_INFISICAL_ENV;
  options.infisicalPath = options.infisicalPath || process.env.INFISICAL_PATH || DEFAULT_INFISICAL_PATH;
  if (!["release-api", "telemetry"].includes(options.profile)) {
    throw new Error(`unsupported --profile '${options.profile}'`);
  }
  return options;
}

function requireArg(argv, index, flag) {
  const value = argv[index];
  if (!value || value.startsWith("--")) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
}

function normalizeList(values) {
  return Array.from(new Set(values.map((value) => value.trim()).filter(Boolean))).sort();
}

async function buildReadinessReport({ argv = [], env = process.env, fetchImpl = globalThis.fetch, spawnSync = childProcess.spawnSync } = {}) {
  const options = parseArgs(argv);
  const profile = readinessProfile(options.profile);
  const wrangler = readWranglerConfig(options.wranglerConfig);
  const workerName = options.workerName || wrangler.name || profile.defaultWorkerName;
  const envResolution = resolveRequiredEnv({
    env,
    requiredEnv: profile.requiredEnv,
    useInfisical: options.useInfisical,
    infisicalEnv: options.infisicalEnv,
    infisicalPath: options.infisicalPath,
    infisicalProjectDir: options.infisicalProjectDir,
    spawnSync,
  });
  const values = envResolution.values;
  const envDistinctions = inspectEnvDistinctions(values, profile.requiredEnvDistinctions);
  const databaseRoles = inspectDatabaseRoles(values, profile.requiredDatabaseRoles);
  const wranglerReadiness = buildWranglerReadiness(
    wrangler,
    options.environments,
    profile.requiredWorkerVars,
    profile.requiredWorkerSecrets,
    profile.requiredCrons,
    profile.requiredRateLimiters,
    profile.requiredQueues,
  );
  const cloudflare = options.inspectCloudflare
    ? await inspectCloudflare({
        accountId: getResolvedValue(values, "CLOUDFLARE_ACCOUNT_ID"),
        apiToken: getResolvedValue(values, "CLOUDFLARE_API_TOKEN"),
        requiredWorkerSecrets: profile.requiredWorkerSecrets,
        workerNames: options.workerName
          ? [workerName]
          : wranglerReadiness.report.environments
              .map((environment) => environment.worker_name)
              .filter(Boolean),
        fetchImpl,
      })
    : skipped("disabled_by_flag");
  const neonApi = options.inspectNeonApi
    ? await inspectNeonApi({
        apiKey: getResolvedValue(values, "NEON_API_KEY"),
        projectId: getResolvedValue(values, "NEON_PROJECT_ID"),
        fetchImpl,
      })
    : skipped("disabled_by_flag");
  const neonDatabase = skipped("disabled_by_profile");

  const unresolved = [
    ...wranglerReadiness.unresolved,
    ...cloudflare.unresolved,
    ...neonApi.unresolved,
    ...neonDatabase.unresolved,
  ].sort();
  const failures = [
    ...envResolution.report.missing.map((name) => `missing_env:${name}`),
    ...envDistinctions.failures,
    ...databaseRoles.failures,
    ...wranglerReadiness.failures,
    ...cloudflare.failures,
    ...neonApi.failures,
    ...neonDatabase.failures,
  ].sort();

  return {
    schema_version: 1,
    ok: failures.length === 0,
    failures,
    mode: {
      cloudflare: options.inspectCloudflare ? "read_only" : "skipped",
      neon_api: options.inspectNeonApi ? "read_only" : "skipped",
      profile: options.profile,
    },
    env: {
      ...envResolution.report,
      database_roles: databaseRoles.report,
      equality: envDistinctions.report,
    },
    wrangler: wranglerReadiness.report,
    cloudflare: cloudflare.report,
    neon: {
      api: neonApi.report,
      database: neonDatabase.report,
    },
    unresolved,
  };
}

function readinessProfile(profile) {
  if (profile === "release-api") {
    return {
      defaultWorkerName: "ctx-release-api",
      requiredDatabaseRoles: [],
      requiredEnv: RELEASE_API_REQUIRED_ENV,
      requiredEnvDistinctions: [],
      requiredCrons: [],
      requiredQueues: {},
      requiredRateLimiters: [],
      requiredWorkerSecrets: RELEASE_API_REQUIRED_WORKER_SECRETS,
      requiredWorkerVars: RELEASE_API_REQUIRED_WORKER_VARS,
    };
  }
  return {
    defaultWorkerName: "ctx-telemetry",
    requiredDatabaseRoles: TELEMETRY_REQUIRED_DATABASE_ROLES,
    requiredEnv: TELEMETRY_REQUIRED_ENV,
    requiredEnvDistinctions: TELEMETRY_REQUIRED_ENV_DISTINCTIONS,
    requiredCrons: TELEMETRY_REQUIRED_CRONS,
    requiredQueues: TELEMETRY_REQUIRED_QUEUES,
    requiredRateLimiters: TELEMETRY_REQUIRED_RATE_LIMITERS,
    requiredWorkerSecrets: TELEMETRY_REQUIRED_WORKER_SECRETS,
    requiredWorkerVars: TELEMETRY_REQUIRED_WORKER_VARS,
  };
}

function inspectEnvDistinctions(values, distinctions) {
  const failures = [];
  const report = distinctions.map(([left, right]) => {
    const leftValue = getResolvedValue(values, left);
    const rightValue = getResolvedValue(values, right);
    const equal = leftValue && rightValue ? leftValue === rightValue : null;
    if (equal) failures.push(`env_values_must_differ:${left}:${right}`);
    return { equal, left, right };
  });
  return { failures, report };
}

function inspectDatabaseRoles(values, requiredRoles) {
  const failures = [];
  const report = requiredRoles.map(([name, expectedRole]) => {
    const value = getResolvedValue(values, name);
    const matches = value ? databaseRole(value) === expectedRole : null;
    if (matches === false) failures.push(`database_url_role_mismatch:${name}:${expectedRole}`);
    return { expected_role: expectedRole, matches, name };
  });
  return { failures, report };
}

function databaseRole(value) {
  try {
    const parsed = new URL(value);
    if (parsed.protocol !== "postgres:" && parsed.protocol !== "postgresql:") return "";
    return decodeURIComponent(parsed.username);
  } catch {
    return "";
  }
}

function resolveRequiredEnv({ env, requiredEnv, useInfisical, infisicalEnv, infisicalPath, infisicalProjectDir, spawnSync }) {
  const values = new Map();
  const required = [];
  const missing = [];
  const lookups = [];
  for (const spec of requiredEnv) {
    const resolution = resolveEnvSpec(spec, {
      env,
      useInfisical,
      infisicalEnv,
      infisicalPath,
      infisicalProjectDir,
      spawnSync,
    });
    if (resolution.value) {
      values.set(spec.name, resolution.value);
    } else {
      missing.push(spec.name);
    }
    required.push({
      aliases: spec.aliases,
      group: spec.group,
      name: spec.name,
      present: Boolean(resolution.value),
      secret: spec.secret,
      source: resolution.source,
    });
    if (resolution.lookup) {
      lookups.push(resolution.lookup);
    }
  }
  return {
    values,
    report: {
      infisical: {
        enabled: useInfisical,
        env: infisicalEnv,
        path: infisicalPath,
        project_dir: relativize(infisicalProjectDir),
      },
      required,
      missing: missing.sort(),
      equality: [],
      lookups: lookups.sort((left, right) => left.name.localeCompare(right.name)),
    },
  };
}

function resolveEnvSpec(spec, { env, useInfisical, infisicalEnv, infisicalPath, infisicalProjectDir, spawnSync }) {
  for (const key of [spec.name, ...spec.aliases]) {
    const value = String(env[key] || "").trim();
    if (value) {
      return { value, source: key === spec.name ? "env" : `env:${key}` };
    }
  }
  if (!useInfisical) {
    return { value: "", source: "missing" };
  }
  if (isBuildkiteCi(env)) {
    return {
      value: "",
      source: "missing",
      lookup: {
        name: spec.name,
        source: "infisical",
        status: "skipped_in_buildkite",
      },
    };
  }
  let lastLookup = null;
  for (const key of [spec.name, ...spec.aliases]) {
    const lookup = readInfisicalSecret(key, {
      env,
      infisicalEnv,
      infisicalPath,
      infisicalProjectDir,
      spawnSync,
    });
    lastLookup = lookup;
    if (lookup.value) {
      return {
        value: lookup.value,
        source: key === spec.name ? "infisical" : `infisical:${key}`,
        lookup: {
          name: key,
          source: "infisical",
          status: "present",
        },
      };
    }
  }
  return {
    value: "",
    source: "missing",
    lookup: {
      name: spec.name,
      source: "infisical",
      status: lastLookup?.status || "missing",
    },
  };
}

function readInfisicalSecret(name, { env, infisicalEnv, infisicalPath, infisicalProjectDir, spawnSync }) {
  const args = ["secrets", "get", name, "--plain", "--env", infisicalEnv];
  if (infisicalPath) {
    args.push("--path", infisicalPath);
  }
  const result = spawnSync("infisical", args, {
    cwd: infisicalProjectDir,
    encoding: "utf8",
    env,
  });
  if (result.error) {
    return { value: "", status: "cli_unavailable" };
  }
  if (result.status !== 0) {
    return { value: "", status: "lookup_failed" };
  }
  const value = String(result.stdout || "").trim();
  return { value, status: value ? "present" : "empty" };
}

function isBuildkiteCi(env) {
  return Boolean(String(env.BUILDKITE || env.BUILDKITE_BUILD_ID || "").trim());
}

function getResolvedValue(values, name) {
  return values.get(name) || "";
}

function buildWranglerReadiness(
  wrangler,
  environments,
  requiredWorkerVars,
  requiredWorkerSecrets,
  requiredCrons,
  requiredRateLimiters,
  requiredQueues,
) {
  const failures = [];
  const unresolved = [];
  const envReports = environments.map((environment) => {
    const environmentConfig = wrangler.env[environment];
    const vars = environmentConfig?.vars || {};
    const crons = environmentConfig?.triggers?.crons || [];
    const workerName = environmentConfig?.name || "";
    const rateLimiters = (environmentConfig?.ratelimits || [])
      .map((binding) => binding.name)
      .filter((name) => typeof name === "string");
    const queueProducers = environmentConfig?.queues?.producers || [];
    const queueConsumers = environmentConfig?.queues?.consumers || [];
    const requiredQueue = requiredQueues[environment];
    const missingVars = requiredWorkerVars.filter((name) => !(name in vars)).sort();
    const missingCrons = requiredCrons.filter((cron) => !crons.includes(cron)).sort();
    const missingRateLimiters = requiredRateLimiters
      .filter((name) => !rateLimiters.includes(name)).sort();
    const missingQueueProducers = requiredQueue && !queueProducers.some(
      (producer) => queueBindingMatches(producer, requiredQueue.producer),
    ) ? [`${requiredQueue.producer.binding}=${requiredQueue.producer.queue}`] : [];
    const missingQueueConsumers = requiredQueue && !queueConsumers.some(
      (consumer) => queueBindingMatches(consumer, requiredQueue.consumer),
    ) ? [requiredQueue.consumer.queue] : [];
    if (missingVars.length > 0) {
      failures.push(`wrangler_missing_vars:${environment}:${missingVars.join(",")}`);
    }
    if (missingCrons.length > 0) {
      failures.push(`wrangler_missing_crons:${environment}:${missingCrons.join(",")}`);
    }
    if (missingRateLimiters.length > 0) {
      failures.push(`wrangler_missing_ratelimits:${environment}:${missingRateLimiters.join(",")}`);
    }
    if (missingQueueProducers.length > 0) {
      failures.push(
        `wrangler_missing_queue_producers:${environment}:${missingQueueProducers.join(",")}`,
      );
    }
    if (missingQueueConsumers.length > 0) {
      failures.push(
        `wrangler_missing_queue_consumers:${environment}:${missingQueueConsumers.join(",")}`,
      );
    }
    if (!workerName) failures.push(`wrangler_missing_worker_name:${environment}`);
    return {
      missing_crons: missingCrons,
      missing_vars: missingVars,
      missing_ratelimits: missingRateLimiters,
      missing_queue_producers: missingQueueProducers,
      missing_queue_consumers: missingQueueConsumers,
      name: environment,
      present_crons: [...crons].sort(),
      present_vars: Object.keys(vars).sort(),
      present_ratelimits: [...rateLimiters].sort(),
      present_queue_producers: queueProducers,
      present_queue_consumers: queueConsumers,
      required_crons: [...requiredCrons].sort(),
      required_vars: [...requiredWorkerVars].sort(),
      required_ratelimits: [...requiredRateLimiters].sort(),
      required_queue: requiredQueue || null,
      worker_name: workerName,
    };
  });

  return {
    failures,
    unresolved,
    report: {
      path: relativize(wrangler.path),
      root_crons: [...wrangler.triggers.crons].sort(),
      worker_name: wrangler.name || "",
      root_vars: Object.keys(wrangler.vars).sort(),
      environments: envReports,
      required_secrets: [...requiredWorkerSecrets].sort(),
    },
  };
}

function queueBindingMatches(actual, expected) {
  return Object.entries(expected).every(([key, value]) => String(actual[key]) === String(value));
}

async function inspectCloudflare({ accountId, apiToken, requiredWorkerSecrets, workerNames, fetchImpl }) {
  const report = {
    checked: false,
    workers: workerNames.map((workerName) => emptyWorkerReport(workerName, requiredWorkerSecrets)),
  };
  const failures = [];
  const unresolved = [];
  if (!accountId || !apiToken) {
    report.skip_reason = "missing_cloudflare_credentials";
    unresolved.push("Cloudflare API inspection requires CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN");
    return { report, failures, unresolved };
  }
  report.checked = true;
  const scripts = await cloudflareGet(`/accounts/${encodeURIComponent(accountId)}/workers/scripts`, apiToken, fetchImpl);
  if (!scripts.ok) {
    failures.push(`cloudflare_workers_list:${scripts.status}`);
    report.error = scripts.error;
    return { report, failures, unresolved };
  }
  const scriptNames = parseCloudflareWorkerNames(scripts.result);
  for (const worker of report.workers) {
    worker.worker_exists = scriptNames.includes(worker.worker_name);
    if (!worker.worker_exists) failures.push(`cloudflare_worker_missing:${worker.worker_name}`);
    if (requiredWorkerSecrets.length === 0) continue;
    const secrets = await cloudflareGet(
      `/accounts/${encodeURIComponent(accountId)}/workers/scripts/${encodeURIComponent(worker.worker_name)}/secrets`,
      apiToken,
      fetchImpl,
    );
    worker.script_secrets.checked = true;
    if (!secrets.ok) {
      failures.push(`cloudflare_worker_secrets:${worker.worker_name}:${secrets.status}`);
      worker.script_secrets.error = secrets.error;
      continue;
    }
    const secretNames = parseCloudflareSecretNames(secrets.result);
    worker.script_secrets.present = requiredWorkerSecrets
      .filter((name) => secretNames.includes(name)).sort();
    worker.script_secrets.missing = requiredWorkerSecrets
      .filter((name) => !secretNames.includes(name)).sort();
    for (const name of worker.script_secrets.missing) {
      failures.push(`cloudflare_worker_secret_missing:${worker.worker_name}:${name}`);
    }
  }
  return { report, failures, unresolved };
}

function emptyWorkerReport(workerName, requiredWorkerSecrets) {
  return {
    worker_exists: null,
    worker_name: workerName,
    script_secrets: {
      checked: false,
      present: [],
      missing: [...requiredWorkerSecrets].sort(),
      required: [...requiredWorkerSecrets].sort(),
    },
  };
}

async function cloudflareGet(pathname, apiToken, fetchImpl) {
  try {
    const response = await fetchImpl(`${CLOUDFLARE_API_BASE}${pathname}`, {
      headers: {
        authorization: `Bearer ${apiToken}`,
        "content-type": "application/json",
      },
      method: "GET",
    });
    const body = await safeJson(response);
    if (!response.ok || body?.success === false) {
      return {
        ok: false,
        status: response.status,
        error: summarizeApiErrors(body),
      };
    }
    return { ok: true, status: response.status, result: body?.result ?? body };
  } catch {
    return { ok: false, status: "network_error", error: "request_failed" };
  }
}

function parseCloudflareWorkerNames(result) {
  const entries = Array.isArray(result) ? result : Array.isArray(result?.items) ? result.items : [];
  return entries
    .map((item) => String(item?.id || item?.name || "").trim())
    .filter(Boolean)
    .sort();
}

function parseCloudflareSecretNames(result) {
  const entries = Array.isArray(result) ? result : Array.isArray(result?.items) ? result.items : [];
  return entries
    .map((item) => String(item?.name || "").trim())
    .filter(Boolean)
    .sort();
}

async function inspectNeonApi({ apiKey, projectId, fetchImpl }) {
  const report = {
    checked: false,
    project_id_present: Boolean(projectId),
    project_exists: null,
  };
  const failures = [];
  const unresolved = [];
  if (!apiKey || !projectId) {
    report.skip_reason = "missing_neon_api_credentials";
    unresolved.push("Neon API inspection requires NEON_API_KEY and NEON_PROJECT_ID");
    return { report, failures, unresolved };
  }
  report.checked = true;
  try {
    const response = await fetchImpl(`${NEON_API_BASE}/projects/${encodeURIComponent(projectId)}`, {
      headers: {
        accept: "application/json",
        authorization: `Bearer ${apiKey}`,
      },
      method: "GET",
    });
    const body = await safeJson(response);
    if (!response.ok) {
      failures.push(`neon_project:${response.status}`);
      report.error = summarizeApiErrors(body);
      return { report, failures, unresolved };
    }
    report.project_exists = Boolean(body?.project?.id || body?.id || body?.project);
    if (!report.project_exists) {
      failures.push("neon_project_missing");
    }
  } catch {
    failures.push("neon_project:network_error");
    report.error = "request_failed";
  }
  return { report, failures, unresolved };
}

function skipped(reason) {
  return {
    failures: [],
    unresolved: [],
    report: {
      checked: false,
      skip_reason: reason,
    },
  };
}

async function safeJson(response) {
  const text = await response.text();
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch {
    return { parse_error: "invalid_json", text: text.slice(0, 200) };
  }
}

function summarizeApiErrors(body) {
  if (!body) return "empty_response";
  if (Array.isArray(body.errors) && body.errors.length > 0) {
    return body.errors
      .map((error) => error?.message || error?.code || JSON.stringify(error))
      .join("; ");
  }
  if (body.error) return String(body.error);
  if (body.message) return String(body.message);
  return "request_failed";
}

function stableJson(value) {
  return `${JSON.stringify(stableSort(value), null, 2)}\n`;
}

function stableSort(value) {
  if (Array.isArray(value)) {
    return value.map(stableSort);
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value)
        .sort(([left], [right]) => left.localeCompare(right))
        .map(([key, entry]) => [key, stableSort(entry)]),
    );
  }
  return value;
}

function relativize(targetPath) {
  const relative = path.relative(process.cwd(), targetPath);
  return relative && !relative.startsWith("..") ? relative : targetPath;
}

async function runCli() {
  const argv = process.argv.slice(2);
  const options = parseArgs(argv);
  if (options.help) {
    process.stdout.write(usage());
    return;
  }
  const report = await buildReadinessReport({ argv });
  process.stdout.write(stableJson(report));
  if (!report.ok) {
    process.exitCode = 1;
  }
}

const invokedPath = process.argv[1] ? pathToFileURL(path.resolve(process.argv[1])).href : "";
if (import.meta.url === invokedPath) {
  runCli().catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}

export {
  buildReadinessReport,
  parseArgs,
  parseCloudflareSecretNames,
  parseCloudflareWorkerNames,
  parseWranglerToml,
  stableJson,
};
