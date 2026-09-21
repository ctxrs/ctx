begin;

set local lock_timeout = '5s';
set local statement_timeout = '2min';
set local idle_in_transaction_session_timeout = '2min';

-- Stop deletion first. An older Worker may continue invoking this function
-- until the Worker rollout reaches it, so compatibility must be harmless.
create or replace function ctx.delete_expired_raw_product_telemetry(
  p_before timestamptz default clock_timestamp() - interval '180 days'
)
returns bigint
language plpgsql
volatile
security definer
set search_path = pg_catalog, ctx
as $disabled_retention$
begin
  perform p_before;
  return 0;
end
$disabled_retention$;

alter function ctx.delete_expired_raw_product_telemetry(timestamptz)
  owner to ctx_migration;
comment on function ctx.delete_expired_raw_product_telemetry(timestamptz) is
  'Compatibility no-op. Raw product telemetry deletion is disabled; permanent identity-free diagnostics are materialized separately.';
revoke all on function ctx.delete_expired_raw_product_telemetry(timestamptz)
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_ingest, ctx_product_health_readonly;
grant execute on function ctx.delete_expired_raw_product_telemetry(timestamptz)
  to ctx_migration, ctx_telemetry_retention;

alter table ctx.telemetry_ingest_rejection_hourly
  drop constraint if exists telemetry_ingest_rejection_hourly_event_family_check;
alter table ctx.telemetry_ingest_rejection_hourly
  add constraint telemetry_ingest_rejection_hourly_event_family_check
    check (event_family in (
      'batch',
      'analytics_delivery_observation',
      'cli_invocation',
      'operation_completed',
      'provider_refresh_completed',
      'runtime_observation',
      'install_stage',
      'unknown'
    )) not valid;
alter table ctx.telemetry_ingest_rejection_hourly
  validate constraint telemetry_ingest_rejection_hourly_event_family_check;

-- Preserve the bounded stable rejection code permanently. The code is chosen
-- by the Worker from a closed validation vocabulary; no payload value, unknown
-- field name, identifier, status text, or raw error reaches this table.
alter table ctx.telemetry_ingest_rejection_hourly
  add column if not exists rejection_code text not null default 'legacy_unknown',
  add column if not exists app_version text not null default 'unknown',
  add column if not exists field_shape_fingerprint text not null default 'none',
  add column if not exists field_shape_overflow boolean not null default false,
  add column if not exists provider_classification text not null default 'unknown',
  add column if not exists size_bucket text not null default 'unknown';
alter table ctx.telemetry_ingest_rejection_hourly
  drop constraint if exists telemetry_ingest_rejection_hourly_rejection_code_check,
  drop constraint if exists telemetry_ingest_rejection_hourly_app_version_check,
  drop constraint if exists telemetry_ingest_rejection_hourly_shape_fingerprint_check,
  drop constraint if exists telemetry_ingest_rejection_hourly_provider_classification_check,
  drop constraint if exists telemetry_ingest_rejection_hourly_size_bucket_check;
alter table ctx.telemetry_ingest_rejection_hourly
  add constraint telemetry_ingest_rejection_hourly_rejection_code_check
    check (rejection_code ~ '^[a-z][a-z0-9_]{0,63}$') not valid,
  add constraint telemetry_ingest_rejection_hourly_app_version_check
    check (app_version ~ '^[0-9A-Za-z][0-9A-Za-z.+_-]{0,63}$') not valid,
  add constraint telemetry_ingest_rejection_hourly_shape_fingerprint_check
    check (
      field_shape_fingerprint = 'none'
      or field_shape_fingerprint ~ '^[0-9a-f]{64}$'
    ) not valid,
  add constraint telemetry_ingest_rejection_hourly_provider_classification_check
    check (provider_classification in (
      'current', 'historical', 'unrecognized', 'neutral', 'invalid', 'mixed', 'unknown'
    )) not valid,
  add constraint telemetry_ingest_rejection_hourly_size_bucket_check
    check (size_bucket in (
      'lt_1kb', '1kb_8kb', '8kb_64kb', '64kb_256kb', '256kb_plus', 'unknown'
    )) not valid;
alter table ctx.telemetry_ingest_rejection_hourly
  validate constraint telemetry_ingest_rejection_hourly_rejection_code_check,
  validate constraint telemetry_ingest_rejection_hourly_app_version_check,
  validate constraint telemetry_ingest_rejection_hourly_shape_fingerprint_check,
  validate constraint telemetry_ingest_rejection_hourly_provider_classification_check,
  validate constraint telemetry_ingest_rejection_hourly_size_bucket_check;

alter table ctx.telemetry_ingest_rejection_hourly
  drop constraint if exists telemetry_ingest_rejection_hourly_pkey;
alter table ctx.telemetry_ingest_rejection_hourly
  add primary key (
    analytics_environment,
    rejection_hour,
    ingest_endpoint,
    event_family,
    rejection_class,
    rejection_code,
    app_version,
    field_shape_fingerprint,
    field_shape_overflow,
    provider_classification,
    size_bucket
  );

create or replace function ctx.record_telemetry_ingest_rejection(
  p_analytics_environment text,
  p_ingest_endpoint text,
  p_event_family text,
  p_rejection_class text,
  p_rejection_code text,
  p_app_version text,
  p_field_shape_fingerprint text,
  p_field_shape_overflow boolean,
  p_provider_classification text,
  p_size_bucket text
)
returns void
language plpgsql
volatile
security definer
set search_path = pg_catalog, ctx
as $record_rejection$
declare
  rejected_at timestamptz := clock_timestamp();
begin
  if p_rejection_class not in (
    'body_too_large',
    'invalid_json',
    'invalid_envelope',
    'invalid_identity',
    'invalid_event',
    'invalid_properties',
    'too_many_events',
    'event_collision',
    'other'
  ) then
    raise exception using
      errcode = '23514',
      message = 'unsupported telemetry rejection class';
  end if;
  if p_rejection_code is null
    or p_rejection_code !~ '^[a-z][a-z0-9_]{0,63}$'
  then
    raise exception using
      errcode = '23514',
      message = 'unsupported telemetry rejection code';
  end if;
  if p_app_version is null
    or p_app_version !~ '^[0-9A-Za-z][0-9A-Za-z.+_-]{0,63}$'
  then
    raise exception using
      errcode = '23514',
      message = 'unsupported telemetry rejection app version';
  end if;
  if p_field_shape_fingerprint is null
    or not (
      p_field_shape_fingerprint = 'none'
      or p_field_shape_fingerprint ~ '^[0-9a-f]{64}$'
    )
  then
    raise exception using
      errcode = '23514',
      message = 'unsupported telemetry rejection field shape';
  end if;
  if p_field_shape_overflow is null
    or p_provider_classification not in (
      'current', 'historical', 'unrecognized', 'neutral', 'invalid', 'mixed', 'unknown'
    )
    or p_size_bucket not in (
      'lt_1kb', '1kb_8kb', '8kb_64kb', '64kb_256kb', '256kb_plus', 'unknown'
    )
  then
    raise exception using
      errcode = '23514',
      message = 'unsupported telemetry rejection diagnostics';
  end if;

  insert into ctx.telemetry_ingest_rejection_hourly (
    analytics_environment,
    rejection_hour,
    ingest_endpoint,
    event_family,
    rejection_class,
    rejection_code,
    app_version,
    field_shape_fingerprint,
    field_shape_overflow,
    provider_classification,
    size_bucket,
    rejected_request_count
  ) values (
    p_analytics_environment,
    date_trunc('hour', rejected_at),
    p_ingest_endpoint,
    p_event_family,
    p_rejection_class,
    p_rejection_code,
    p_app_version,
    p_field_shape_fingerprint,
    p_field_shape_overflow,
    p_provider_classification,
    p_size_bucket,
    1
  )
  on conflict (
    analytics_environment,
    rejection_hour,
    ingest_endpoint,
    event_family,
    rejection_class,
    rejection_code,
    app_version,
    field_shape_fingerprint,
    field_shape_overflow,
    provider_classification,
    size_bucket
  ) do update set
    rejected_request_count =
      ctx.telemetry_ingest_rejection_hourly.rejected_request_count + 1;
end
$record_rejection$;

alter function ctx.record_telemetry_ingest_rejection(
  text, text, text, text, text, text, text, boolean, text, text
)
  owner to ctx_migration;
comment on function ctx.record_telemetry_ingest_rejection(
  text, text, text, text, text, text, text, boolean, text, text
) is
  'Increments one permanent server-hour rejection count by bounded class, stable code, app version, content-free shape fingerprint, provider classification, and size bucket; accepts no payload, identifier, field value, status text, or raw error.';

-- Keep an old Worker harmless during a staged rollout. It continues using the
-- four-argument signature and records an explicit legacy code.
create or replace function ctx.record_telemetry_ingest_rejection(
  p_analytics_environment text,
  p_ingest_endpoint text,
  p_event_family text,
  p_rejection_class text
)
returns void
language sql
volatile
security definer
set search_path = pg_catalog, ctx
as $legacy_record_rejection$
  select ctx.record_telemetry_ingest_rejection(
    p_analytics_environment,
    p_ingest_endpoint,
    p_event_family,
    p_rejection_class,
    'legacy_unknown',
    'unknown',
    'none',
    false,
    'unknown',
    'unknown'
  )
$legacy_record_rejection$;

alter function ctx.record_telemetry_ingest_rejection(text, text, text, text)
  owner to ctx_migration;
revoke all on function ctx.record_telemetry_ingest_rejection(
  text, text, text, text, text, text, text, boolean, text, text
)
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_retention, ctx_product_health_readonly;
revoke all on function ctx.record_telemetry_ingest_rejection(text, text, text, text)
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_retention, ctx_product_health_readonly;
grant execute on function ctx.record_telemetry_ingest_rejection(
  text, text, text, text, text, text, text, boolean, text, text
)
  to ctx_migration, ctx_telemetry_ingest;
grant execute on function ctx.record_telemetry_ingest_rejection(text, text, text, text)
  to ctx_migration, ctx_telemetry_ingest;

create or replace view ctx.analytics_ingest_rejections_hourly
with (security_barrier = true) as
select
  analytics_environment,
  rejection_hour,
  ingest_endpoint,
  event_family,
  rejection_class,
  rejected_request_count,
  rejection_code,
  app_version,
  field_shape_fingerprint,
  field_shape_overflow,
  provider_classification,
  size_bucket
from ctx.telemetry_ingest_rejection_hourly;
alter view ctx.analytics_ingest_rejections_hourly owner to ctx_migration;
revoke all on table ctx.analytics_ingest_rejections_hourly
  from public, ctx_control_plane, ctx_telemetry_ingest,
    ctx_telemetry_retention, ctx_product_health_readonly;
grant select on table ctx.analytics_ingest_rejections_hourly
  to ctx_migration, ctx_analytics_readonly;

-- This table is permanent diagnostic history. Its dimensions all come from
-- closed admitted fields. It never stores event IDs, identity hashes, key
-- versions, exact timestamps, payloads, or raw errors.
create table if not exists ctx.telemetry_diagnostic_daily (
  received_date date not null,
  source_family text not null
    check (source_family in ('telemetry', 'installer', 'blame')),
  analytics_environment text not null,
  traffic_class text not null,
  event_name text not null,
  app_version text not null,
  os text not null,
  arch text not null,
  surface text not null,
  operation text not null,
  outcome text not null,
  duration_bucket text not null,
  provider_id text not null,
  trigger text not null,
  failure_scope text not null,
  failure_type text not null,
  failure_code text not null,
  retryable_state text not null
    check (retryable_state in ('none', 'true', 'false')),
  delivery_failure_class text not null,
  queued_count_bucket text not null,
  retry_attempt_count_bucket text not null,
  dropped_count_bucket text not null,
  oldest_queued_age_bucket text not null,
  event_count bigint not null check (event_count > 0),
  profile_count bigint not null check (profile_count >= 0),
  data_root_count bigint not null check (data_root_count >= 0),
  primary key (
    received_date, source_family, analytics_environment, traffic_class,
    event_name, app_version, os, arch, surface, operation, outcome,
    duration_bucket, provider_id, trigger, failure_scope, failure_type,
    failure_code, retryable_state, delivery_failure_class,
    queued_count_bucket, retry_attempt_count_bucket, dropped_count_bucket,
    oldest_queued_age_bucket
  )
);

alter table ctx.telemetry_diagnostic_daily owner to ctx_migration;
comment on table ctx.telemetry_diagnostic_daily is
  'Permanent identity-free daily counts over closed product telemetry, installer, and Blame dimensions; contains no event IDs, identity hashes, raw payloads, or exact timestamps.';
revoke all on table ctx.telemetry_diagnostic_daily
  from public, ctx_control_plane, ctx_telemetry_ingest,
    ctx_telemetry_retention, ctx_product_health_readonly;
grant select on table ctx.telemetry_diagnostic_daily
  to ctx_migration, ctx_analytics_readonly;

create or replace function ctx.materialize_product_telemetry_history(
  p_received_date date default (
    (clock_timestamp() at time zone 'utc')::date - 1
  )
)
returns bigint
language plpgsql
volatile
security definer
set search_path = pg_catalog, ctx
as $materialize$
declare
  materialized_count bigint := 0;
begin
  if p_received_date is null
    or p_received_date >= (clock_timestamp() at time zone 'utc')::date
  then
    raise exception 'telemetry history date must be a complete UTC day';
  end if;

  delete from ctx.telemetry_diagnostic_daily
  where received_date = p_received_date;

  insert into ctx.telemetry_diagnostic_daily (
    received_date, source_family, analytics_environment, traffic_class,
    event_name, app_version, os, arch, surface, operation, outcome,
    duration_bucket, provider_id, trigger, failure_scope, failure_type,
    failure_code, retryable_state, delivery_failure_class,
    queued_count_bucket, retry_attempt_count_bucket, dropped_count_bucket,
    oldest_queued_age_bucket,
    event_count, profile_count, data_root_count
  )
  with admitted as (
    select
      (coalesce(event.received_at, event.occurred_at) at time zone 'utc')::date
        as received_date,
      'telemetry'::text as source_family,
      coalesce(event.analytics_environment, 'unknown') as analytics_environment,
      coalesce(event.traffic_class, 'unknown') as traffic_class,
      event.event_name,
      coalesce(event.app_version, 'unknown') as app_version,
      coalesce(event.os, 'unknown') as os,
      coalesce(event.arch, 'unknown') as arch,
      coalesce(event.surface, 'unknown') as surface,
      coalesce(event.properties ->> 'operation', 'unknown') as operation,
      coalesce(event.properties ->> 'outcome', event.status, 'unknown') as outcome,
      coalesce(event.duration_bucket, 'unknown') as duration_bucket,
      coalesce(event.provider_id, 'none') as provider_id,
      coalesce(event.properties ->> 'trigger', 'none') as trigger,
      coalesce(event.properties ->> 'failure_scope', 'none') as failure_scope,
      coalesce(
        event.properties ->> 'failure_type',
        event.properties ->> 'failure_class',
        'none'
      ) as failure_type,
      coalesce(event.properties ->> 'failure_code', 'none') as failure_code,
      case jsonb_typeof(event.properties -> 'retryable')
        when 'boolean' then event.properties ->> 'retryable'
        else 'none'
      end as retryable_state,
      coalesce(event.properties ->> 'failure_class', 'none')
        as delivery_failure_class,
      coalesce(event.properties ->> 'queued_count_bucket', 'none')
        as queued_count_bucket,
      coalesce(event.properties ->> 'retry_attempt_count_bucket', 'none')
        as retry_attempt_count_bucket,
      coalesce(event.properties ->> 'dropped_count_bucket', 'none')
        as dropped_count_bucket,
      coalesce(event.properties ->> 'oldest_queued_age_bucket', 'none')
        as oldest_queued_age_bucket,
      event.client_profile_id_hash as profile_subject,
      event.data_root_id_hash as data_root_subject
    from ctx.telemetry_event as event
    where event.plane = 'product'
      and event.schema_version = 1
      and event.event_name in (
        'analytics_delivery_observation',
        'operation_completed',
        'provider_refresh_completed',
        'runtime_observation'
      )
      and (coalesce(event.received_at, event.occurred_at) at time zone 'utc')::date
        = p_received_date

    union all

    select
      (coalesce(event.received_at, event.occurred_at) at time zone 'utc')::date,
      'installer',
      coalesce(event.analytics_environment, 'unknown'),
      coalesce(event.traffic_class, 'unknown'),
      coalesce(event.event_name, 'install_stage'),
      coalesce(event.version, 'unknown'),
      coalesce(event.platform, 'unknown'),
      coalesce(event.arch, 'unknown'),
      'installer',
      event.stage,
      event.status,
      coalesce(event.duration_bucket, 'unknown'),
      'none', 'none', 'none', 'none', 'none', 'none', 'none', 'none', 'none',
      'none', 'none',
      event.install_attempt_id_hash,
      null::text
    from ctx.install_attempt_event as event
    where (coalesce(event.received_at, event.occurred_at) at time zone 'utc')::date
      = p_received_date

    union all

    select
      (event.received_at at time zone 'utc')::date,
      'blame',
      event.analytics_environment,
      event.traffic_class,
      'blame_product',
      event.app_version,
      event.os,
      event.arch,
      coalesce(event.properties ->> 'blame_surface', 'unknown'),
      'blame',
      event.outcome,
      event.duration_bucket,
      'none', 'none', 'none',
      coalesce(event.properties ->> 'blame_failure_class', 'none'),
      coalesce(event.properties ->> 'blame_failure_class', 'none'),
      'none', 'none', 'none', 'none', 'none', 'none',
      event.subject_hash,
      null::text
    from ctx.blame_product_event as event
    where (event.received_at at time zone 'utc')::date = p_received_date
  )
  select
    received_date, source_family, analytics_environment, traffic_class,
    event_name, app_version, os, arch, surface, operation, outcome,
    duration_bucket, provider_id, trigger, failure_scope, failure_type,
    failure_code, retryable_state, delivery_failure_class,
    queued_count_bucket, retry_attempt_count_bucket, dropped_count_bucket,
    oldest_queued_age_bucket,
    count(*)::bigint,
    count(distinct profile_subject)::bigint,
    count(distinct data_root_subject)::bigint
  from admitted
  group by
    received_date, source_family, analytics_environment, traffic_class,
    event_name, app_version, os, arch, surface, operation, outcome,
    duration_bucket, provider_id, trigger, failure_scope, failure_type,
    failure_code, retryable_state, delivery_failure_class,
    queued_count_bucket, retry_attempt_count_bucket, dropped_count_bucket,
    oldest_queued_age_bucket;

  get diagnostics materialized_count = row_count;
  return materialized_count;
end
$materialize$;

alter function ctx.materialize_product_telemetry_history(date)
  owner to ctx_migration;
comment on function ctx.materialize_product_telemetry_history(date) is
  'Idempotently materializes one complete UTC day of permanent identity-free telemetry diagnostics; it never deletes raw rows.';
revoke all on function ctx.materialize_product_telemetry_history(date)
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_ingest, ctx_product_health_readonly;
grant execute on function ctx.materialize_product_telemetry_history(date)
  to ctx_migration, ctx_telemetry_retention;

-- Prompt incident view for authorized operators. It exposes counts and closed
-- dimensions only, never event IDs or identity hashes.
create or replace view ctx.telemetry_failures_live
with (security_barrier = true, security_invoker = false) as
select
  date_trunc('hour', event.received_at at time zone 'utc')
    at time zone 'utc' as received_hour,
  event.analytics_environment,
  event.event_name,
  event.app_version,
  event.surface,
  coalesce(event.properties ->> 'operation', 'unknown') as operation,
  coalesce(event.provider_id, 'none') as provider_id,
  coalesce(event.properties ->> 'trigger', 'none') as trigger,
  coalesce(event.properties ->> 'failure_scope', 'none') as failure_scope,
  coalesce(
    event.properties ->> 'failure_type',
    event.properties ->> 'failure_class',
    'none'
  ) as failure_type,
  coalesce(event.properties ->> 'failure_code', 'none') as failure_code,
  coalesce(event.properties ->> 'retryable', 'none') as retryable_state,
  count(*)::bigint as event_count,
  count(distinct event.client_profile_id_hash)::bigint as profile_count,
  count(distinct event.data_root_id_hash)::bigint as data_root_count
from ctx.telemetry_event as event
where event.received_at >= statement_timestamp() - interval '14 days'
  and event.plane = 'product'
  and event.schema_version = 1
  and event.event_name in (
    'analytics_delivery_observation',
    'operation_completed',
    'provider_refresh_completed',
    'runtime_observation'
  )
  and (
    event.status = 'failure'
    or coalesce(event.properties ->> 'failure_scope', 'none') <> 'none'
    or coalesce(event.properties ->> 'failure_type', 'none') <> 'none'
    or coalesce(event.properties ->> 'failure_class', 'none') <> 'none'
    or coalesce(event.properties ->> 'dropped_count_bucket', '0') <> '0'
  )
group by
  date_trunc('hour', event.received_at at time zone 'utc')
    at time zone 'utc',
  event.analytics_environment, event.event_name, event.app_version,
  event.surface, coalesce(event.properties ->> 'operation', 'unknown'),
  coalesce(event.provider_id, 'none'),
  coalesce(event.properties ->> 'trigger', 'none'),
  coalesce(event.properties ->> 'failure_scope', 'none'),
  coalesce(
    event.properties ->> 'failure_type',
    event.properties ->> 'failure_class',
    'none'
  ),
  coalesce(event.properties ->> 'failure_code', 'none'),
  coalesce(event.properties ->> 'retryable', 'none');

alter view ctx.telemetry_failures_live owner to ctx_migration;
comment on view ctx.telemetry_failures_live is
  'Restricted 14-day hourly telemetry failure counts by closed diagnostic dimensions; no event IDs or identity hashes are exposed.';
revoke all on table ctx.telemetry_failures_live
  from public, ctx_control_plane, ctx_telemetry_ingest,
    ctx_telemetry_retention, ctx_product_health_readonly;
grant select on table ctx.telemetry_failures_live
  to ctx_migration, ctx_analytics_readonly;

-- Extend the existing bounded health receipt. Old Workers selecting the first
-- three named columns remain compatible; the new Worker also alerts on
-- refresh and delivery-health signals.
drop function ctx.telemetry_ingest_health_snapshot(text);
create function ctx.telemetry_ingest_health_snapshot(
  p_analytics_environment text
)
returns table (
  compatibility_rejection_max bigint,
  event_collision_count bigint,
  other_rejection_count bigint,
  provider_refresh_failure_count bigint,
  delivery_degraded_count bigint,
  delivery_dropped_count bigint
)
language plpgsql
stable
security definer
set search_path = pg_catalog, ctx
as $health$
begin
  if p_analytics_environment not in ('production', 'staging') then
    raise exception using
      errcode = '22023',
      message = 'unsupported telemetry analytics environment';
  end if;

  return query
  with grouped_rejection as (
    select
      rejection_class,
      sum(rejected_request_count)::bigint as grouped_rejection_count
    from ctx.telemetry_ingest_rejection_hourly
    where analytics_environment = p_analytics_environment
      and rejection_hour >=
        date_trunc('hour', statement_timestamp()) - interval '1 hour'
    group by ingest_endpoint, event_family, rejection_class
  ), rejection as (
    select
      coalesce(max(grouped_rejection_count) filter (
        where rejection_class in (
          'invalid_envelope', 'invalid_identity',
          'invalid_event', 'invalid_properties'
        )
      ), 0)::bigint as compatibility_rejection_max,
      coalesce(sum(grouped_rejection_count) filter (
        where rejection_class = 'event_collision'
      ), 0)::bigint as event_collision_count,
      coalesce(sum(grouped_rejection_count) filter (
        where rejection_class = 'other'
      ), 0)::bigint as other_rejection_count
    from grouped_rejection
  ), operational as (
    select
      count(*) filter (
        where event_name = 'provider_refresh_completed'
          and (
            status = 'failure'
            or coalesce(properties ->> 'failure_scope', 'none') <> 'none'
          )
      )::bigint as provider_refresh_failure_count,
      count(*) filter (
        where event_name = 'analytics_delivery_observation'
          and status = 'failure'
      )::bigint as delivery_degraded_count,
      count(*) filter (
        where event_name = 'analytics_delivery_observation'
          and coalesce(properties ->> 'dropped_count_bucket', '0') <> '0'
      )::bigint as delivery_dropped_count
    from ctx.telemetry_event
    where analytics_environment = p_analytics_environment
      and received_at >=
        date_trunc('hour', statement_timestamp()) - interval '1 hour'
  )
  select
    rejection.compatibility_rejection_max,
    rejection.event_collision_count,
    rejection.other_rejection_count,
    operational.provider_refresh_failure_count,
    operational.delivery_degraded_count,
    operational.delivery_dropped_count
  from rejection cross join operational;
end
$health$;

alter function ctx.telemetry_ingest_health_snapshot(text)
  owner to ctx_migration;
comment on function ctx.telemetry_ingest_health_snapshot(text) is
  'Returns bounded two-clock-hour ingestion rejection, provider-refresh failure, and analytics-delivery health counts; exposes no event rows, identities, payloads, or raw errors.';
revoke all on function ctx.telemetry_ingest_health_snapshot(text) from public;
grant execute on function ctx.telemetry_ingest_health_snapshot(text)
  to ctx_migration, ctx_telemetry_ingest;

commit;
