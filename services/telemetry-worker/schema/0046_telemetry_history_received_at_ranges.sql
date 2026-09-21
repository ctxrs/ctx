begin;

set local lock_timeout = '5s';
set local statement_timeout = '2min';
set local idle_in_transaction_session_timeout = '2min';

select pg_advisory_xact_lock(
  hashtext('ctx.materialize_product_telemetry_history.v2')
);

do $precondition$
begin
  if current_user <> 'ctx_migration'
    or to_regprocedure(
      'ctx.materialize_product_telemetry_history(date)'
    ) is null
    or to_regclass('ctx.telemetry_diagnostic_daily') is null
    or to_regclass('ctx.telemetry_event') is null
    or to_regclass('ctx.install_attempt_event') is null
    or to_regclass('ctx.blame_product_event') is null
    or not exists (
      select 1
      from pg_index
      where indexrelid = to_regclass(
          'ctx.telemetry_event_received_at_idx'
        )
        and indrelid = to_regclass('ctx.telemetry_event')
        and indisvalid
        and indisready
    )
    or not exists (
      select 1
      from pg_index
      where indexrelid = to_regclass(
          'ctx.blame_product_event_received_at_idx'
        )
        and indrelid = to_regclass('ctx.blame_product_event')
        and indisvalid
        and indisready
    )
  then
    raise exception
      'telemetry history received-at ranges require ctx_migration, migrations 0043-0045, and valid telemetry/Blame receipt indexes';
  end if;
end
$precondition$;

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
  received_day_start timestamptz;
  received_day_end timestamptz;
begin
  if p_received_date is null
    or p_received_date >= (clock_timestamp() at time zone 'utc')::date
  then
    raise exception 'telemetry history date must be a complete UTC day';
  end if;

  received_day_start :=
    p_received_date::timestamp without time zone at time zone 'utc';
  received_day_end :=
    (p_received_date + 1)::timestamp without time zone at time zone 'utc';

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
  with install_attempt_event_for_day as not materialized (
    select event.*
    from ctx.install_attempt_event as event
    where event.received_at >= received_day_start
      and event.received_at < received_day_end

    union all

    select event.*
    from ctx.install_attempt_event as event
    where event.received_at is null
      and event.occurred_at >= received_day_start
      and event.occurred_at < received_day_end
  ), admitted as (
    select
      p_received_date as received_date,
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
    -- Every telemetry row admitted here is schema_version 1, whose validated
    -- contract requires received_at. Older null-receipt rows have a null
    -- schema version and were never part of this aggregate; omitting an
    -- impossible fallback arm prevents a full scan of the raw telemetry table.
    from ctx.telemetry_event as event
    where event.plane = 'product'
      and event.schema_version = 1
      and event.event_name in (
        'analytics_delivery_observation',
        'operation_completed',
        'provider_refresh_completed',
        'runtime_observation'
      )
      and event.received_at >= received_day_start
      and event.received_at < received_day_end

    union all

    select
      p_received_date,
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
    from install_attempt_event_for_day as event

    union all

    select
      p_received_date,
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
    where event.received_at >= received_day_start
      and event.received_at < received_day_end
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
  'Idempotently materializes one complete UTC receipt day of permanent identity-free telemetry diagnostics, with occurred-time fallback only for legacy rows lacking received_at; it never deletes raw rows.';
revoke all on function ctx.materialize_product_telemetry_history(date)
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_ingest, ctx_telemetry_retention,
    ctx_product_health_readonly;
grant execute on function ctx.materialize_product_telemetry_history(date)
  to ctx_migration, ctx_telemetry_retention;

commit;
