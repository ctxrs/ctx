begin;

set local lock_timeout = '5s';
set local statement_timeout = '30s';

do $precondition$
begin
  if current_user <> 'ctx_migration'
    or to_regclass('ctx.analytics_canonical_telemetry_events') is null
    or not exists (select 1 from pg_roles where rolname = 'ctx_analytics_readonly')
  then
    raise exception 'rollout telemetry requires ctx_migration, the canonical telemetry view, and ctx_analytics_readonly';
  end if;
end
$precondition$;

create or replace function ctx.analytics_rollout_window(
  p_environment text,
  p_from timestamptz,
  p_until timestamptz,
  p_versions text[] default null
)
returns table (
  aggregation_level text,
  schema_version integer,
  app_version text,
  event_name text,
  surface text,
  os text,
  arch text,
  operation text,
  outcome text,
  upgrade_mode text,
  upgrade_operation text,
  upgrade_status text,
  upgrade_applied boolean,
  upgrade_channel text,
  upgrade_failure_kind text,
  provider_id text,
  refresh_result text,
  failure_scope text,
  failure_type text,
  failure_code text,
  retryable boolean,
  refresh_failure_stage text,
  refresh_failure_kind text,
  refresh_failure_reason text,
  refresh_coverage_reason text,
  refresh_source_failure_class text,
  event_count bigint,
  client_profile_count bigint,
  data_root_count bigint,
  legacy_origin_install_count bigint
)
language plpgsql stable security definer
set search_path = pg_catalog, ctx, pg_temp
as $function$
begin
  if p_environment is null or p_environment not in ('production', 'staging')
    or p_from is null or p_until is null
    or not isfinite(p_from) or not isfinite(p_until)
    or p_until <= p_from or p_until - p_from > interval '24 hours'
  then
    raise exception using errcode = '22023',
      message = 'expected production/staging and a finite positive window of at most 24 hours';
  end if;
  if exists (
    select 1 from unnest(p_versions) as version(value)
    where version.value is null or btrim(version.value) = ''
  ) then
    raise exception using errcode = '22023', message = 'version filters must contain nonempty values';
  end if;

  return query
  with bounded as (
    select
      event.schema_version, event.app_version, event.event_name,
      event.surface, event.os, event.arch, event.operation, event.outcome,
      event.provider_id, event.client_profile_id_hash, event.data_root_id_hash,
      event.legacy_origin_install_id_hash,
      case when event.operation = 'upgrade'
        and event.event_name in ('cli_invocation', 'operation_completed')
        then event.properties else '{}'::jsonb end as upgrade,
      case when event.event_name = 'provider_refresh_completed'
        then event.properties else '{}'::jsonb end as refresh
    from ctx.analytics_canonical_telemetry_events as event
    where event.analytics_environment = p_environment
      and event.occurred_at >= p_from
      and event.occurred_at < p_until
      and event.is_eligible_product_telemetry
      and (p_versions is null or event.app_version = any(p_versions))
  ), dimensions as (
    select
      b.schema_version, b.app_version, b.event_name, b.surface, b.os, b.arch,
      b.operation, b.outcome, b.client_profile_id_hash, b.data_root_id_hash,
      b.legacy_origin_install_id_hash,
      b.upgrade ->> 'upgrade_mode' as upgrade_mode,
      b.upgrade ->> 'upgrade_operation' as upgrade_operation,
      b.upgrade ->> 'upgrade_status' as upgrade_status,
      case when jsonb_typeof(b.upgrade -> 'upgrade_applied') = 'boolean'
        then (b.upgrade ->> 'upgrade_applied')::boolean end as upgrade_applied,
      b.upgrade ->> 'upgrade_channel' as upgrade_channel,
      b.upgrade ->> 'upgrade_failure_kind' as upgrade_failure_kind,
      b.provider_id,
      b.refresh ->> 'refresh_result' as refresh_result,
      b.refresh ->> 'failure_scope' as failure_scope,
      b.refresh ->> 'failure_type' as failure_type,
      b.refresh ->> 'failure_code' as failure_code,
      case when jsonb_typeof(b.refresh -> 'retryable') = 'boolean'
        then (b.refresh ->> 'retryable')::boolean end as retryable,
      b.refresh ->> 'refresh_failure_stage' as refresh_failure_stage,
      b.refresh ->> 'refresh_failure_kind' as refresh_failure_kind,
      b.refresh ->> 'refresh_failure_reason' as refresh_failure_reason,
      b.refresh ->> 'refresh_coverage_reason' as refresh_coverage_reason,
      b.refresh ->> 'refresh_source_failure_class' as refresh_source_failure_class
    from bounded as b
  )
  select
    case when grouping(d.event_name) = 1 then 'version' else 'event' end,
    d.schema_version, d.app_version, d.event_name, d.surface, d.os, d.arch,
    d.operation, d.outcome, d.upgrade_mode, d.upgrade_operation,
    d.upgrade_status, d.upgrade_applied, d.upgrade_channel, d.upgrade_failure_kind,
    d.provider_id, d.refresh_result, d.failure_scope, d.failure_type,
    d.failure_code, d.retryable, d.refresh_failure_stage, d.refresh_failure_kind,
    d.refresh_failure_reason, d.refresh_coverage_reason, d.refresh_source_failure_class,
    count(*)::bigint,
    count(distinct d.client_profile_id_hash)::bigint,
    count(distinct d.data_root_id_hash)::bigint,
    count(distinct d.legacy_origin_install_id_hash)
      filter (where d.schema_version is null)::bigint
  from dimensions as d
  group by grouping sets (
    (d.schema_version, d.app_version),
    (d.schema_version, d.app_version, d.event_name, d.surface, d.os, d.arch,
     d.operation, d.outcome, d.upgrade_mode, d.upgrade_operation,
     d.upgrade_status, d.upgrade_applied, d.upgrade_channel, d.upgrade_failure_kind,
     d.provider_id, d.refresh_result, d.failure_scope, d.failure_type,
     d.failure_code, d.retryable, d.refresh_failure_stage, d.refresh_failure_kind,
     d.refresh_failure_reason, d.refresh_coverage_reason, d.refresh_source_failure_class)
  );
end
$function$;

alter function ctx.analytics_rollout_window(text, timestamptz, timestamptz, text[])
  owner to ctx_migration;
revoke all on function ctx.analytics_rollout_window(text, timestamptz, timestamptz, text[])
  from public, ctx_analytics_readonly, ctx_control_plane, ctx_telemetry_ingest,
    ctx_telemetry_retention, ctx_product_health_readonly;
grant execute on function ctx.analytics_rollout_window(text, timestamptz, timestamptz, text[])
  to ctx_analytics_readonly;
comment on function ctx.analytics_rollout_window(text, timestamptz, timestamptz, text[]) is
  'Eligible canonical events in a half-open occurrence-time window (at most 24 hours). Version rows deduplicate observed profiles/roots within each version; event rows overlap. Counts are not machines, all installations, destination upgrade versions, or a completeness watermark. Nullable old-producer diagnostics remain unknown. Delivery observations are outside the canonical population.';

commit;
