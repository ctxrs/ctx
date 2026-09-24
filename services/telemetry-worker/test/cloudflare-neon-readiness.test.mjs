import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, test } from "vitest";

import {
  buildReadinessReport,
  parseArgs,
  parseCloudflareSecretNames,
  parseCloudflareWorkerNames,
  parseWranglerToml,
  stableJson,
} from "../scripts/cloudflare-neon-readiness.mjs";

const PACKAGE_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

describe("cloudflare neon readiness script", () => {
  test("deployment contract keeps generic and staging deploys away from production", () => {
    const wranglerText = fs.readFileSync(path.join(PACKAGE_ROOT, "wrangler.toml"), "utf8");
    const wrangler = parseWranglerToml(wranglerText);
    const packageJson = JSON.parse(
      fs.readFileSync(path.join(PACKAGE_ROOT, "package.json"), "utf8"),
    );
    const rootSection = wranglerText.split(/^\[/mu, 1)[0];
    const stagingSection = tomlSection(wranglerText, "env.staging");
    const prodSection = tomlSection(wranglerText, "env.prod");
    const stagingObservability = tomlSection(wranglerText, "env.staging.observability");
    const stagingLogs = tomlSection(wranglerText, "env.staging.observability.logs");
    const prodObservability = tomlSection(wranglerText, "env.prod.observability");
    const prodLogs = tomlSection(wranglerText, "env.prod.observability.logs");

    expect(wrangler.name).toBe("ctx-telemetry-staging");
    expect(wrangler.vars.TELEMETRY_ANALYTICS_ENVIRONMENT).toBe("staging");
    expect(wrangler.vars.TELEMETRY_CLOUDFLARE_ACCOUNT_ID).toMatch(/^[0-9a-f]{32}$/u);
    expect(wrangler.triggers.crons).toEqual(["17 4 * * *"]);
    expect(wrangler.ratelimits).toContainEqual({
      name: "TELEMETRY_RATE_LIMITER",
      namespace_id: "2201",
      simple: "{ limit = 600, period = 60 }",
    });
    expect(wrangler.queues).toEqual({
      producers: [{
        binding: "TELEMETRY_INGEST_QUEUE",
        queue: "ctx-telemetry-ingest-staging",
      }],
      consumers: [{
        dead_letter_queue: "ctx-telemetry-ingest-staging-dlq",
        max_batch_size: "10",
        max_batch_timeout: "5",
        max_retries: "10",
        max_concurrency: "4",
        queue: "ctx-telemetry-ingest-staging",
      }],
    });
    expect(rootSection).not.toContain("routes");

    expect(wrangler.env.staging).toMatchObject({
      name: "ctx-telemetry-staging",
      vars: { TELEMETRY_ANALYTICS_ENVIRONMENT: "staging" },
      triggers: { crons: ["17 4 * * *"] },
    });
    expect(wrangler.env.staging.ratelimits).toContainEqual(
      expect.objectContaining({ name: "TELEMETRY_RATE_LIMITER", namespace_id: "2201" }),
    );
    expect(wrangler.env.staging.queues).toEqual(wrangler.queues);
    expect(stagingSection).toContain("workers_dev = false");
    expect(stagingSection).toContain("preview_urls = false");
    expect(stagingSection).toMatch(
      /^\s*\{ pattern = "telemetry-staging\.ctx\.rs", custom_domain = true \},$/mu,
    );
    expect(stagingSection.match(/custom_domain = true/gmu)).toHaveLength(1);
    expect(wranglerText.match(/^workers_dev = false$/gmu)).toHaveLength(1);
    expect(wranglerText.match(/^preview_urls = false$/gmu)).toHaveLength(1);
    expect(wranglerText).not.toContain("workers.dev");
    expect(stagingObservability).toContain("enabled = true");
    expect(stagingObservability).toContain("head_sampling_rate = 1");
    expect(stagingLogs).toContain("invocation_logs = false");

    expect(wrangler.env.prod).toMatchObject({
      name: "ctx-telemetry",
      vars: { TELEMETRY_ANALYTICS_ENVIRONMENT: "production" },
      triggers: { crons: ["17 3 * * *"] },
    });
    expect(wrangler.env.prod.ratelimits).toContainEqual(
      expect.objectContaining({ name: "TELEMETRY_RATE_LIMITER", namespace_id: "2202" }),
    );
    expect(wrangler.env.prod.queues).toEqual({
      producers: [{
        binding: "TELEMETRY_INGEST_QUEUE",
        queue: "ctx-telemetry-ingest-prod",
      }],
      consumers: [{
        dead_letter_queue: "ctx-telemetry-ingest-prod-dlq",
        max_batch_size: "10",
        max_batch_timeout: "5",
        max_retries: "10",
        max_concurrency: "4",
        queue: "ctx-telemetry-ingest-prod",
      }],
    });
    expect(wrangler.env.prod.queues.producers[0].queue)
      .not.toBe(wrangler.env.staging.queues.producers[0].queue);
    expect(wrangler.env.prod.queues.consumers[0].dead_letter_queue)
      .not.toBe(wrangler.env.staging.queues.consumers[0].dead_letter_queue);
    expect(prodSection).toContain("cli.ctx.rs/functions/v1/analytics*");
    expect(prodSection).toContain("cli.ctx.rs/functions/v1/install-attempt*");
    expect(prodSection).toContain("api.ctx.rs/functions/v1/telemetry*");
    expect(prodObservability).toContain("enabled = true");
    expect(prodObservability).toContain("head_sampling_rate = 1");
    expect(prodLogs).toContain("invocation_logs = false");

    expect(packageJson.scripts.deploy).toBe("wrangler deploy --env staging");
    expect(packageJson.scripts["deploy:staging"]).toBe("wrangler deploy --env staging");
    expect(packageJson.scripts["deploy:prod"]).toBe("wrangler deploy --env prod");
    expect(packageJson.scripts.deploy).not.toContain("--env prod");
  });

  test("rejects removed relay readiness options", () => {
    expect(() => parseArgs(["--profile", "relay"])).toThrow(/unsupported --profile/u);
    expect(() => parseArgs(["--ensure-neon-roles"])).toThrow(/unsupported argument/u);
  });

  test("telemetry profile never serializes secret values", async () => {
    const env = {
      CLOUDFLARE_ACCOUNT_ID: "account-id",
      CLOUDFLARE_API_TOKEN: "cf-secret-token",
      NEON_API_KEY: "neon-secret-token",
      NEON_PROJECT_ID: "neon-project",
      TELEMETRY_DATABASE_URL: "postgresql://ctx_telemetry_ingest:test-db@db.example.test/neondb?sslmode=require",
      TELEMETRY_RETENTION_DATABASE_URL: "postgresql://ctx_telemetry_retention:test-ret@db.example.test/neondb?sslmode=require",
      TELEMETRY_READ_DATABASE_URL: "postgresql://ctx_analytics_readonly:test-read@db.example.test/neondb?sslmode=require",
      TELEMETRY_IDENTITY_HMAC_KEY: "identity-hmac-secret",
      TELEMETRY_QUEUE_HEALTH_API_TOKEN: "queue-health-token",
    };

    const report = await buildReadinessReport({
      argv: ["--no-infisical", "--skip-cloudflare", "--skip-neon-api"],
      env,
      spawnSync: unavailableSpawn,
    });
    const serialized = stableJson(report);

    expect(report.env.missing).toEqual([]);
    expect(serialized).not.toContain("cf-secret-token");
    expect(serialized).not.toContain("neon-secret-token");
    expect(serialized).not.toContain("test-db");
    expect(serialized).not.toContain("test-read");
    expect(serialized).not.toContain("identity-hmac-secret");
    expect(report.mode.profile).toBe("telemetry");
    expect(report.env.database_roles).toEqual([
      {
        expected_role: "ctx_telemetry_ingest",
        matches: true,
        name: "TELEMETRY_DATABASE_URL",
      },
      {
        expected_role: "ctx_analytics_readonly",
        matches: true,
        name: "TELEMETRY_READ_DATABASE_URL",
      },
      {
        expected_role: "ctx_telemetry_retention",
        matches: true,
        name: "TELEMETRY_RETENTION_DATABASE_URL",
      },
    ]);
    expect(report.env.equality).toEqual([
      {
        equal: false,
        left: "TELEMETRY_DATABASE_URL",
        right: "TELEMETRY_RETENTION_DATABASE_URL",
      },
      {
        equal: false,
        left: "TELEMETRY_READ_DATABASE_URL",
        right: "TELEMETRY_RETENTION_DATABASE_URL",
      },
    ]);
  });

  test("telemetry profile requires Worker secrets and Neon telemetry storage without serializing values", async () => {
    const env = {
      CLOUDFLARE_ACCOUNT_ID: "account-id",
      CLOUDFLARE_API_TOKEN: "cf-secret-token",
      NEON_API_KEY: "neon-secret-token",
      NEON_PROJECT_ID: "neon-project",
      TELEMETRY_DATABASE_URL: "postgresql://ctx_telemetry_ingest:test-db@db.example.test/neondb?sslmode=require",
      TELEMETRY_RETENTION_DATABASE_URL: "postgresql://ctx_telemetry_retention:test-ret@db.example.test/neondb?sslmode=require",
      TELEMETRY_READ_DATABASE_URL: "postgresql://ctx_analytics_readonly:test-read@db.example.test/neondb?sslmode=require",
      TELEMETRY_IDENTITY_HMAC_KEY: "identity-hmac-secret",
      TELEMETRY_QUEUE_HEALTH_API_TOKEN: "queue-health-token",
    };

    const report = await buildReadinessReport({
      argv: [
        "--profile",
        "telemetry",
        "--wrangler-config",
        "wrangler.toml",
        "--worker-name",
        "ctx-telemetry",
        "--no-infisical",
        "--skip-cloudflare",
        "--skip-neon-api",
      ],
      env,
      spawnSync: unavailableSpawn,
    });
    const serialized = stableJson(report);

    expect(report.env.missing).toEqual([]);
    expect(report.mode.profile).toBe("telemetry");
    expect(report.neon.database).toMatchObject({
      checked: false,
      skip_reason: "disabled_by_profile",
    });
    expect(serialized).not.toContain("cf-secret-token");
    expect(serialized).not.toContain("neon-secret-token");
    expect(serialized).not.toContain("test-db");
    expect(serialized).not.toContain("test-read");
    expect(serialized).not.toContain("identity-hmac-secret");
    expect(report.failures).toEqual([]);
    expect(report.wrangler.environments).toContainEqual(
      expect.objectContaining({ name: "staging", required_crons: ["17 4 * * *"] }),
    );
    expect(report.wrangler.environments).toContainEqual(
      expect.objectContaining({
        missing_queue_consumers: [],
        missing_queue_producers: [],
        missing_vars: [],
        name: "prod",
        required_queue: {
          consumer: {
            dead_letter_queue: "ctx-telemetry-ingest-prod-dlq",
            max_batch_size: "10",
            max_batch_timeout: "5",
            max_retries: "10",
            max_concurrency: "4",
            queue: "ctx-telemetry-ingest-prod",
          },
          producer: {
            binding: "TELEMETRY_INGEST_QUEUE",
            queue: "ctx-telemetry-ingest-prod",
          },
        },
        required_crons: ["17 3 * * *"],
        required_vars: [
          "TELEMETRY_ANALYTICS_ENVIRONMENT",
          "TELEMETRY_CLOUDFLARE_ACCOUNT_ID",
          "TELEMETRY_IDENTITY_KEY_VERSION",
        ],
      }),
    );
    expect(report.wrangler.required_secrets).toEqual([
      "TELEMETRY_DATABASE_URL",
      "TELEMETRY_IDENTITY_HMAC_KEY",
      "TELEMETRY_QUEUE_HEALTH_API_TOKEN",
      "TELEMETRY_RETENTION_DATABASE_URL",
    ]);
  });

  test("telemetry readiness rejects missing or cross-wired environment queues", async () => {
    const original = fs.readFileSync(path.join(PACKAGE_ROOT, "wrangler.toml"), "utf8");
    const broken = original
      .replace(
        /\n\[\[env\.staging\.queues\.producers\]\]\nbinding = "TELEMETRY_INGEST_QUEUE"\nqueue = "ctx-telemetry-ingest-staging"\n/u,
        '\n[[env.staging.queues.producers]]\nbinding = "TELEMETRY_INGEST_QUEUE"\nqueue = "ctx-telemetry-ingest-prod"\n',
      )
      .replace(
        /\n\[\[env\.prod\.queues\.consumers\]\][\s\S]*?dead_letter_queue = "ctx-telemetry-ingest-prod-dlq"\n/u,
        "\n",
      );
    const temporaryRoot = fs.mkdtempSync(path.join(os.tmpdir(), "telemetry-readiness-"));
    const configPath = path.join(temporaryRoot, "wrangler.toml");
    fs.writeFileSync(configPath, broken);
    try {
      const report = await buildReadinessReport({
        argv: [
          "--wrangler-config", configPath,
          "--no-infisical", "--skip-cloudflare", "--skip-neon-api",
        ],
        env: readinessEnv(),
        spawnSync: unavailableSpawn,
      });

      expect(report.failures).toContain(
        "wrangler_missing_queue_producers:staging:TELEMETRY_INGEST_QUEUE=ctx-telemetry-ingest-staging",
      );
      expect(report.failures).toContain(
        "wrangler_missing_queue_consumers:prod:ctx-telemetry-ingest-prod",
      );
    } finally {
      fs.rmSync(temporaryRoot, { force: true, recursive: true });
    }
  });

  test("telemetry profile rejects reuse of the ingestion credential for retention", async () => {
    const sharedDatabaseUrl = "postgresql://must-not-serialize@db.example.test/neondb";
    const report = await buildReadinessReport({
      argv: ["--no-infisical", "--skip-cloudflare", "--skip-neon-api"],
      env: {
        CLOUDFLARE_ACCOUNT_ID: "account-id",
        CLOUDFLARE_API_TOKEN: "cf-token",
        NEON_API_KEY: "neon-token",
        NEON_PROJECT_ID: "neon-project",
        TELEMETRY_DATABASE_URL: sharedDatabaseUrl,
        TELEMETRY_RETENTION_DATABASE_URL: sharedDatabaseUrl,
        TELEMETRY_READ_DATABASE_URL: "postgresql://readonly@db.example.test/neondb",
        TELEMETRY_IDENTITY_HMAC_KEY: "identity-key",
        TELEMETRY_QUEUE_HEALTH_API_TOKEN: "queue-health-token",
      },
      spawnSync: unavailableSpawn,
    });

    expect(report.failures).toContain(
      "env_values_must_differ:TELEMETRY_DATABASE_URL:TELEMETRY_RETENTION_DATABASE_URL",
    );
    expect(report.env.equality).toContainEqual(expect.objectContaining({ equal: true }));
    expect(stableJson(report)).not.toContain(sharedDatabaseUrl);
  });

  test("telemetry profile rejects a distinct URL that reuses the ingestion role", async () => {
    const report = await buildReadinessReport({
      argv: ["--no-infisical", "--skip-cloudflare", "--skip-neon-api"],
      env: {
        CLOUDFLARE_ACCOUNT_ID: "account-id",
        CLOUDFLARE_API_TOKEN: "cf-token",
        NEON_API_KEY: "neon-token",
        NEON_PROJECT_ID: "neon-project",
        TELEMETRY_DATABASE_URL: "postgresql://ctx_telemetry_ingest:one@db.example.test/neondb",
        TELEMETRY_RETENTION_DATABASE_URL: "postgresql://ctx_telemetry_ingest:two@db.example.test/neondb?application_name=retention",
        TELEMETRY_READ_DATABASE_URL: "postgresql://ctx_analytics_readonly:three@db.example.test/neondb",
        TELEMETRY_IDENTITY_HMAC_KEY: "identity-key",
        TELEMETRY_QUEUE_HEALTH_API_TOKEN: "queue-health-token",
      },
      spawnSync: unavailableSpawn,
    });

    expect(report.failures).toContain(
      "database_url_role_mismatch:TELEMETRY_RETENTION_DATABASE_URL:ctx_telemetry_retention",
    );
    expect(report.env.equality.every((entry) => entry.equal === false)).toBe(true);
    expect(stableJson(report)).not.toContain("application_name=retention");
  });

  test("wrangler parser reports env-specific vars instead of inheriting root vars", () => {
    const wrangler = parseWranglerToml(`
name = "ctx-telemetry"

[vars]
ENVIRONMENT = "dev"
TELEMETRY_ANALYTICS_ENVIRONMENT = "staging"

[triggers]
crons = ["1 2 * * *"]

[[ratelimits]]
name = "ROOT_LIMITER"
namespace_id = "1"

[env.staging]
name = "ctx-telemetry-staging"

[env.staging.vars]
ENVIRONMENT = "staging"
TELEMETRY_ANALYTICS_ENVIRONMENT = "staging"

[env.staging.triggers]
crons = ["17 3 * * *"]

[[env.staging.ratelimits]]
name = "TELEMETRY_RATE_LIMITER"
namespace_id = "2"
`);

    expect(wrangler.name).toBe("ctx-telemetry");
    expect(Object.keys(wrangler.vars).sort()).toEqual([
      "ENVIRONMENT",
      "TELEMETRY_ANALYTICS_ENVIRONMENT",
    ]);
    expect(Object.keys(wrangler.env.staging.vars).sort()).toEqual([
      "ENVIRONMENT",
      "TELEMETRY_ANALYTICS_ENVIRONMENT",
    ]);
    expect(wrangler.triggers.crons).toEqual(["1 2 * * *"]);
    expect(wrangler.env.staging.triggers.crons).toEqual(["17 3 * * *"]);
    expect(wrangler.ratelimits).toContainEqual(expect.objectContaining({ name: "ROOT_LIMITER" }));
    expect(wrangler.env.staging.ratelimits).toContainEqual(
      expect.objectContaining({ name: "TELEMETRY_RATE_LIMITER" }),
    );
    expect(wrangler.env.staging.name).toBe("ctx-telemetry-staging");
  });

  test("Cloudflare readiness inspects secrets for both selected Worker environments", async () => {
    const requested = [];
    const response = (result) => new Response(JSON.stringify({ result, success: true }), {
      headers: { "content-type": "application/json" },
      status: 200,
    });
    const fetchImpl = async (url) => {
      requested.push(String(url));
      if (String(url).endsWith("/workers/scripts")) {
        return response([{ id: "ctx-telemetry" }, { id: "ctx-telemetry-staging" }]);
      }
      return response([
        { name: "TELEMETRY_DATABASE_URL" },
        { name: "TELEMETRY_IDENTITY_HMAC_KEY" },
        { name: "TELEMETRY_QUEUE_HEALTH_API_TOKEN" },
        { name: "TELEMETRY_RETENTION_DATABASE_URL" },
      ]);
    };
    const report = await buildReadinessReport({
      argv: ["--no-infisical", "--skip-neon-api"],
      env: {
        CLOUDFLARE_ACCOUNT_ID: "account-id",
        CLOUDFLARE_API_TOKEN: "cf-token",
        NEON_API_KEY: "neon-token",
        NEON_PROJECT_ID: "neon-project",
        TELEMETRY_DATABASE_URL: "postgresql://ctx_telemetry_ingest:one@db.example.test/neondb",
        TELEMETRY_RETENTION_DATABASE_URL: "postgresql://ctx_telemetry_retention:two@db.example.test/neondb",
        TELEMETRY_READ_DATABASE_URL: "postgresql://ctx_analytics_readonly:three@db.example.test/neondb",
        TELEMETRY_IDENTITY_HMAC_KEY: "identity-key",
        TELEMETRY_QUEUE_HEALTH_API_TOKEN: "queue-health-token",
      },
      fetchImpl,
      spawnSync: unavailableSpawn,
    });

    expect(report.failures).toEqual([]);
    expect(report.cloudflare.workers.map((worker) => worker.worker_name).sort()).toEqual([
      "ctx-telemetry",
      "ctx-telemetry-staging",
    ]);
    expect(requested.filter((url) => url.endsWith("/secrets"))).toHaveLength(2);
  });

  test("parses Cloudflare list responses without secret text", () => {
    expect(parseCloudflareWorkerNames([{ id: "ctx-telemetry" }, { name: "other" }])).toEqual([
      "ctx-telemetry",
      "other",
    ]);
    expect(
      parseCloudflareSecretNames({
        items: [
          { name: "TELEMETRY_IDENTITY_HMAC_KEY", text: "must-not-serialize" },
          { name: "TELEMETRY_QUEUE_HEALTH_API_TOKEN", text: "must-not-serialize-queue-token" },
          { name: "TELEMETRY_DATABASE_URL", text: "must-not-serialize-either" },
          { name: "TELEMETRY_RETENTION_DATABASE_URL", text: "must-not-serialize-retention" },
        ],
      }),
    ).toEqual([
      "TELEMETRY_DATABASE_URL",
      "TELEMETRY_IDENTITY_HMAC_KEY",
      "TELEMETRY_QUEUE_HEALTH_API_TOKEN",
      "TELEMETRY_RETENTION_DATABASE_URL",
    ]);
  });
});

function unavailableSpawn() {
  return {
    error: new Error("unavailable"),
    status: 1,
    stderr: "",
    stdout: "",
  };
}

function readinessEnv() {
  return {
    CLOUDFLARE_ACCOUNT_ID: "account-id",
    CLOUDFLARE_API_TOKEN: "cf-token",
    NEON_API_KEY: "neon-token",
    NEON_PROJECT_ID: "neon-project",
    TELEMETRY_DATABASE_URL: "postgresql://ctx_telemetry_ingest:one@db.example.test/neondb",
    TELEMETRY_RETENTION_DATABASE_URL: "postgresql://ctx_telemetry_retention:two@db.example.test/neondb",
    TELEMETRY_READ_DATABASE_URL: "postgresql://ctx_analytics_readonly:three@db.example.test/neondb",
    TELEMETRY_IDENTITY_HMAC_KEY: "identity-key",
    TELEMETRY_QUEUE_HEALTH_API_TOKEN: "queue-health-token",
  };
}

function tomlSection(text, name) {
  const marker = `[${name}]`;
  const start = text.indexOf(marker);
  if (start < 0) return "";
  const bodyStart = start + marker.length;
  const next = text.slice(bodyStart).search(/^\s*\[/mu);
  return next < 0 ? text.slice(bodyStart) : text.slice(bodyStart, bodyStart + next);
}
