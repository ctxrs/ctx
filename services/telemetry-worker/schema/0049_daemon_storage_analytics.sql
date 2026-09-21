begin;

set local lock_timeout = '5s';
set local statement_timeout = '2min';
set local idle_in_transaction_session_timeout = '2min';

select pg_advisory_xact_lock(
  hashtextextended('ctx.neon.migration.0049.daemon_storage_analytics', 0)
);

do $precondition$
begin
  if current_user <> 'ctx_migration'
    or to_regclass('ctx.telemetry_event_effective') is null
    or not exists (
      select 1
      from pg_roles
      where rolname = 'ctx_analytics_readonly'
    )
  then
    raise exception
      'daemon storage analytics requires ctx_migration, ctx_analytics_readonly, and the effective telemetry event authority';
  end if;
end
$precondition$;

-- This is deliberately a long, one-dimensional surface. It never exposes the
-- hidden profile/root grain used to choose one latest daemon observation per
-- received UTC day, and it never publishes a cross-tab of storage dimensions.
create or replace view ctx.analytics_daemon_storage_daily
with (security_barrier = true, security_invoker = false) as
with dimension_vocabulary as (
  select vocabulary.dimension, bucket.bucket
  from (
    values
      (
        'filesystem_total_bytes_bucket'::text,
        array[
          '0', 'lt_100mb', '100mb-1gb', '1gb-5gb', '5gb-10gb',
          '10gb-25gb', '25gb-50gb', '50gb-100gb', '100gb-250gb',
          '250gb-500gb', '500gb-1tb', '1tb-2tb', '2tb-5tb', '5tb+'
        ]::text[]
      ),
      (
        'filesystem_available_bytes_bucket',
        array[
          '0', 'lt_100mb', '100mb-1gb', '1gb-5gb', '5gb-10gb',
          '10gb-25gb', '25gb-50gb', '50gb-100gb', '100gb-250gb',
          '250gb-500gb', '500gb-1tb', '1tb-2tb', '2tb-5tb', '5tb+'
        ]::text[]
      ),
      (
        'filesystem_available_fraction_bucket',
        array[
          '0', 'lt_5pct', '5pct-10pct', '10pct-20pct', '20pct-40pct',
          '40pct-60pct', '60pct+'
        ]::text[]
      ),
      (
        'core_active_logical_bytes_bucket',
        array[
          '0', 'lt_100mb', '100mb-1gb', '1gb-5gb', '5gb-10gb',
          '10gb-25gb', '25gb-50gb', '50gb-100gb', '100gb-250gb',
          '250gb-500gb', '500gb-1tb', '1tb-2tb', '2tb-5tb', '5tb+'
        ]::text[]
      ),
      (
        'core_certified_source_bytes_bucket',
        array[
          '0', 'lt_100mb', '100mb-1gb', '1gb-5gb', '5gb-10gb',
          '10gb-25gb', '25gb-50gb', '50gb-100gb', '100gb-250gb',
          '250gb-500gb', '500gb-1tb', '1tb-2tb', '2tb-5tb', '5tb+'
        ]::text[]
      ),
      (
        'core_logical_amplification_bucket',
        array[
          'lt_0_10x', '0_10x-0_25x', '0_25x-0_35x', '0_35x-0_50x',
          '0_50x-1x', '1x-2x', '2x+'
        ]::text[]
      ),
      (
        'filesystem_available_to_active_core_ratio_bucket',
        array[
          'lt_0_5x', '0_5x-1x', '1x-1_25x', '1_25x-2x', '2x-4x', '4x+'
        ]::text[]
      )
  ) as vocabulary(dimension, buckets)
  cross join lateral unnest(vocabulary.buckets) as bucket(bucket)
), latest_observation as (
  select distinct on (
    (event.received_at at time zone 'utc')::date,
    event.identity_key_version,
    event.client_profile_id_hash,
    event.data_root_id_hash
  )
    (event.received_at at time zone 'utc')::date as received_date,
    event.properties
  from ctx.telemetry_event_effective as event
  where event.schema_version = 1
    and event.event_version = 1
    and event.plane = 'product'
    and event.analytics_environment = 'production'
    and event.traffic_class = 'unclassified_public'
    and event.event_name = 'runtime_observation'
    and event.surface = 'daemon'
    and event.properties ->> 'operation' in ('ready', 'liveness')
    and event.received_at is not null
    and event.received_at <
      date_trunc('day', statement_timestamp() at time zone 'utc')
        at time zone 'utc'
    and event.identity_key_version is not null
    and event.client_profile_id_hash is not null
    and event.data_root_id_hash is not null
  order by
    (event.received_at at time zone 'utc')::date,
    event.identity_key_version,
    event.client_profile_id_hash,
    event.data_root_id_hash,
    event.received_at desc,
    event.event_id desc
), eligible as (
  select received_date, count(*)::bigint as eligible_profile_data_root_count
  from latest_observation
  group by received_date
), measurement as (
  select
    observation.received_date,
    value.dimension,
    value.bucket
  from latest_observation as observation
  cross join lateral (
    values
      ('filesystem_total_bytes_bucket', observation.properties ->> 'filesystem_total_bytes_bucket'),
      ('filesystem_available_bytes_bucket', observation.properties ->> 'filesystem_available_bytes_bucket'),
      ('filesystem_available_fraction_bucket', observation.properties ->> 'filesystem_available_fraction_bucket'),
      ('core_active_logical_bytes_bucket', observation.properties ->> 'core_active_logical_bytes_bucket'),
      ('core_certified_source_bytes_bucket', observation.properties ->> 'core_certified_source_bytes_bucket'),
      ('core_logical_amplification_bucket', observation.properties ->> 'core_logical_amplification_bucket'),
      ('filesystem_available_to_active_core_ratio_bucket', observation.properties ->> 'filesystem_available_to_active_core_ratio_bucket')
  ) as value(dimension, bucket)
  where value.bucket is not null
), measured as (
  select
    received_date,
    dimension,
    count(*)::bigint as measured_profile_data_root_count
  from measurement
  group by received_date, dimension
), histogram as (
  select
    received_date,
    dimension,
    bucket,
    count(*)::bigint as bucket_profile_data_root_count
  from measurement
  group by received_date, dimension, bucket
)
select
  eligible.received_date,
  'analytics_enabled_daemon_observed_profile_data_roots'::text as population,
  vocabulary.dimension,
  vocabulary.bucket,
  eligible.eligible_profile_data_root_count,
  coalesce(measured.measured_profile_data_root_count, 0)::bigint
    as measured_profile_data_root_count,
  coalesce(histogram.bucket_profile_data_root_count, 0)::bigint
    as bucket_profile_data_root_count
from eligible
cross join dimension_vocabulary as vocabulary
left join measured
  on measured.received_date = eligible.received_date
  and measured.dimension = vocabulary.dimension
left join histogram
  on histogram.received_date = eligible.received_date
  and histogram.dimension = vocabulary.dimension
  and histogram.bucket = vocabulary.bucket;

alter view ctx.analytics_daemon_storage_daily owner to ctx_migration;
comment on view ctx.analytics_daemon_storage_daily is
  'One-dimensional daily storage bucket histograms over analytics-enabled production daemon-observed profile/data-root grains; this population is not humans or all installations, and the view exposes no identifiers or storage cross-tabs.';
revoke all privileges on table ctx.analytics_daemon_storage_daily
  from public, ctx_control_plane, ctx_telemetry_ingest,
    ctx_telemetry_retention, ctx_product_health_readonly,
    ctx_analytics_readonly;
grant select on table ctx.analytics_daemon_storage_daily
  to ctx_migration, ctx_analytics_readonly;

commit;
