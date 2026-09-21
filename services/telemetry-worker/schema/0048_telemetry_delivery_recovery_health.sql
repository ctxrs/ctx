begin;

set local lock_timeout = '5s';
set local statement_timeout = '2min';
set local idle_in_transaction_session_timeout = '2min';

select pg_advisory_xact_lock(
  hashtextextended('ctx.neon.migration.0048.telemetry_delivery_recovery_health', 0)
);

do $precondition$
begin
  if current_user <> 'ctx_migration'
    or to_regprocedure('ctx.telemetry_ingest_health_snapshot(text)') is null
  then
    raise exception
      'telemetry delivery recovery health requires ctx_migration and migration 0045';
  end if;
end
$precondition$;

-- Delivery health reflects the latest queue and drop buckets for each
-- privacy-safe profile/root grain. A historical failure status no longer
-- keeps a grain degraded after its queue and drop buckets recover to zero.
create or replace function ctx.telemetry_ingest_health_snapshot(
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
  ), latest_refresh as (
    select distinct on (
      event.client_profile_id_hash,
      event.data_root_id_hash
    )
      event.status
    from ctx.telemetry_event as event
    where event.analytics_environment = p_analytics_environment
      and event.received_at >=
        date_trunc('hour', statement_timestamp()) - interval '1 hour'
      and event.plane = 'product'
      and event.schema_version = 1
      and event.event_name = 'provider_refresh_completed'
      and jsonb_typeof(event.properties -> 'retryable') = 'boolean'
      and event.client_profile_id_hash is not null
      and event.data_root_id_hash is not null
    order by
      event.client_profile_id_hash,
      event.data_root_id_hash,
      event.received_at desc,
      event.event_id desc
  ), latest_delivery as (
    select distinct on (
      event.client_profile_id_hash,
      event.data_root_id_hash
    )
      event.status,
      coalesce(event.properties ->> 'queued_count_bucket', '0')
        as queued_count_bucket,
      coalesce(event.properties ->> 'dropped_count_bucket', '0')
        as dropped_count_bucket
    from ctx.telemetry_event as event
    where event.analytics_environment = p_analytics_environment
      and event.received_at >=
        date_trunc('hour', statement_timestamp()) - interval '1 hour'
      and event.plane = 'product'
      and event.schema_version = 1
      and event.event_name = 'analytics_delivery_observation'
      and event.client_profile_id_hash is not null
      and event.data_root_id_hash is not null
    order by
      event.client_profile_id_hash,
      event.data_root_id_hash,
      event.received_at desc,
      event.event_id desc
  ), operational as (
    select
      (select count(*)::bigint
       from latest_refresh
       where status = 'failure') as provider_refresh_failure_count,
      (select count(*)::bigint
       from latest_delivery
       where queued_count_bucket <> '0') as delivery_degraded_count,
      (select count(*)::bigint
       from latest_delivery
       where dropped_count_bucket <> '0') as delivery_dropped_count
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
  'Returns bounded two-clock-hour rejection counts and latest structured provider-refresh/delivery state counts by privacy-safe profile/root grain; exposes no event rows, identities, payloads, or raw errors.';
revoke all on function ctx.telemetry_ingest_health_snapshot(text) from public;
grant execute on function ctx.telemetry_ingest_health_snapshot(text)
  to ctx_migration, ctx_telemetry_ingest;

commit;
