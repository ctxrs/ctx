// Retained canonical view definition for authored, synthetic PostgreSQL fixtures.
// No production records; the runtime aggregate calls the established view.
export const CANONICAL_TELEMETRY_SCHEMA = String.raw`
create table ctx.telemetry_traffic_class_overrides (
  override_key text primary key, event_id text,
  origin_install_id_hash text, broker_install_id_hash text,
  client_profile_id_hash text, data_root_id_hash text, identity_key_version integer,
  app_version text, traffic_class text not null,
  created_at timestamptz not null default now(), expires_at timestamptz
);
alter table ctx.telemetry_traffic_class_overrides owner to ctx_migration;
create or replace view ctx.analytics_canonical_telemetry_events
with (security_barrier = true) as
with normalized as (
  select
    event.event_id,
    event.occurred_at,
    event.received_at,
    event.ingested_at,
    event.event_name,
    event.event_version,
    event.schema_version,
    event.plane,
    event.analytics_environment,
    coalesce(traffic_override.traffic_class, event.traffic_class) as traffic_class,
    event.activity_class as stored_activity_class,
    event.client_profile_id_hash,
    event.data_root_id_hash,
    event.identity_key_version,
    event.origin_install_id_hash as legacy_origin_install_id_hash,
    event.origin_device_id_hash as legacy_origin_device_id_hash,
    event.surface,
    event.origin_runtime,
    event.broker_runtime,
    event.app_version,
    event.os,
    event.arch,
    event.provider_id,
    event.duration_bucket,
    event.status,
    event.success,
    event.properties,
    case
      when event.event_name = 'cli_invocation' then event.properties ->> 'action'
      else event.properties ->> 'operation'
    end as operation,
    case
      when event.properties ->> 'outcome' in (
        'success',
        'failure',
        'cancelled',
        'partial',
        'skipped'
      ) then event.properties ->> 'outcome'
      when event.success is true then 'success'
      when event.success is false then 'failure'
      when event.status in (
        'success',
        'completed',
        'failure',
        'failed',
        'cancelled',
        'partial',
        'skipped'
      ) then case
        when event.status = 'completed' then 'success'
        when event.status = 'failed' then 'failure'
        else event.status
      end
      else 'unknown'
    end as outcome,
    event.traffic_class as raw_traffic_class,
    traffic_override.override_key as traffic_class_override_key,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'trigger' end as provider_trigger,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'source_mode' end as provider_source_mode,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'change' end as provider_change,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'content_evidence' end as provider_content_evidence,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'work_kind' end as provider_work_kind,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'refresh_result' end as provider_refresh_result,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'core_result' end as provider_core_result,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'canonical_pro_result' end as provider_canonical_pro_result,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'output_pro_result' end as provider_output_pro_result,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'failure_scope' end as provider_failure_scope,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'failure_type' end as provider_failure_type,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'outcome' end as provider_outcome,
    case
      when event.event_name = 'provider_refresh_completed'
        and jsonb_typeof(event.properties -> 'work_remaining') = 'boolean'
        then (event.properties ->> 'work_remaining')::boolean
    end as provider_work_remaining,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'sources_bucket' end as provider_sources_bucket,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'source_files_bucket' end as provider_source_files_bucket,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'sessions_bucket' end as provider_sessions_bucket,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'events_bucket' end as provider_events_bucket,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'edges_bucket' end as provider_edges_bucket,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'skips_bucket' end as provider_skips_bucket,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'rejections_bucket' end as provider_rejections_bucket,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'failures_bucket' end as provider_failures_bucket,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'retired_records_bucket' end as provider_retired_records_bucket,
    case when event.event_name = 'provider_refresh_completed'
      then event.properties ->> 'bytes_bucket' end as provider_bytes_bucket
  from ctx.telemetry_event event
  left join lateral (
    select override.*
    from ctx.telemetry_traffic_class_overrides override
    where (override.expires_at is null or override.expires_at > now())
      and (override.event_id is null or override.event_id = event.event_id)
      and (
        override.origin_install_id_hash is null
        or override.origin_install_id_hash = event.origin_install_id_hash
      )
      and (
        override.broker_install_id_hash is null
        or override.broker_install_id_hash = event.broker_install_id_hash
      )
      and (
        override.client_profile_id_hash is null
        or (
          override.client_profile_id_hash = event.client_profile_id_hash
          and override.identity_key_version = event.identity_key_version
        )
      )
      and (
        override.data_root_id_hash is null
        or (
          override.data_root_id_hash = event.data_root_id_hash
          and override.identity_key_version = event.identity_key_version
        )
      )
      and (override.app_version is null or override.app_version = event.app_version)
    order by
      (
        case when override.event_id is not null then 32 else 0 end
        + case when override.client_profile_id_hash is not null then 16 else 0 end
        + case when override.data_root_id_hash is not null then 8 else 0 end
        + case when override.origin_install_id_hash is not null then 4 else 0 end
        + case when override.broker_install_id_hash is not null then 2 else 0 end
        + case when override.app_version is not null then 1 else 0 end
      ) desc,
      override.created_at desc,
      override.override_key
    limit 1
  ) traffic_override on true
  where event.event_version = 1
    and event.event_name in (
      'cli_invocation',
      'operation_completed',
      'provider_refresh_completed',
      'runtime_observation'
    )
), classified as (
  select
    normalized.*,
    case
      when schema_version = 1 then stored_activity_class
      when event_name = 'cli_invocation' and operation in ('setup', 'setup_started') then 'setup'
      when event_name = 'cli_invocation' and operation = 'status' then 'status'
      when event_name = 'cli_invocation' and operation = 'upgrade' then 'automatic'
      when event_name = 'cli_invocation'
        and success is true
        and (
          (
            operation = 'search'
            and coalesce(properties ->> 'zero_result', 'true') = 'false'
          )
          or operation in ('show', 'locate', 'sql', 'work_graph')
        ) then 'product_value'
      when event_name = 'cli_invocation' then 'product_activity'
      else stored_activity_class
    end as activity_class
  from normalized
), eligible as (
  select
    classified.*,
    analytics_environment is not null
      and (
        (schema_version = 1 and traffic_class = 'unclassified_public')
        or (
          schema_version is null
          and traffic_class in ('user', 'unclassified_public')
        )
      )
      and plane = 'product'
      and app_version is not null
      and app_version !~ '^0\.0\.0'
      and coalesce(provider_id, '') <> 'fake'
      and coalesce(properties ->> 'smoke_run_id', '') = ''
      and (
        event_name <> 'cli_invocation'
        or (
          origin_runtime = 'cli'
          and broker_runtime = 'cli'
          and surface = 'cli'
        )
      ) as is_eligible_product_telemetry
  from classified
)
select
  event_id,
  occurred_at,
  received_at,
  ingested_at,
  event_name,
  event_version,
  schema_version,
  plane,
  analytics_environment,
  traffic_class,
  stored_activity_class,
  client_profile_id_hash,
  data_root_id_hash,
  identity_key_version,
  legacy_origin_install_id_hash,
  legacy_origin_device_id_hash,
  surface,
  origin_runtime,
  broker_runtime,
  app_version,
  os,
  arch,
  provider_id,
  duration_bucket,
  status,
  success,
  properties,
  operation,
  outcome,
  activity_class,
  is_eligible_product_telemetry,
  is_eligible_product_telemetry
    and event_name in ('cli_invocation', 'operation_completed')
    and activity_class in ('product_activity', 'product_value') as is_real_activity,
  is_eligible_product_telemetry
    and event_name in ('cli_invocation', 'operation_completed')
    and activity_class = 'product_value'
    and outcome = 'success' as is_value_activity,
  raw_traffic_class,
  traffic_class_override_key,
  provider_trigger,
  provider_source_mode,
  provider_change,
  provider_content_evidence,
  provider_work_kind,
  provider_refresh_result,
  provider_core_result,
  provider_canonical_pro_result,
  provider_output_pro_result,
  provider_failure_scope,
  provider_failure_type,
  provider_outcome,
  provider_work_remaining,
  provider_sources_bucket,
  provider_source_files_bucket,
  provider_sessions_bucket,
  provider_events_bucket,
  provider_edges_bucket,
  provider_skips_bucket,
  provider_rejections_bucket,
  provider_failures_bucket,
  provider_retired_records_bucket,
  provider_bytes_bucket
from eligible;
alter view ctx.analytics_canonical_telemetry_events owner to ctx_migration;
revoke all on ctx.analytics_canonical_telemetry_events from public, ctx_analytics_readonly;
create index telemetry_event_env_ts_idx
  on ctx.telemetry_event (analytics_environment, occurred_at desc);
create index telemetry_event_event_name_ts_idx
  on ctx.telemetry_event (event_name, occurred_at desc);
`;
