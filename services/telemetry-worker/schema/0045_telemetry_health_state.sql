begin;

set local lock_timeout = '5s';
set local statement_timeout = '2min';
set local idle_in_transaction_session_timeout = '2min';

select pg_advisory_xact_lock(hashtext('ctx.telemetry_health_state.v1'));

do $precondition$
begin
  if to_regprocedure('ctx.telemetry_ingest_health_snapshot(text)') is null
    or to_regclass('ctx.telemetry_failures_live') is null
  then
    raise exception 'telemetry health state requires migration 0044';
  end if;
end
$precondition$;

-- Keep recent incident diagnosis on the analytics-reader boundary while
-- exposing the closed delivery buckets needed to distinguish transient retry,
-- live backlog, and actual drops. Existing columns retain their order; the new
-- dimensions are appended for view compatibility.
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
  count(distinct event.data_root_id_hash)::bigint as data_root_count,
  coalesce(event.properties ->> 'failure_class', 'none')
    as delivery_failure_class,
  coalesce(event.properties ->> 'queued_count_bucket', 'none')
    as queued_count_bucket,
  coalesce(event.properties ->> 'retry_attempt_count_bucket', 'none')
    as retry_attempt_count_bucket,
  coalesce(event.properties ->> 'dropped_count_bucket', 'none')
    as dropped_count_bucket,
  coalesce(event.properties ->> 'oldest_queued_age_bucket', 'none')
    as oldest_queued_age_bucket
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
  coalesce(event.properties ->> 'retryable', 'none'),
  coalesce(event.properties ->> 'failure_class', 'none'),
  coalesce(event.properties ->> 'queued_count_bucket', 'none'),
  coalesce(event.properties ->> 'retry_attempt_count_bucket', 'none'),
  coalesce(event.properties ->> 'dropped_count_bucket', 'none'),
  coalesce(event.properties ->> 'oldest_queued_age_bucket', 'none');

alter view ctx.telemetry_failures_live owner to ctx_migration;
comment on view ctx.telemetry_failures_live is
  'Restricted 14-day hourly telemetry failure counts by closed diagnostic and delivery-state dimensions; no event IDs or identity hashes are exposed.';
revoke all on table ctx.telemetry_failures_live
  from public, ctx_control_plane, ctx_telemetry_ingest,
    ctx_telemetry_retention, ctx_product_health_readonly;
grant select on table ctx.telemetry_failures_live
  to ctx_migration, ctx_analytics_readonly;

-- The public health endpoint reports current affected profile/root grains, not
-- the volume of recurring events. Delivery and refresh state are selected from
-- each grain's latest accepted observation in the current/previous clock hour.
-- Legacy refresh producers without the structured retryability field remain
-- available to incident views but cannot permanently swamp this live signal.
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
       where status = 'failure') as delivery_degraded_count,
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
