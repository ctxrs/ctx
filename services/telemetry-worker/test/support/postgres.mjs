// Synthetic local schema and isolated PostgreSQL helpers; no production data.
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { requirePostgres } from "./postgres-test-prerequisite.mjs";

export const postgresBin = requirePostgres();

export const BASE_SCHEMA = String.raw`
create role neondb_owner login noinherit;
create role ctx_migration login noinherit;
grant ctx_migration to neondb_owner
  with admin option, inherit false, set false;
create role ctx_analytics_readonly login noinherit;
create role ctx_control_plane login noinherit;
create role ctx_telemetry_ingest login noinherit;
create role ctx_telemetry_retention login noinherit;
create role ctx_product_health_readonly login noinherit;

create schema ctx authorization ctx_migration;
alter schema ctx owner to neondb_owner;
grant usage, create on schema ctx to ctx_migration;
grant usage on schema ctx to ctx_analytics_readonly, ctx_telemetry_ingest,
  ctx_telemetry_retention;

create table ctx.telemetry_event (
  event_id text primary key,
  install_id_hash text,
  device_id_hash text,
  broker_install_id_hash text,
  broker_device_id_hash text,
  origin_install_id_hash text,
  origin_device_id_hash text,
  occurred_at timestamptz not null,
  received_at timestamptz,
  ingested_at timestamptz default clock_timestamp(),
  event_name text not null,
  event_version integer,
  schema_version integer,
  plane text,
  broker_runtime text,
  origin_runtime text,
  source text,
  analytics_environment text,
  traffic_class text,
  activity_class text,
  app_version text,
  os text,
  arch text,
  surface text,
  env_target text,
  provider_id text,
  model_id text,
  duration_ms bigint,
  duration_bucket text,
  status text,
  success boolean,
  session_root_kind text,
  client_profile_id_hash text,
  data_root_id_hash text,
  identity_key_version integer,
  payload_fingerprint text,
  properties jsonb not null default '{}'::jsonb,
  constraint telemetry_event_new_family_schema_chk check (true),
  constraint telemetry_event_typed_v1_contract_chk check (true)
);
alter table ctx.telemetry_event owner to neondb_owner;
grant all privileges on table ctx.telemetry_event to ctx_migration;

create table ctx.install_attempt_event (
  id uuid primary key default gen_random_uuid(),
  event_id text,
  install_attempt_id_hash text not null,
  occurred_at timestamptz not null,
  received_at timestamptz,
  event_name text,
  analytics_environment text,
  traffic_class text,
  stage text not null,
  status text not null,
  platform text,
  arch text,
  version text,
  duration_bucket text
);
alter table ctx.install_attempt_event owner to ctx_migration;

create table ctx.blame_product_event (
  event_id text primary key,
  received_at timestamptz not null,
  occurred_at timestamptz not null,
  analytics_environment text not null,
  traffic_class text not null,
  activity_class text not null,
  app_version text not null,
  os text not null,
  arch text not null,
  duration_bucket text not null,
  outcome text not null,
  properties jsonb not null,
  identity_key_version integer not null,
  subject_hash text not null
);
alter table ctx.blame_product_event owner to ctx_migration;

create table ctx.telemetry_ingest_rejection_hourly (
  analytics_environment text not null,
  rejection_hour timestamptz not null,
  ingest_endpoint text not null,
  event_family text not null,
  rejection_class text not null,
  rejected_request_count bigint not null,
  primary key (
    analytics_environment, rejection_hour, ingest_endpoint,
    event_family, rejection_class
  )
);
alter table ctx.telemetry_ingest_rejection_hourly owner to ctx_migration;

create function ctx.delete_expired_raw_product_telemetry(timestamptz)
returns bigint language sql as 'select 99::bigint';
alter function ctx.delete_expired_raw_product_telemetry(timestamptz)
  owner to ctx_migration;

create function ctx.telemetry_ingest_health_snapshot(text)
returns table (
  compatibility_rejection_max bigint,
  event_collision_count bigint,
  other_rejection_count bigint
)
language sql as 'select 0::bigint, 0::bigint, 0::bigint';
alter function ctx.telemetry_ingest_health_snapshot(text) owner to ctx_migration;
`;

export function startPostgres() {
  const root = mkdtempSync(path.join(process.env.TEST_TMPDIR ?? tmpdir(), "ctx-telemetry-history-"));
  const data = path.join(root, "data");
  const socket = path.join(root, "socket");
  const log = path.join(root, "postgres.log");
  mkdirSync(socket);
  execFileSync(path.join(postgresBin, "initdb"), [
    "-D", data, "-A", "trust", "-U", "postgres", "--no-locale", "--encoding=UTF8",
  ], { stdio: "pipe" });
  execFileSync(path.join(postgresBin, "pg_ctl"), [
    "-D", data, "-l", log, "-o", `-F -k ${socket} -h ''`, "-w", "start",
  ], { stdio: "pipe" });
  return {
    socket,
    cleanup() {
      try {
        execFileSync(path.join(postgresBin, "pg_ctl"), ["-D", data, "-m", "immediate", "-w", "stop"], {
          stdio: "pipe",
        });
      } finally {
        rmSync(root, { recursive: true, force: true });
      }
    },
  };
}

export function sql(database, statement) {
  execFileSync(path.join(postgresBin, "psql"), [
    "-h", database.socket, "-U", "postgres", "-d", "postgres",
    "-v", "ON_ERROR_STOP=1", "-X", "-q", "-c", statement,
  ], { stdio: "pipe" });
}

export function sqlFileAs(database, name, role) {
  execFileSync(path.join(postgresBin, "psql"), [
    "-h", database.socket, "-U", "postgres", "-d", "postgres",
    "-v", "ON_ERROR_STOP=1", "-X", "-q", "-c", `set role ${role}`,
    "-f", fileURLToPath(new URL(`../../schema/${name}`, import.meta.url)),
  ], { stdio: "pipe" });
}
export function scalar(database, statement) {
  return execFileSync(path.join(postgresBin, "psql"), [
    "-h", database.socket, "-U", "postgres", "-d", "postgres",
    "-v", "ON_ERROR_STOP=1", "-X", "-A", "-t", "-q", "-c", statement,
  ], { encoding: "utf8" }).trim();
}

export function rows(database, statement) {
  const output = scalar(database, statement);
  return output.length === 0 ? [] : output.split("\n");
}
