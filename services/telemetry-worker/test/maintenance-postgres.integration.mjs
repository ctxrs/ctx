import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import ts from "typescript";
import {
  BASE_SCHEMA, postgresBin, rows, scalar, sql, sqlFileAs, startPostgres,
} from "./support/postgres.mjs";

// Execute the actual adapter's SQL against the existing PostgreSQL fixture.
// database.ts has only type imports; its optional Neon import is never invoked.
const compiled = ts.transpileModule(readFileSync(new URL("../src/database.ts", import.meta.url), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
}).outputText;
const { NeonTelemetryMaintenanceDatabase } = await import(
  `data:text/javascript;base64,${Buffer.from(compiled).toString("base64")}`
);

test("telemetry maintenance catch-up repairs delayed persistence and skipped days exactly once", async (t) => {
  assert.ok(postgresBin, "PostgreSQL 16+ is required; this behavioral test must not skip");
  const database = startPostgres();
  try {
    sql(database, BASE_SCHEMA);
    assert.throws(() => sqlFileAs(database, "0050_installer_history_date_indexes.sql", "ctx_migration"),
      /require ctx_migration and the daily materializer/u);
    sqlFileAs(database, "0044_privacy_safe_telemetry_history.sql", "ctx_migration");
    sql(database, `
      create index telemetry_event_received_at_idx on ctx.telemetry_event (received_at);
      create index blame_product_event_received_at_idx on ctx.blame_product_event (received_at);
    `);
    sqlFileAs(database, "0046_telemetry_history_received_at_ranges.sql", "ctx_migration");
    sqlFileAs(database, "0051_telemetry_history_serialization.sql", "ctx_migration");
    assert.throws(() => sqlFileAs(database, "0052_telemetry_history_sort_memory.sql", "neondb_owner"),
      /requires ctx_migration and the reviewed materializer/u);
    sqlFileAs(database, "0052_telemetry_history_sort_memory.sql", "ctx_migration");
    sqlFileAs(database, "0052_telemetry_history_sort_memory.sql", "ctx_migration");
    assert.match(scalar(database, `select array_to_string(proconfig, ',') from pg_proc
      where oid = 'ctx.materialize_product_telemetry_history(date)'::regprocedure`), /work_mem=64MB/u);
    assert.throws(() => sqlFileAs(database, "0050_installer_history_date_indexes.sql", "neondb_owner"),
      /require ctx_migration and the daily materializer/u);
    sqlFileAs(database, "0050_installer_history_date_indexes.sql", "ctx_migration");
    const validIndexes = indexState(database);
    sqlFileAs(database, "0050_installer_history_date_indexes.sql", "ctx_migration");
    assert.deepEqual(indexState(database), validIndexes, "repeat preserves both valid index objects");
    verifyInvalidIndexRepair(database, validIndexes, t);
    const currentDay = scalar(database, "select (statement_timestamp() at time zone 'utc')::date");
    let adapterStatement;
    const maintenance = new NeonTelemetryMaintenanceDatabase({
      async query(statement) {
        adapterStatement = statement;
        return [JSON.parse(scalar(database, `
          set timezone = 'Pacific/Kiritimati';
          set role ctx_telemetry_retention;
          select row_to_json(result) from (${statement}) as result
        `))];
      },
    });
    const emptyReceipt = await maintenance.materializeProductTelemetryHistory();
    assert.equal(scalar(database, "select count(*) from ctx.telemetry_diagnostic_daily"), "0");
    // D was processed while the queued row was still absent from Neon.
    sql(database, `select ctx.materialize_product_telemetry_history(date '${currentDay}' - 2)`);
    assert.equal(scalar(database, "select count(*) from ctx.telemetry_diagnostic_daily"), "0");
    // Persist D later; other rows represent a missed Cron and the window edges.
    sql(database, `
      insert into ctx.telemetry_event (
        event_id, occurred_at, received_at, event_name, schema_version, plane,
        analytics_environment, traffic_class, properties
      ) select 'late-' || age, statement_timestamp(),
        (date '${currentDay}' - age)::timestamp at time zone 'utc',
        'operation_completed', 1, 'product', 'staging', 'synthetic', '{"operation":"search"}'::jsonb
      from unnest(array[0, 1, 2, 4, 8, 9, 10]) as ages(age);
    `);
    let queries = 0;
    const countedMaintenance = new NeonTelemetryMaintenanceDatabase({
      async query(statement) {
        queries += 1;
        const result = scalar(database, `
          set timezone = 'Pacific/Kiritimati';
          set role ctx_telemetry_retention;
          select row_to_json(result) from (${statement}) as result
        `);
        return [JSON.parse(result)];
      },
    });
    await countedMaintenance.materializeProductTelemetryHistory();
    const expected = ["1|1", "2|1", "4|1", "8|1", "9|1"];
    const dailyCounts = () => rows(database, `
      select (date '${currentDay}' - received_date)::text || '|' || sum(event_count)::text
      from ctx.telemetry_diagnostic_daily group by received_date order by received_date desc
    `);
    assert.deepEqual(dailyCounts(), expected);
    await countedMaintenance.materializeProductTelemetryHistory();
    assert.deepEqual(dailyCounts(), expected);
    assert.equal(queries, 20, "one UTC anchor and nine bounded daily requests per schedule");
    assert.equal(scalar(database, "select count(*) from ctx.telemetry_event"), "7");

    // A failed day keeps its prior aggregate; earlier completed days commit.
    sql(database, `
      insert into ctx.telemetry_event (
        event_id, occurred_at, received_at, event_name, schema_version, plane,
        analytics_environment, traffic_class, properties
      ) values ('later-2', statement_timestamp(),
        (date '${currentDay}' - 2)::timestamp at time zone 'utc',
        'operation_completed', 1, 'product', 'staging', 'synthetic', '{"operation":"search"}');
      insert into ctx.telemetry_event (
        event_id, occurred_at, received_at, event_name, schema_version, plane,
        analytics_environment, traffic_class, properties
      ) values ('later-9', statement_timestamp(),
        (date '${currentDay}' - 9)::timestamp at time zone 'utc',
        'operation_completed', 1, 'product', 'staging', 'synthetic', '{"operation":"search"}');
      create function ctx.fail_window_for_test() returns trigger language plpgsql as $$
      begin
        if new.received_date = date '${currentDay}' - 8 then
          raise exception 'injected_materialization_failure';
        end if;
        return new;
      end $$;
      create trigger fail_window_for_test before insert on ctx.telemetry_diagnostic_daily
        for each row execute function ctx.fail_window_for_test();
    `);
    await assert.rejects(countedMaintenance.materializeProductTelemetryHistory(), /injected_materialization_failure/u);
    assert.deepEqual(dailyCounts(), [...expected.slice(0, -1), "9|2"],
      "completed older days remain committed");
    sql(database, "drop trigger fail_window_for_test on ctx.telemetry_diagnostic_daily");
    await countedMaintenance.materializeProductTelemetryHistory();
    expected[1] = "2|2";
    expected[4] = "9|2";
    assert.deepEqual(dailyCounts(), expected);

    // The existing routine remains the explicit repair for older receipt dates.
    sql(database, `select ctx.materialize_product_telemetry_history(date '${currentDay}' - 10)`);
    assert.deepEqual(dailyCounts(), [...expected, "10|1"]);
    assert.equal(scalar(database, `
      select count(*) from information_schema.columns
      where table_schema = 'ctx' and table_name = 'telemetry_diagnostic_daily'
      and (column_name like '%_hash' or column_name in ('event_id', 'identity_key_version', 'received_at'))
    `), "0");

    // Check the empty receipt after catch-up assertions so the base negative
    // control first exposes missing delayed days, rather than the new receipt shape.
    assert.deepEqual(emptyReceipt, {
      materialized_count: "0",
      first_received_date: scalar(database, `select (date '${currentDay}' - 9)::text`),
      last_received_date: scalar(database, `select (date '${currentDay}' - 1)::text`),
    });

    // Load all three raw families before executing the real combined adapter.
    loadCostFixture(database, currentDay);
    const rawCounts = rows(database, `
      select 'telemetry|' || count(*) from ctx.telemetry_event union all
      select 'installer|' || count(*) from ctx.install_attempt_event union all
      select 'blame|' || count(*) from ctx.blame_product_event order by 1
    `);
    assert.deepEqual(rawCounts, ["blame|37376", "installer|37376", "telemetry|37376"]);
    await maintenance.materializeProductTelemetryHistory();
    assert.deepEqual(rows(database, `
      select source_family || '|' || sum(event_count) from ctx.telemetry_diagnostic_daily
      group by source_family order by source_family
    `), ["blame|4608", "installer|4608", "telemetry|4608"]);

    // The adapter's exact single-day statement is profiled without publishing
    // observations; a multi-day HTTP request is no longer issued.
    t.diagnostic(JSON.stringify({ label: "scheduled-day", rawCounts,
      ...explainMaterialization(database, adapterStatement, 1) }));
    assert.deepEqual(rows(database, `
      select source_family || '|' || sum(event_count) from ctx.telemetry_diagnostic_daily
      group by source_family order by source_family
    `), ["blame|4608", "installer|4608", "telemetry|4608"]);
    for (const [column, predicate, index] of [
      ["received_at", "received_at is not null", "install_attempt_event_received_at_idx"],
      ["occurred_at", "received_at is null", "install_attempt_event_legacy_occurred_at_idx"],
    ]) {
      const plan = JSON.parse(scalar(database, `
        explain (analyze, buffers, format json)
        select * from ctx.install_attempt_event where ${predicate}
          and ${column} >= (date '${currentDay}' - 1)::timestamp at time zone 'utc'
          and ${column} < date '${currentDay}'::timestamp at time zone 'utc'
      `))[0].Plan;
      assert.ok(nodes(plan).some((node) => node["Index Name"] === index));
      assert.equal(plan["Actual Rows"], 256);
      t.diagnostic(JSON.stringify({ index, fixture_rows: 37376,
        actual_rows: plan["Actual Rows"], shared_hit_blocks: plan["Shared Hit Blocks"],
        shared_read_blocks: plan["Shared Read Blocks"] }));
    }
  } finally {
    database.cleanup();
  }
});

function indexState(database) {
  return rows(database, `select c.relname || '|' || i.indexrelid || '|' || i.indisvalid || '|' || i.indisready
    from pg_index i join pg_class c on c.oid = i.indexrelid
    where c.relname in ('install_attempt_event_received_at_idx', 'install_attempt_event_legacy_occurred_at_idx')
    order by c.relname`);
}

function verifyInvalidIndexRepair(database, validIndexes, t) {
  sql(database, "drop index concurrently ctx.install_attempt_event_legacy_occurred_at_idx");
  sql(database, `insert into ctx.install_attempt_event (install_attempt_id_hash, occurred_at, stage, status)
    values ('invalid-index-a', timestamptz '2020-01-01 12:00:00+00', 'installer', 'started'),
      ('invalid-index-b', timestamptz '2020-01-01 12:00:00+00', 'installer', 'started')`);
  // A real failed concurrent build leaves an invalid index; no catalog mutation.
  assert.throws(() => sql(database, `create unique index concurrently install_attempt_event_legacy_occurred_at_idx
    on ctx.install_attempt_event (occurred_at) where received_at is null`), /could not create unique index/u);
  const failedIndexes = indexState(database);
  assert.equal(failedIndexes[1], validIndexes[1], "the valid received index survives");
  assert.match(failedIndexes[0], /\|false\|/u);
  assert.throws(() => sqlFileAs(database, "0050_installer_history_date_indexes.sql", "ctx_migration"),
    /indexes are not valid and ready/u);
  assert.deepEqual(indexState(database), failedIndexes, "failed retry preserves index state");
  assert.equal(scalar(database, "select count(*) from ctx.install_attempt_event"), "2");
  // Explicit fixture repair removes only the known invalid index, then retries the real file.
  sql(database, "drop index concurrently ctx.install_attempt_event_legacy_occurred_at_idx");
  sqlFileAs(database, "0050_installer_history_date_indexes.sql", "ctx_migration");
  const repairedIndexes = indexState(database);
  assert.equal(repairedIndexes[1], validIndexes[1]);
  assert.ok(repairedIndexes.every((index) => index.endsWith("|true|true")));
  assert.equal(scalar(database, "select count(*) from ctx.install_attempt_event"), "2");
  t.diagnostic(JSON.stringify({ migration: "0050 real psql-file apply/repeat/failure/explicit repair",
    validIndexes, failedIndexes, repairedIndexes, raw_rows_preserved: 2 }));
}

function loadCostFixture(database, currentDay) {
  sql(database, `
    truncate ctx.telemetry_event, ctx.install_attempt_event, ctx.blame_product_event, ctx.telemetry_diagnostic_daily;
    create temporary table cost_fixture as
      select n, timestamptz '2020-01-01 12:00:00+00' + n * interval '1 minute' as stamp
      from generate_series(1, 32768) as retained(n)
      union all
      select 32768 + (age - 1) * 512 + n,
        ((date '${currentDay}' - age)::timestamp at time zone 'utc') + interval '12 hours' + n * interval '1 second'
      from generate_series(1, 9) as days(age), generate_series(1, 512) as recent(n);
    insert into ctx.telemetry_event (event_id, occurred_at, received_at, event_name, schema_version, plane,
      analytics_environment, traffic_class, app_version, os, arch, surface, duration_bucket,
      client_profile_id_hash, data_root_id_hash, properties)
      select 'cost-' || n, stamp, stamp, 'operation_completed', 1, 'product', 'staging', 'synthetic',
        '1.2.' || (n % 4), 'linux', 'x86_64', 'cli', 'lt_1s', repeat(md5((n % 257)::text), 2),
        repeat(md5((n % 127)::text), 2), jsonb_build_object('operation', case when n % 3 = 0 then 'show' else 'search' end,
          'outcome', case when n % 5 = 0 then 'failure' else 'success' end) from cost_fixture;
    insert into ctx.install_attempt_event (install_attempt_id_hash, occurred_at, received_at, stage, status,
      analytics_environment, traffic_class, platform, arch, version, duration_bucket)
      select repeat(md5((n % 257)::text), 2), stamp, case when n % 2 = 0 then stamp end, 'installer',
        case when n % 5 = 0 then 'failed' else 'started' end, 'staging', 'synthetic', 'linux', 'x86_64',
        '1.2.' || (n % 4), 'lt_1s' from cost_fixture;
    insert into ctx.blame_product_event (event_id, received_at, occurred_at, analytics_environment, traffic_class,
      activity_class, app_version, os, arch, duration_bucket, outcome, properties, identity_key_version, subject_hash)
      select 'cost-' || n, stamp, stamp, 'staging', 'synthetic', 'product_activity', '1.2.' || (n % 4),
        'linux', 'x86_64', 'lt_1s', case when n % 5 = 0 then 'failure' else 'success' end,
        jsonb_build_object('blame_surface', 'cli'), 1, repeat(md5((n % 257)::text), 2) from cost_fixture;
    analyze ctx.telemetry_event; analyze ctx.install_attempt_event; analyze ctx.blame_product_event;
  `);
}

function nodes(node) {
  return [node, ...(node.Plans ?? []).flatMap(nodes)];
}

function explainMaterialization(database, statement, days) {
  // Session-local instrumentation on the test cluster. Missing auto_explain fails.
  // TIMING OFF avoids per-node clock reads; total execution time is still reported.
  const result = spawnSync(path.join(postgresBin, "psql"), [
    "-h", database.socket, "-U", "postgres", "-d", "postgres", "-X", "-A", "-t", "-q",
    "-v", "ON_ERROR_STOP=1", "-c", `
      load 'auto_explain';
      set auto_explain.log_min_duration = 0;
      set auto_explain.log_analyze = on;
      set auto_explain.log_buffers = on;
      set auto_explain.log_timing = off;
      set auto_explain.log_nested_statements = on;
      set auto_explain.log_format = json;
      set auto_explain.log_level = notice;
      begin;
      set local timezone = 'Pacific/Kiritimati';
      set local role ctx_telemetry_retention;
      explain (analyze, buffers, format json, timing off) ${statement};
      rollback;
    `,
  ], { encoding: "utf8", timeout: 120000, maxBuffer: 16 * 1024 * 1024 });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  const outer = JSON.parse(result.stdout)[0];
  const nested = [...result.stderr.matchAll(/plan:\s*(\{[\s\S]*?\n\})/gu)].map((match) => JSON.parse(match[1]));
  const inserts = nested.filter((plan) => /^insert into ctx\.telemetry_diagnostic_daily/iu.test(plan["Query Text"].trim()));
  const deletes = nested.filter((plan) => /^delete from ctx\.telemetry_diagnostic_daily/iu.test(plan["Query Text"].trim()));
  assert.equal(inserts.length, days, "must observe the actual daily aggregate insert plans");
  assert.equal(deletes.length, days, "must observe the actual daily aggregate delete plans");
  for (const plan of inserts) {
    const all = nodes(plan.Plan);
    for (const relation of ["telemetry_event", "install_attempt_event", "blame_product_event"]) {
      assert.ok(all.some((node) => node["Relation Name"] === relation), `actual plan must include ${relation}`);
    }
    assert.ok(all.some((node) => node["Node Type"].includes("Aggregate")));
  }
  // Full plans preserve rows, buffer counts and available sort/memory/disk/temp fields.
  // Inclusive node counters are not summed, and no absence-of-spill claim is inferred.
  return { days, statement, settings: rows(database, `select name || '=' || setting from pg_settings
    where name in ('server_version', 'work_mem', 'shared_buffers', 'effective_cache_size',
      'random_page_cost', 'seq_page_cost', 'max_parallel_workers_per_gather') order by name`), outer, nested };
}
