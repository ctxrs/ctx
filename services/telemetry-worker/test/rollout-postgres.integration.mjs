import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { BASE_SCHEMA, scalar, sql, sqlFileAs, startPostgres } from "./support/postgres.mjs";
import { CANONICAL_TELEMETRY_SCHEMA } from "./support/canonical-telemetry.mjs";

const migration = "0053_rollout_telemetry_window.sql";
const signature = "ctx.analytics_rollout_window(text,timestamptz,timestamptz,text[])";
const from = "2026-09-30T00:00:00Z";
const until = "2026-09-30T12:00:00Z";
const quote = (value) => `'${value.replaceAll("'", "''")}'`;
const call = (versions = "null", start = quote(from), end = quote(until)) =>
  `ctx.analytics_rollout_window('production', ${start}, ${end}, ${versions})`;

function insert(database, id, changes = {}) {
  const event = {
    event_id: id, occurred_at: from, received_at: until,
    event_name: "operation_completed", event_version: 1, schema_version: 1,
    plane: "product", analytics_environment: "production", traffic_class: "unclassified_public",
    activity_class: "product_activity", app_version: "2.1.3", os: "linux", arch: "x86_64",
    surface: "cli", origin_runtime: "cli", broker_runtime: "cli",
    client_profile_id_hash: "a".repeat(64), data_root_id_hash: "b".repeat(64), identity_key_version: 1,
    properties: { operation: "search", outcome: "success" }, ...changes,
  };
  sql(database, `insert into ctx.telemetry_event select * from jsonb_populate_record(
    null::ctx.telemetry_event, ${quote(JSON.stringify(event))}::jsonb)`);
}

function read(database, expression = call(), settings = "") {
  return JSON.parse(scalar(database, `begin read only; set local role ctx_analytics_readonly;
    ${settings}
    select coalesce(jsonb_agg(result), '[]'::jsonb) from ${expression} as result; commit;`));
}

function override(database, key, selectors, trafficClass = "internal") {
  const row = { override_key: key, traffic_class: trafficClass, created_at: "2026-01-01Z", ...selectors };
  sql(database, `insert into ctx.telemetry_traffic_class_overrides select * from
    jsonb_populate_record(null::ctx.telemetry_traffic_class_overrides, ${quote(JSON.stringify(row))}::jsonb)`);
}

test("rollout window preserves canonical classification, bounded plans and reader aggregates", async (t) => {
  const database = startPostgres();
  const reset = () => sql(database, "truncate ctx.telemetry_event, ctx.telemetry_traffic_class_overrides");
  try {
    sql(database, BASE_SCHEMA);
    assert.throws(() => sqlFileAs(database, migration, "ctx_migration"), /canonical telemetry view/u);
    sql(database, CANONICAL_TELEMETRY_SCHEMA);
    assert.throws(() => sqlFileAs(database, migration, "neondb_owner"), /requires ctx_migration/u);
    sqlFileAs(database, migration, "ctx_migration");
    sqlFileAs(database, migration, "ctx_migration");

    await t.test("half-open occurrence bounds, nullable old fields and unconstrained version cohorts", () => {
      insert(database, "start");
      insert(database, "end", { occurred_at: until });
      insert(database, "late-old", { occurred_at: "2026-09-29T23:59:00Z" });
      insert(database, "different-version", { app_version: "2.2.1" });
      assert.deepEqual(read(database).filter((r) => r.aggregation_level === "version")
        .map((r) => [r.app_version, r.event_count]).sort(), [["2.1.3", 1], ["2.2.1", 1]]);
      assert.equal(read(database, call("array['2.2.1']"))[0].event_count, 1);
      assert.deepEqual(read(database, call("array[]::text[]")), []);
      assert.deepEqual(read(database, call("array['missing']")), []);
      assert.equal(read(database, call(`array[${Array.from({ length: 32 }, (_, i) => quote(`2.2.${i}`)).join(",")}]`))
        .filter((r) => r.aggregation_level === "version").length, 1);
      assert.equal(read(database, call(), "set local timezone = 'Pacific/Kiritimati';")
        .filter((r) => r.aggregation_level === "version").length, 2);
      assert.ok(read(database, call("null", quote(from), quote("2026-10-01T00:00:00Z"))).length > 0);
      for (const expression of [
        call("null", "null"), call("null", quote(from), "null"),
        call("null", "'-infinity'"), call("null", quote(from), "'infinity'"),
        call("null", quote(until), quote(from)), call("null", quote(from), quote(from)),
        call("null", quote(from), quote("2026-10-01T00:00:01Z")),
        call("array[null]::text[]"), call("array['']"), call("array['   ']"),
        call().replace("'production'", "null"), call().replace("'production'", "'other'"),
      ]) assert.throws(() => read(database, expression), /expected production\/staging|version filters/u);
      insert(database, "late-commit", { occurred_at: "2026-09-30T01:00:00Z", received_at: "2026-10-01Z" });
      assert.equal(read(database).find((r) => r.aggregation_level === "version" && r.app_version === "2.1.3").event_count, 2);
    });

    await t.test("effective overrides, expiry, precedence, key version and canonical exclusions", () => {
      reset();
      insert(database, "ordinary");
      insert(database, "root-excluded", { client_profile_id_hash: "c", data_root_id_hash: "root-hidden" });
      override(database, "root", { data_root_id_hash: "root-hidden", identity_key_version: 1 });
      insert(database, "event-wins", { client_profile_id_hash: "profile-hidden" });
      override(database, "profile", { client_profile_id_hash: "profile-hidden", identity_key_version: 1 });
      override(database, "event", { event_id: "event-wins" }, "unclassified_public");
      insert(database, "profile-wins", { client_profile_id_hash: "profile-visible", data_root_id_hash: "root-hidden" });
      override(database, "profile-allow", { client_profile_id_hash: "profile-visible", identity_key_version: 1 }, "unclassified_public");
      insert(database, "key-mismatch", { data_root_id_hash: "root-hidden", identity_key_version: 2 });
      insert(database, "expired");
      override(database, "expired", { event_id: "expired", expires_at: "2000-01-01Z" });
      insert(database, "tie-order");
      override(database, "tie-a", { event_id: "tie-order" }, "unclassified_public");
      override(database, "tie-z", { event_id: "tie-order" });
      insert(database, "newer-wins");
      override(database, "newer-old", { event_id: "newer-wins" });
      override(database, "newer-new", { event_id: "newer-wins", created_at: "2026-02-01Z" }, "unclassified_public");
      insert(database, "version-mismatch");
      override(database, "wrong-version", { event_id: "version-mismatch", app_version: "2.0.0" });
      insert(database, "version-excluded");
      override(database, "matching-version", { event_id: "version-excluded", app_version: "2.1.3" });
      for (const [id, changes] of [
        ["internal", { traffic_class: "internal" }], ["synthetic", { traffic_class: "synthetic" }],
        ["staging", { analytics_environment: "staging", traffic_class: "synthetic" }],
        ["wrong-plane", { plane: "control" }], ["fake", { provider_id: "fake" }],
        ["smoke", { properties: { operation: "search", smoke_run_id: "fixture" } }],
        ["zero-version", { app_version: "0.0.0-test" }], ["missing-version", { app_version: null }],
        ["delivery", { event_name: "analytics_delivery_observation" }], ["future", { event_version: 2 }],
        ["legacy-wrong-runtime", { schema_version: null, event_name: "cli_invocation", origin_runtime: "daemon" }],
      ]) insert(database, id, changes);
      insert(database, "legacy-user", { schema_version: null, event_name: "cli_invocation", traffic_class: "user",
        origin_install_id_hash: "legacy", properties: { action: "upgrade" }, success: true });
      insert(database, "legacy-public", { schema_version: null, event_name: "cli_invocation",
        origin_install_id_hash: "legacy", properties: { action: "upgrade" }, success: true });
      const expected = ["event-wins", "expired", "key-mismatch", "legacy-public", "legacy-user",
        "newer-wins", "ordinary", "profile-wins", "tie-order", "version-mismatch"];
      assert.deepEqual(JSON.parse(scalar(database, `select jsonb_agg(event_id order by event_id)
        from ctx.analytics_canonical_telemetry_events where analytics_environment = 'production'
        and is_eligible_product_telemetry`)), expected);
      assert.equal(read(database).filter((r) => r.aggregation_level === "version")
        .reduce((total, r) => total + r.event_count, 0), expected.length);
      assert.deepEqual(read(database, call().replace("'production'", "'staging'")), []);
    });

    await t.test("applied differs from successful checks, populations and refresh reason coverage", () => {
      reset();
      for (const [id, version, status, applied, mode, operation] of [
        ["applied", "2.1.3", "applied", true, "manual", "apply"],
        ["uptodate", "2.2.1", "up_to_date", false, "auto", "apply"],
        ["check", "2.2.1", "up_to_date", false, "manual", "check"],
        ["failed", "2.1.3", "failed", false, "auto", "apply"],
        ["scheduled", "2.1.3", "scheduled", false, "manual", "apply"],
        ["dry", "2.1.3", "dry_run", false, "manual", "apply"],
        ["inconsistent-history", "2.1.3", "applied", false, "manual", "apply"],
      ]) insert(database, id, { app_version: version, properties: { operation: "upgrade",
        outcome: status === "failed" ? "failure" : "success", upgrade_status: status,
        upgrade_applied: applied, upgrade_mode: mode, upgrade_operation: operation,
        upgrade_channel: "stable", ...(status === "failed" ? { upgrade_failure_kind: "artifact_download" } : {}) } });
      insert(database, "legacy-no-status", { schema_version: null, event_name: "cli_invocation",
        properties: { action: "upgrade" }, success: true, origin_install_id_hash: "legacy" });
      const failure = { operation: "refresh", outcome: "failure", refresh_result: "failure",
        failure_code: "source_refresh_failed", retryable: false };
      insert(database, "old-failure", { event_name: "provider_refresh_completed", surface: "daemon", properties: failure });
      insert(database, "new-failure", { event_name: "provider_refresh_completed", surface: "daemon",
        data_root_id_hash: "other-root", properties: { ...failure, refresh_failure_stage: "execution",
          refresh_failure_kind: "io", refresh_failure_reason: "io_permission_denied" } });
      insert(database, "partial", { event_name: "provider_refresh_completed", surface: "daemon",
        properties: { operation: "refresh", outcome: "success", refresh_result: "partial",
          failure_scope: "source", failure_type: "malformed_source", refresh_source_failure_class: "incompatible" } });
      insert(database, "no-identity", { client_profile_id_hash: null, data_root_id_hash: null, identity_key_version: null });
      insert(database, "runtime", { event_name: "runtime_observation", surface: "daemon",
        properties: { operation: "failed", outcome: "failure" } });
      const result = read(database);
      const detail = result.filter((r) => r.aggregation_level === "event");
      assert.equal(detail.filter((r) => r.upgrade_status === "applied" && r.upgrade_applied === true)
        .reduce((n, r) => n + r.event_count, 0), 1);
      assert.equal(detail.filter((r) => r.upgrade_status === "up_to_date" && r.upgrade_applied === false)
        .reduce((n, r) => n + r.event_count, 0), 2);
      assert.equal(detail.find((r) => r.event_name === "cli_invocation").upgrade_status, null);
      assert.equal(detail.find((r) => r.upgrade_status === "failed").upgrade_failure_kind, "artifact_download");
      const failures = detail.filter((r) => r.event_name === "provider_refresh_completed" && r.outcome === "failure");
      assert.equal(failures.reduce((n, r) => n + r.event_count, 0), 2);
      assert.equal(failures.filter((r) => r.refresh_failure_reason !== null).length, 1);
      assert.equal(detail.find((r) => r.refresh_source_failure_class === "incompatible").outcome, "success");
      const versions = result.filter((r) => r.aggregation_level === "version");
      assert.deepEqual(versions.map((r) => [r.schema_version, r.app_version, r.event_count,
        r.client_profile_count, r.data_root_count, r.legacy_origin_install_count]).sort(),
      [[1, "2.1.3", 10, 1, 2, 0], [1, "2.2.1", 2, 1, 1, 0], [null, "2.1.3", 1, 1, 1, 1]].sort());
      assert.ok(detail.reduce((n, r) => n + r.client_profile_count, 0) > 1);
      for (const row of versions) assert.equal(row.event_name, null);
    });

    await t.test("reader ACL, fixed outputs and safe search path", () => {
      sql(database, "create role rollout_unprivileged; grant usage on schema ctx to rollout_unprivileged");
      for (const role of ["rollout_unprivileged", "ctx_telemetry_ingest", "ctx_telemetry_retention", "ctx_product_health_readonly", "ctx_control_plane"]) {
        assert.equal(scalar(database, `select has_function_privilege('${role}', '${signature}', 'execute')`), "f");
      }
      for (const relation of ["telemetry_event", "analytics_canonical_telemetry_events", "telemetry_traffic_class_overrides"]) {
        assert.equal(scalar(database, `select has_table_privilege('ctx_analytics_readonly', 'ctx.${relation}', 'select')`), "f");
        assert.throws(() => scalar(database, `set role ctx_analytics_readonly; select * from ctx.${relation}`), /permission denied/u);
      }
      assert.equal(scalar(database, `select pg_get_userbyid(proowner) || '|' || prosecdef || '|' || provolatile::text
        from pg_proc where oid = '${signature}'::regprocedure`), "ctx_migration|true|s");
      assert.match(scalar(database, `select proconfig::text from pg_proc where oid = '${signature}'::regprocedure`),
        /search_path=pg_catalog, ctx, pg_temp/u);
      const columns = Object.keys(read(database)[0]);
      assert.equal(columns.length, 30);
      assert.ok(!columns.some((c) => /hash|event_id|identity|properties|attempt_id|override/u.test(c)));
      // A hostile session search path cannot replace the qualified canonical authority.
      sql(database, "create schema shadow; create table shadow.analytics_canonical_telemetry_events (event_id text)");
      assert.equal(read(database, call(), "set local search_path = shadow, pg_temp;").length,
        read(database).length);
    });

    await t.test("the actual inner aggregate uses time-index bounds before classification", () => {
      reset();
      sql(database, `insert into ctx.telemetry_event
        (event_id, occurred_at, event_name, event_version, schema_version, plane,
         analytics_environment, traffic_class, app_version, properties)
        select 'old-' || n, timestamptz '2020-01-01Z' + n * interval '1 minute',
          'operation_completed', 1, 1, 'product', 'production', 'unclassified_public',
          '2.1.3', '{"operation":"search","outcome":"success"}'::jsonb
        from generate_series(1, 20000) as n`);
      insert(database, "current");
      sql(database, "analyze ctx.telemetry_event; analyze ctx.telemetry_traffic_class_overrides");
      const source = readFileSync(new URL(`../schema/${migration}`, import.meta.url), "utf8");
      const inner = source.split("  return query\n")[1].split(";\nend\n$function$")[0];
      assert.ok(inner?.startsWith("  with bounded"));
      const plans = {};
      for (const [name, versions] of [["all", "null::text[]"], ["selected", "array['2.1.3']"], ["generic", "null::text[]"]]) {
        const statement = inner.replaceAll("p_environment", "'production'::text")
          .replaceAll("p_from", `${quote(from)}::timestamptz`)
          .replaceAll("p_until", `${quote(until)}::timestamptz`).replaceAll("p_versions", versions);
        const prepared = inner.replaceAll("p_environment", "$1").replaceAll("p_from", "$2")
          .replaceAll("p_until", "$3").replaceAll("p_versions", "$4");
        const explain = name === "generic"
          ? `set plan_cache_mode = force_generic_plan;
             prepare inner_aggregate(text,timestamptz,timestamptz,text[]) as ${prepared};
             explain (format json) execute inner_aggregate('production','${from}','${until}',null)`
          : `explain (format json) ${statement}`;
        const plan = JSON.parse(scalar(database, `set timezone = 'UTC'; set role ctx_migration; ${explain}`))[0].Plan;
        const nodes = [];
        function visit(node, ancestors = []) {
          nodes.push({ node, ancestors });
          for (const child of node.Plans ?? []) visit(child, [...ancestors, node]);
        }
        visit(plan);
        const boundedScan = nodes.find(({ node }) => /Index/u.test(node["Node Type"])
          && node["Index Name"] === "telemetry_event_env_ts_idx"
          && /occurred_at >=/u.test(node["Index Cond"] ?? "") && /occurred_at </u.test(node["Index Cond"] ?? ""));
        assert.ok(boundedScan, JSON.stringify(plan));
        assert.ok(boundedScan.ancestors.some((node) => node["Node Type"] === "Nested Loop"));
        assert.ok(!nodes.some(({ node }) => node["Node Type"] === "Seq Scan" && node["Relation Name"] === "telemetry_event"));
        plans[name] = plan;
      }
      if (process.env.TEST_UNDECLARED_OUTPUTS_DIR) writeFileSync(
        path.join(process.env.TEST_UNDECLARED_OUTPUTS_DIR, "rollout-plans.json"), JSON.stringify(plans, null, 2));
      const counts = scalar(database, `begin read only; set local role ctx_analytics_readonly;
        prepare bounded(text[]) as select event_count from ctx.analytics_rollout_window(
          'production', '${from}', '${until}', $1) where aggregation_level = 'version';
        ${Array.from({ length: 8 }, () => "execute bounded(null);").join("\n")}
        execute bounded(array['missing']); commit;`);
      assert.deepEqual(counts.split("\n"), Array(8).fill("1"));
    });
  } finally {
    database.cleanup();
  }
});
