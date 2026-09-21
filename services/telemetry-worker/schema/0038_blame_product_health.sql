begin;

set local lock_timeout = '5s';
set local statement_timeout = '2min';
set local idle_in_transaction_session_timeout = '2min';

-- Neon provisions this least-privilege product reader before migration. The
-- application migration role deliberately cannot create or alter login roles.
-- neon-role: ctx_product_health_readonly
do $reader_preflight$
begin
  if not exists (
    select 1
    from pg_roles as role
    where role.rolname = 'ctx_product_health_readonly'
      and role.rolcanlogin
      and not role.rolinherit
      and not role.rolsuper
      and not role.rolcreaterole
      and not role.rolcreatedb
      and not role.rolreplication
      and not role.rolbypassrls
      and not exists (
        select 1
        from pg_auth_members as membership
        where membership.member = role.oid
      )
      and (
        select count(*) = 1
          and bool_and(
            member_role.rolname = 'neondb_owner'
            and grantor_role.rolname = 'cloud_admin'
            and membership.admin_option
            and not membership.inherit_option
            and not membership.set_option
          )
        from pg_auth_members as membership
        join pg_roles as member_role on member_role.oid = membership.member
        join pg_roles as grantor_role on grantor_role.oid = membership.grantor
        where membership.roleid = role.oid
      )
  ) or has_database_privilege(
    'ctx_product_health_readonly', current_database(), 'create'
  ) then
    raise exception
      'least-privilege ctx_product_health_readonly LOGIN NOINHERIT must be provisioned by the Neon operator before migration 0038';
  end if;
end
$reader_preflight$;

create schema if not exists ctx_product_health authorization ctx_migration;
alter schema ctx_product_health owner to ctx_migration;
comment on schema ctx_product_health is
  'Owner-rights boundary for explicitly granted settled product-health views.';
revoke all on schema ctx_product_health from public, ctx_analytics_readonly,
  ctx_control_plane, ctx_telemetry_ingest, ctx_telemetry_retention,
  ctx_product_health_readonly;
grant usage on schema ctx_product_health to ctx_product_health_readonly;
revoke all on schema ctx from ctx_product_health_readonly;

-- Current Blame is deliberately isolated from ctx.telemetry_event. One row is
-- both the restricted terminal receipt and its proof-derived subject fact.
create table if not exists ctx.blame_product_event (
  event_id text primary key
    check ((event_id ~ '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$') is true),
  replay_fingerprint text not null
    check ((replay_fingerprint ~ '^[0-9a-f]{64}$') is true),
  received_at timestamptz not null,
  occurred_at timestamptz not null,
  analytics_environment text not null
    check ((analytics_environment in ('production', 'staging')) is true),
  traffic_class text not null
    check ((traffic_class = case analytics_environment
      when 'production' then 'unclassified_public'
      else 'synthetic'
    end) is true),
  activity_class text not null
    check ((activity_class in ('product_activity', 'product_value')) is true),
  app_version text not null
    check ((length(app_version) between 1 and 64
      and app_version ~ '^(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})\.(0|[1-9][0-9]{0,4})(-((0|[1-9][0-9]*)|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)(\.((0|[1-9][0-9]*)|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$') is true),
  os text not null
    check ((os in ('linux', 'macos', 'windows', 'freebsd')) is true),
  arch text not null
    check ((arch in ('x86_64', 'aarch64', 'x86', 'arm')) is true),
  duration_bucket text not null
    check ((duration_bucket in (
      'unknown', 'lt_100ms', 'lt_1s', 'lt_5s', 'lt_30s', 'lt_2m',
      'lt_10m', 'lt_1h', 'gte_1h'
    )) is true),
  outcome text not null
    check ((outcome in ('success', 'failure')) is true),
  properties jsonb not null,
  identity_key_version integer not null
    check ((identity_key_version > 0) is true),
  subject_hash text not null
    check ((subject_hash ~ '^[0-9a-f]{64}$') is true),
  check ((jsonb_typeof(properties) = 'object') is true),
  check (((properties - array[
    'operation', 'outcome', 'blame_schema_version',
    'blame_semantics_version', 'blame_surface', 'blame_target_kind',
    'blame_request_kind', 'blame_access_state', 'blame_result_state',
    'blame_failure_class', 'blame_freshness', 'blame_has_more',
    'blame_output_served', 'blame_pro_version',
    'blame_pro_protocol_version'
  ]) = '{}'::jsonb) is true),
  check ((properties ?& array[
    'operation', 'outcome', 'blame_schema_version',
    'blame_semantics_version', 'blame_surface', 'blame_target_kind',
    'blame_request_kind', 'blame_output_served'
  ]) is true),
  check ((jsonb_typeof(properties -> 'operation') = 'string'
    and properties ->> 'operation' = 'blame'
    and jsonb_typeof(properties -> 'outcome') = 'string'
    and properties ->> 'outcome' = outcome
    and jsonb_typeof(properties -> 'blame_schema_version') = 'number'
    and properties -> 'blame_schema_version' = '1'::jsonb
    and jsonb_typeof(properties -> 'blame_semantics_version') = 'number'
    and properties -> 'blame_semantics_version' = '1'::jsonb
    and jsonb_typeof(properties -> 'blame_surface') = 'string'
    and properties ->> 'blame_surface' = 'cli'
    and jsonb_typeof(properties -> 'blame_target_kind') = 'string'
    and properties ->> 'blame_target_kind' in (
      'file', 'commit', 'pull_request'
    )
    and jsonb_typeof(properties -> 'blame_request_kind') = 'string'
    and properties ->> 'blame_request_kind' in (
      'first_request', 'continuation'
    )
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
  ) is true),
  check (((
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
  )) is true)
);

alter table ctx.blame_product_event owner to ctx_migration;
comment on table ctx.blame_product_event is
  'Restricted proof-backed CLI Blame terminal receipts; isolated from generic telemetry and retained for 180 days.';
revoke all privileges on table ctx.blame_product_event
  from public, ctx_analytics_readonly, ctx_telemetry_ingest,
    ctx_control_plane, ctx_telemetry_retention,
    ctx_product_health_readonly;

-- The Worker passes one already strictly admitted receipt. This routine owns
-- replay idempotence and rejects any changed event, proof subject, or key epoch.
create or replace function ctx.record_blame_product_receipt(p_receipt jsonb)
returns void
language plpgsql
volatile
security definer
set search_path = pg_catalog, ctx
as $record$
declare
  v_event_id text;
  v_replay_fingerprint text;
  v_received_at timestamptz;
  v_occurred_at timestamptz;
  v_analytics_environment text;
  v_traffic_class text;
  v_activity_class text;
  v_app_version text;
  v_os text;
  v_arch text;
  v_duration_bucket text;
  v_outcome text;
  v_properties jsonb;
  v_identity_key_version integer;
  v_subject_hash text;
begin
  if jsonb_typeof(p_receipt) <> 'object'
    or not (p_receipt ?& array[
      'event_id', 'replay_fingerprint', 'received_at', 'occurred_at',
      'analytics_environment', 'traffic_class', 'activity_class',
      'app_version', 'os', 'arch', 'duration_bucket', 'outcome',
      'properties', 'identity_key_version', 'subject_hash'
    ])
    or (p_receipt - array[
      'event_id', 'replay_fingerprint', 'received_at', 'occurred_at',
      'analytics_environment', 'traffic_class', 'activity_class',
      'app_version', 'os', 'arch', 'duration_bucket', 'outcome',
      'properties', 'identity_key_version', 'subject_hash'
    ]) <> '{}'::jsonb
    or jsonb_typeof(p_receipt -> 'event_id') <> 'string'
    or jsonb_typeof(p_receipt -> 'replay_fingerprint') <> 'string'
    or jsonb_typeof(p_receipt -> 'received_at') <> 'string'
    or jsonb_typeof(p_receipt -> 'occurred_at') <> 'string'
    or jsonb_typeof(p_receipt -> 'analytics_environment') <> 'string'
    or jsonb_typeof(p_receipt -> 'traffic_class') <> 'string'
    or jsonb_typeof(p_receipt -> 'activity_class') <> 'string'
    or jsonb_typeof(p_receipt -> 'app_version') <> 'string'
    or jsonb_typeof(p_receipt -> 'os') <> 'string'
    or jsonb_typeof(p_receipt -> 'arch') <> 'string'
    or jsonb_typeof(p_receipt -> 'duration_bucket') <> 'string'
    or jsonb_typeof(p_receipt -> 'outcome') <> 'string'
    or jsonb_typeof(p_receipt -> 'properties') <> 'object'
    or jsonb_typeof(p_receipt -> 'identity_key_version') <> 'number'
    or jsonb_typeof(p_receipt -> 'subject_hash') <> 'string'
  then
    raise exception using
      errcode = '22023',
      message = 'invalid isolated Blame product receipt';
  end if;

  begin
    v_event_id := p_receipt ->> 'event_id';
    v_replay_fingerprint := p_receipt ->> 'replay_fingerprint';
    v_received_at := (p_receipt ->> 'received_at')::timestamptz;
    v_occurred_at := (p_receipt ->> 'occurred_at')::timestamptz;
    v_analytics_environment := p_receipt ->> 'analytics_environment';
    v_traffic_class := p_receipt ->> 'traffic_class';
    v_activity_class := p_receipt ->> 'activity_class';
    v_app_version := p_receipt ->> 'app_version';
    v_os := p_receipt ->> 'os';
    v_arch := p_receipt ->> 'arch';
    v_duration_bucket := p_receipt ->> 'duration_bucket';
    v_outcome := p_receipt ->> 'outcome';
    v_properties := p_receipt -> 'properties';
    v_identity_key_version :=
      (p_receipt ->> 'identity_key_version')::integer;
    v_subject_hash := p_receipt ->> 'subject_hash';

    insert into ctx.blame_product_event (
      event_id, replay_fingerprint, received_at, occurred_at,
      analytics_environment, traffic_class, activity_class, app_version,
      os, arch, duration_bucket, outcome, properties,
      identity_key_version, subject_hash
    ) values (
      v_event_id, v_replay_fingerprint, v_received_at, v_occurred_at,
      v_analytics_environment, v_traffic_class, v_activity_class,
      v_app_version, v_os, v_arch, v_duration_bucket, v_outcome,
      v_properties, v_identity_key_version, v_subject_hash
    )
    on conflict (event_id) do nothing;
  exception
    when check_violation or invalid_text_representation
      or datetime_field_overflow or numeric_value_out_of_range then
      raise exception using
        errcode = '22023',
        message = 'invalid isolated Blame product receipt';
  end;

  if not exists (
    select 1
    from ctx.blame_product_event as stored
    where stored.event_id = v_event_id
      and stored.replay_fingerprint = v_replay_fingerprint
      and stored.occurred_at = v_occurred_at
      and stored.analytics_environment = v_analytics_environment
      and stored.traffic_class = v_traffic_class
      and stored.activity_class = v_activity_class
      and stored.app_version = v_app_version
      and stored.os = v_os
      and stored.arch = v_arch
      and stored.duration_bucket = v_duration_bucket
      and stored.outcome = v_outcome
      and stored.properties = v_properties
      and stored.identity_key_version = v_identity_key_version
      and stored.subject_hash = v_subject_hash
  ) then
    raise exception using
      errcode = '22012',
      message = 'isolated Blame product receipt collision';
  end if;
end
$record$;

alter function ctx.record_blame_product_receipt(jsonb) owner to ctx_migration;
revoke all on function ctx.record_blame_product_receipt(jsonb)
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_retention, ctx_product_health_readonly;
grant execute on function ctx.record_blame_product_receipt(jsonb)
  to ctx_migration, ctx_telemetry_ingest;

-- Extend the existing purpose-scoped daily retention routine to the isolated
-- receipt table. The retention login still receives no direct table privilege.
create or replace function ctx.delete_expired_raw_product_telemetry(
  p_before timestamptz default clock_timestamp() - interval '180 days'
)
returns bigint
language plpgsql
volatile
security definer
set search_path = pg_catalog, ctx
as $retention$
declare
  deleted_telemetry_count bigint := 0;
  deleted_install_attempt_count bigint := 0;
  deleted_blame_count bigint := 0;
begin
  if p_before > clock_timestamp() - interval '180 days' then
    raise exception 'product telemetry retention cutoff must be at least 180 days old';
  end if;

  delete from ctx.telemetry_event
  where plane = 'product'
    and event_name in (
      'cli_invocation', 'operation_completed',
      'provider_refresh_completed', 'runtime_observation'
    )
    and coalesce(received_at, occurred_at) < p_before;
  get diagnostics deleted_telemetry_count = row_count;

  delete from ctx.install_attempt_event
  where (
      schema_version is null
      or (
        schema_version = 1
        and event_name = 'install_stage'
        and event_version = 1
      )
    )
    and coalesce(received_at, occurred_at) < p_before;
  get diagnostics deleted_install_attempt_count = row_count;

  delete from ctx.blame_product_event
  where received_at < p_before;
  get diagnostics deleted_blame_count = row_count;

  return deleted_telemetry_count
    + deleted_install_attempt_count
    + deleted_blame_count;
end
$retention$;

alter function ctx.delete_expired_raw_product_telemetry(timestamptz)
  owner to ctx_migration;
comment on function ctx.delete_expired_raw_product_telemetry(timestamptz) is
  'Deletes raw product telemetry, install-attempt rows, and isolated Blame receipts older than the 180-day received boundary.';
revoke all on function ctx.delete_expired_raw_product_telemetry(timestamptz)
  from public, ctx_product_health_readonly;
grant execute on function ctx.delete_expired_raw_product_telemetry(timestamptz)
  to ctx_migration, ctx_telemetry_retention;

-- Option B: exactly 20 fully settled received weeks. One statement-stable
-- anchor drives both the fixed grid and the immutable event window.
create or replace view ctx_product_health.blame_weekly
with (security_barrier = true, security_invoker = false) as
with anchor as materialized (
  select
    (current_week_start_utc at time zone 'utc')::date as current_week,
    current_week_start_utc
  from (
    select date_trunc('week', statement_timestamp() at time zone 'utc')
      at time zone 'utc' as current_week_start_utc
  ) as statement_anchor
), fixed_grid as (
  select
    (anchor.current_week - offset_number * interval '1 week')::date
      as received_week
  from anchor
  cross join generate_series(2, 21) as offset_number
), proof_event as (
  select
    date_trunc('week', event.received_at at time zone 'utc')::date
      as received_week,
    event.identity_key_version,
    event.subject_hash,
    ((event.properties ->> 'blame_access_state' in (
      'active', 'canceling_paid', 'offline_grace'
    )) is true) as paid,
    ((event.outcome = 'success'
      and event.activity_class = 'product_value'
      and event.properties ->> 'blame_result_state' in (
        'proven', 'possible', 'conflicting', 'none'
      )
      and event.properties ->> 'blame_freshness' in (
        'current', 'stale_committed'
      )
      and jsonb_typeof(event.properties -> 'blame_has_more') = 'boolean'
      and event.properties ->> 'blame_output_served' = 'true'
    ) is true) as product_value
  from anchor
  join ctx.blame_product_event as event
    on event.received_at >=
      anchor.current_week_start_utc - interval '21 weeks'
    and event.received_at <
      anchor.current_week_start_utc - interval '1 week'
  where event.analytics_environment = 'production'
    and event.traffic_class = 'unclassified_public'
), epoch_support as (
  select
    received_week,
    identity_key_version,
    count(*)::bigint as attempt_count,
    count(distinct subject_hash)::bigint as subject_count
  from proof_event
  group by received_week, identity_key_version
), epoch_subject as (
  select
    received_week,
    identity_key_version,
    subject_hash,
    coalesce(bool_or(paid), false) as paid,
    coalesce(bool_or(product_value), false) as product_value
  from proof_event
  group by received_week, identity_key_version, subject_hash
), epoch_metric as (
  select
    subject.received_week,
    subject.identity_key_version,
    support.attempt_count,
    support.subject_count,
    count(*) filter (where subject.paid)::bigint as paid_subjects,
    count(*) filter (where not subject.paid)::bigint as unpaid_subjects,
    count(*) filter (
      where subject.paid and subject.product_value
    )::bigint as paid_value_subjects,
    count(*) filter (
      where subject.paid and not subject.product_value
    )::bigint as paid_without_value_subjects
  from epoch_subject as subject
  join epoch_support as support using (
    received_week, identity_key_version
  )
  group by
    subject.received_week,
    subject.identity_key_version,
    support.attempt_count,
    support.subject_count
), combined as (
  select
    received_week,
    bool_and(
      attempt_count >= 100
      and subject_count >= 20
      and paid_subjects >= 20
      and unpaid_subjects >= 20
    ) as paid_volume_supported,
    bool_and(
      attempt_count >= 100
      and subject_count >= 20
      and paid_value_subjects >= 20
      and paid_without_value_subjects >= 20
    ) as paid_value_supported,
    sum(paid_subjects)::bigint as paid_subjects,
    sum(paid_value_subjects)::bigint as paid_value_subjects,
    sum(paid_without_value_subjects)::bigint
      as paid_without_value_subjects
  from epoch_metric
  group by received_week
)
select
  grid.received_week,
  'cli'::text as blame_surface,
  case
    when combined.paid_volume_supported is not true then 'sparse'
    when combined.paid_subjects < 100 then '20-99'
    when combined.paid_subjects < 500 then '100-499'
    when combined.paid_subjects < 2000 then '500-1999'
    else '2k+'
  end as paid_proof_subject_volume_band,
  case
    when combined.paid_value_supported is not true then 'sparse'
    when combined.paid_value_subjects::numeric
      / (
        combined.paid_value_subjects
        + combined.paid_without_value_subjects
      )::numeric < 0.25 then 'low'
    when combined.paid_value_subjects::numeric
      / (
        combined.paid_value_subjects
        + combined.paid_without_value_subjects
      )::numeric <= 0.75 then 'middle'
    else 'high'
  end as paid_value_subject_rate_band,
  'observed'::text as delivery_observation_status
from fixed_grid as grid
left join combined using (received_week);

alter view ctx_product_health.blame_weekly owner to ctx_migration;
comment on view ctx_product_health.blame_weekly is
  'Exactly 20 production settled CLI received weeks; one bounded proof-subject contribution per metric and hidden immutable key version; exact versions remain restricted raw evidence.';
revoke all privileges on table ctx_product_health.blame_weekly
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_ingest, ctx_telemetry_retention,
    ctx_product_health_readonly;
grant select on table ctx_product_health.blame_weekly
  to ctx_product_health_readonly;

-- Inventory the reader's effective authority across every non-system schema.
-- has_*_privilege includes privileges inherited from PUBLIC.
do $reader_postcondition$
declare
  v_reader oid;
begin
  select oid into strict v_reader
  from pg_roles
  where rolname = 'ctx_product_health_readonly';

  if exists (
    select 1
    from pg_auth_members as membership
    where membership.member = v_reader
  ) or not (
    select count(*) = 1
      and bool_and(
        member_role.rolname = 'neondb_owner'
        and grantor_role.rolname = 'cloud_admin'
        and membership.admin_option
        and not membership.inherit_option
        and not membership.set_option
      )
    from pg_auth_members as membership
    join pg_roles as member_role on member_role.oid = membership.member
    join pg_roles as grantor_role on grantor_role.oid = membership.grantor
    where membership.roleid = v_reader
  ) or has_database_privilege(
    'ctx_product_health_readonly', current_database(), 'create'
  ) or not has_schema_privilege(
    'ctx_product_health_readonly', 'ctx_product_health', 'usage'
  ) or has_schema_privilege(
    'ctx_product_health_readonly', 'ctx', 'usage'
  ) or exists (
    select 1
    from pg_namespace as namespace
    where namespace.nspname not in ('pg_catalog', 'information_schema')
      and namespace.nspname not like 'pg_toast%'
      and namespace.nspname not like 'pg_temp_%'
      and (
        namespace.nspowner = v_reader
        or has_schema_privilege(
          'ctx_product_health_readonly', namespace.oid, 'create'
        )
      )
  ) or exists (
    select 1
    from pg_class as relation
    join pg_namespace as namespace on namespace.oid = relation.relnamespace
    where namespace.nspname not in ('pg_catalog', 'information_schema')
      and namespace.nspname not like 'pg_toast%'
      and namespace.nspname not like 'pg_temp_%'
      and relation.relkind in ('r', 'p', 'v', 'm', 'f')
      and (
        (
          namespace.nspname = 'ctx_product_health'
          and relation.relname = 'blame_weekly'
          and has_any_column_privilege(
            'ctx_product_health_readonly', relation.oid,
            'insert,update,references'
          )
        )
        or (
          not (
            namespace.nspname = 'ctx_product_health'
            and relation.relname = 'blame_weekly'
          )
          and has_any_column_privilege(
            'ctx_product_health_readonly', relation.oid,
            'select,insert,update,references'
          )
        )
      )
  ) or exists (
    select 1
    from pg_class as relation
    join pg_namespace as namespace on namespace.oid = relation.relnamespace
    where namespace.nspname not in ('pg_catalog', 'information_schema')
      and namespace.nspname not like 'pg_toast%'
      and namespace.nspname not like 'pg_temp_%'
      and relation.relkind in ('r', 'p', 'v', 'm', 'f')
      and (
        relation.relowner = v_reader
        or (
          namespace.nspname = 'ctx_product_health'
          and relation.relname = 'blame_weekly'
          and has_table_privilege(
            'ctx_product_health_readonly', relation.oid,
            'insert,update,delete,truncate,references,trigger'
          )
        )
        or (
          not (
            namespace.nspname = 'ctx_product_health'
            and relation.relname = 'blame_weekly'
          )
          and has_table_privilege(
            'ctx_product_health_readonly', relation.oid,
            'select,insert,update,delete,truncate,references,trigger'
          )
        )
      )
  ) or exists (
    select 1
    from pg_class as sequence
    join pg_namespace as namespace on namespace.oid = sequence.relnamespace
    where namespace.nspname not in ('pg_catalog', 'information_schema')
      and namespace.nspname not like 'pg_toast%'
      and namespace.nspname not like 'pg_temp_%'
      and case when sequence.relkind = 'S' then (
        sequence.relowner = v_reader
        or has_sequence_privilege(
          'ctx_product_health_readonly', sequence.oid,
          'usage,select,update'
        )
      ) else false end
  ) or exists (
    select 1
    from pg_proc as function
    join pg_namespace as namespace on namespace.oid = function.pronamespace
    where namespace.nspname not in ('pg_catalog', 'information_schema')
      and namespace.nspname not like 'pg_toast%'
      and namespace.nspname not like 'pg_temp_%'
      and (
        function.proowner = v_reader
        or (
          has_schema_privilege(
            'ctx_product_health_readonly', namespace.oid, 'usage'
          )
          and has_function_privilege(
            'ctx_product_health_readonly', function.oid, 'execute'
          )
        )
      )
  ) or not has_table_privilege(
    'ctx_product_health_readonly',
    'ctx_product_health.blame_weekly',
    'select'
  ) then
    raise exception
      'ctx_product_health_readonly effective authority exceeds the selected product-health view';
  end if;
end
$reader_postcondition$;

commit;
