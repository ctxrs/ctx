begin;

set local lock_timeout = '5s';
set local statement_timeout = '2min';
set local idle_in_transaction_session_timeout = '2min';

-- Migration 0038 created five unnamed table-level checks. Keep the object
-- contract strict while replacing only the four V1-specific checks.
alter table ctx.blame_product_event
  drop constraint if exists blame_product_event_properties_check1,
  drop constraint if exists blame_product_event_properties_check2,
  drop constraint if exists blame_product_event_check,
  drop constraint if exists blame_product_event_check1,
  drop constraint if exists blame_product_event_check2,
  drop constraint if exists blame_product_event_check3,
  drop constraint if exists blame_product_event_check4,
  drop constraint if exists blame_product_event_properties_keys_v1_v2_check,
  drop constraint if exists blame_product_event_properties_required_v1_v2_check,
  drop constraint if exists blame_product_event_properties_common_v1_v2_check,
  drop constraint if exists blame_product_event_properties_terminal_v1_v2_check;

alter table ctx.blame_product_event
  add constraint blame_product_event_properties_keys_v1_v2_check check ((
    (
      properties -> 'blame_schema_version' = '1'::jsonb
      and (properties - array[
        'operation', 'outcome', 'blame_schema_version',
        'blame_semantics_version', 'blame_surface', 'blame_target_kind',
        'blame_request_kind', 'blame_access_state', 'blame_result_state',
        'blame_failure_class', 'blame_freshness', 'blame_has_more',
        'blame_output_served', 'blame_pro_version',
        'blame_pro_protocol_version'
      ]) = '{}'::jsonb
    ) or (
      properties -> 'blame_schema_version' = '2'::jsonb
      and (properties - array[
        'operation', 'outcome', 'blame_schema_version', 'blame_surface',
        'blame_target_kind', 'blame_request_kind',
        'blame_query_duration_bucket',
        'blame_result_state', 'blame_result_count_bucket', 'blame_failure_class',
        'blame_failure_phase', 'blame_freshness', 'blame_has_more'
      ]) = '{}'::jsonb
    )
  ) is true),
  add constraint blame_product_event_properties_required_v1_v2_check
  check ((
    properties ?& array[
      'operation', 'outcome', 'blame_schema_version', 'blame_surface',
      'blame_target_kind', 'blame_request_kind'
    ]
    and (
      properties -> 'blame_schema_version' <> '1'::jsonb
      or properties ?& array[
        'blame_semantics_version', 'blame_output_served'
      ]
    )
  ) is true),
  add constraint blame_product_event_properties_common_v1_v2_check check ((
    jsonb_typeof(properties -> 'operation') = 'string'
    and properties ->> 'operation' = 'blame'
    and jsonb_typeof(properties -> 'outcome') = 'string'
    and properties ->> 'outcome' = outcome
    and jsonb_typeof(properties -> 'blame_schema_version') = 'number'
    and jsonb_typeof(properties -> 'blame_surface') = 'string'
    and jsonb_typeof(properties -> 'blame_target_kind') = 'string'
    and properties ->> 'blame_target_kind' in (
      'file', 'commit', 'pull_request'
    )
    and jsonb_typeof(properties -> 'blame_request_kind') = 'string'
    and properties ->> 'blame_request_kind' in (
      'first_request', 'continuation'
    )
    and (
      (
        properties -> 'blame_schema_version' = '1'::jsonb
        and jsonb_typeof(properties -> 'blame_semantics_version') = 'number'
        and properties -> 'blame_semantics_version' = '1'::jsonb
        and properties ->> 'blame_surface' = 'cli'
        and (
          not (properties ? 'blame_access_state')
          or (
            jsonb_typeof(properties -> 'blame_access_state') = 'string'
            and properties ->> 'blame_access_state' in (
              'trial', 'active', 'canceling_paid', 'offline_grace',
              'locked', 'unavailable'
            )
          )
        )
        and (
          not (properties ? 'blame_pro_version')
          or (
            jsonb_typeof(properties -> 'blame_pro_version') = 'string'
            and length(properties ->> 'blame_pro_version') between 1 and 64
            and properties ->> 'blame_pro_version' ~ '^(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})(-((0|[1-9][0-9]*)|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)(\.((0|[1-9][0-9]*)|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'
          )
        )
        and (
          not (properties ? 'blame_pro_protocol_version')
          or (
            jsonb_typeof(properties -> 'blame_pro_protocol_version') = 'number'
            and (properties ->> 'blame_pro_protocol_version')::integer
              between 1 and 65535
          )
        )
      ) or (
        properties -> 'blame_schema_version' = '2'::jsonb
        and properties ->> 'blame_surface' in ('cli', 'mcp')
        and (
          not (properties ? 'blame_query_duration_bucket')
          or (
            jsonb_typeof(properties -> 'blame_query_duration_bucket') = 'string'
            and properties ->> 'blame_query_duration_bucket' in (
              'lt_100ms', 'lt_1s', 'lt_5s', 'lt_30s', 'lt_2m',
              'lt_10m', 'lt_1h', 'gte_1h'
            )
          )
        )
      )
    )
  ) is true),
  add constraint blame_product_event_properties_terminal_v1_v2_check
  check ((
    (
      properties -> 'blame_schema_version' = '1'::jsonb
      and (
        (
          outcome = 'success'
          and activity_class = 'product_value'
          and properties ->> 'blame_output_served' = 'true'
          and jsonb_typeof(properties -> 'blame_output_served') = 'boolean'
          and jsonb_typeof(properties -> 'blame_result_state') = 'string'
          and properties ->> 'blame_result_state' in (
            'proven', 'possible', 'conflicting', 'none'
          )
          and jsonb_typeof(properties -> 'blame_freshness') = 'string'
          and properties ->> 'blame_freshness' in (
            'current', 'stale_committed'
          )
          and jsonb_typeof(properties -> 'blame_has_more') = 'boolean'
          and not (properties ? 'blame_failure_class')
        ) or (
          outcome = 'failure'
          and activity_class = 'product_activity'
          and properties ->> 'blame_output_served' = 'false'
          and jsonb_typeof(properties -> 'blame_output_served') = 'boolean'
          and jsonb_typeof(properties -> 'blame_failure_class') = 'string'
          and properties ->> 'blame_failure_class' in (
            'commercial', 'installation', 'authorization', 'key_store',
            'protocol', 'source', 'repository', 'stale', 'ambiguous',
            'invalid_request', 'invalid_response', 'cancelled',
            'helper_crashed', 'helper_timeout', 'output', 'other'
          )
          and not (properties ? 'blame_result_state')
          and not (properties ? 'blame_freshness')
          and not (properties ? 'blame_has_more')
        )
      )
    ) or (
      properties -> 'blame_schema_version' = '2'::jsonb
      and (
        (
          outcome = 'success'
          and activity_class = 'product_value'
          and jsonb_typeof(
            properties -> 'blame_query_duration_bucket'
          ) = 'string'
          and jsonb_typeof(properties -> 'blame_result_state') = 'string'
          and properties ->> 'blame_result_state' in (
            'proven', 'possible', 'conflicting', 'none'
          )
          and jsonb_typeof(
            properties -> 'blame_result_count_bucket'
          ) = 'string'
          and properties ->> 'blame_result_count_bucket' in (
            '0', '1', '2-5', '6-20', '21-100', '101-1k',
            '1k-10k', '10k-100k', '100k-1m', '1m+'
          )
          and jsonb_typeof(properties -> 'blame_freshness') = 'string'
          and properties ->> 'blame_freshness' in (
            'current', 'stale_committed'
          )
          and jsonb_typeof(properties -> 'blame_has_more') = 'boolean'
          and not (properties ? 'blame_failure_class')
          and not (properties ? 'blame_failure_phase')
        ) or (
          outcome = 'failure'
          and activity_class = 'product_activity'
          and jsonb_typeof(properties -> 'blame_failure_class') = 'string'
          and properties ->> 'blame_failure_class' in (
            'commercial', 'installation', 'authorization', 'key_store',
            'protocol', 'source', 'repository', 'stale', 'ambiguous',
            'invalid_request', 'invalid_response', 'cancelled',
            'helper_crashed', 'helper_timeout', 'other'
          )
          and jsonb_typeof(properties -> 'blame_failure_phase') = 'string'
          and properties ->> 'blame_failure_phase' in (
            'setup', 'query', 'presentation'
          )
          and (
            (
              properties ->> 'blame_failure_phase' = 'setup'
              and not (properties ? 'blame_query_duration_bucket')
            ) or (
              properties ->> 'blame_failure_phase' in (
                'query', 'presentation'
              )
              and jsonb_typeof(
                properties -> 'blame_query_duration_bucket'
              ) = 'string'
            )
          )
          and not (properties ? 'blame_result_state')
          and not (properties ? 'blame_result_count_bucket')
          and not (properties ? 'blame_freshness')
          and not (properties ? 'blame_has_more')
        )
      )
    )
  ) is true);

create index if not exists blame_product_event_received_at_idx
  on ctx.blame_product_event (received_at);

-- Replace the thresholded V1 publication with one compact owner-rights
-- aggregate. A successful follow-up is derived by filtering the published
-- outcome and request-kind dimensions; protected subjects appear only in a
-- distinct aggregate.
drop view if exists ctx_product_health.blame_summary;
drop view if exists ctx_product_health.blame_weekly;

create view ctx_product_health.blame_summary
with (security_barrier = true, security_invoker = false) as
with bounded_event as (
  select
    date_trunc('day', event.received_at at time zone 'utc')::date
      as received_day,
    event.properties ->> 'blame_surface' as blame_surface,
    event.properties ->> 'blame_target_kind' as blame_target_kind,
    event.properties ->> 'blame_request_kind' as request_kind,
    event.app_version,
    event.os,
    event.arch,
    event.duration_bucket as total_duration_bucket,
    event.properties ->> 'blame_query_duration_bucket'
      as query_duration_bucket,
    event.outcome,
    event.properties ->> 'blame_result_state' as result_state,
    event.properties ->> 'blame_result_count_bucket'
      as result_count_bucket,
    event.properties ->> 'blame_freshness' as freshness,
    case when event.properties ? 'blame_has_more'
      then (event.properties ->> 'blame_has_more')::boolean
    end as has_more,
    event.properties ->> 'blame_failure_class' as failure_class,
    event.properties ->> 'blame_failure_phase' as failure_phase,
    event.subject_hash as protected_subject_hash
  from ctx.blame_product_event as event
  where event.analytics_environment = 'production'
    and event.traffic_class = 'unclassified_public'
    and event.properties -> 'blame_schema_version' = '2'::jsonb
    and event.received_at >= statement_timestamp() - interval '180 days'
)
select
  received_day,
  blame_surface,
  blame_target_kind,
  request_kind,
  app_version,
  os,
  arch,
  total_duration_bucket,
  query_duration_bucket,
  outcome,
  result_state,
  result_count_bucket,
  freshness,
  has_more,
  failure_class,
  failure_phase,
  count(*)::bigint as attempt_count,
  count(distinct protected_subject_hash)::bigint
    as observed_installation_count
from bounded_event
group by
  received_day, blame_surface, blame_target_kind, request_kind, app_version,
  os, arch, total_duration_bucket, query_duration_bucket,
  outcome, result_state, result_count_bucket, freshness, has_more,
  failure_class, failure_phase;

alter view ctx_product_health.blame_summary owner to ctx_migration;
comment on view ctx_product_health.blame_summary is
  'Owner-only daily aggregate of retained production Blame V2 receipts; protected subjects contribute only to a distinct installation count.';

revoke all privileges on table ctx.blame_product_event
  from public, ctx_analytics_readonly, ctx_telemetry_ingest,
    ctx_control_plane, ctx_telemetry_retention,
    ctx_product_health_readonly;
revoke all on function ctx.record_blame_product_receipt(jsonb)
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_retention, ctx_product_health_readonly;
grant execute on function ctx.record_blame_product_receipt(jsonb)
  to ctx_migration, ctx_telemetry_ingest;
grant usage on schema ctx to ctx_telemetry_retention;

revoke all on schema ctx_product_health
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_ingest, ctx_telemetry_retention,
    ctx_product_health_readonly;
grant usage on schema ctx_product_health to ctx_product_health_readonly;
revoke all on schema ctx from ctx_product_health_readonly;
revoke all privileges on table ctx_product_health.blame_summary
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_ingest, ctx_telemetry_retention,
    ctx_product_health_readonly;
grant select on table ctx_product_health.blame_summary
  to ctx_product_health_readonly;

commit;
