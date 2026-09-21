begin;

set local lock_timeout = '5s';
set local statement_timeout = '2min';
set local idle_in_transaction_session_timeout = '2min';

select pg_advisory_xact_lock(
  hashtextextended('ctx.neon.migration.0047.telemetry_collision_receipts', 0)
);

do $precondition$
begin
  if current_user <> 'ctx_migration'
    or to_regclass('ctx.telemetry_ingest_rejection_hourly') is null
    or to_regprocedure(
      'ctx.record_telemetry_ingest_rejection(text,text,text,text,text,text,text,boolean,text,text)'
    ) is null
  then
    raise exception
      'telemetry collision receipts require ctx_migration and migration 0044';
  end if;
end
$precondition$;

-- This short-lived ledger contains only a domain-separated SHA-256 receipt
-- digest and the server receipt time. Receipts become cleanup-eligible after
-- nine days: four days in the primary Queue, four more days in the DLQ, and
-- one daily-cleanup scheduling margin. Bounded cleanup or failed runs
-- may retain eligible receipts longer without retaining event IDs or payloads.
create table if not exists ctx.telemetry_event_collision_receipt (
  collision_receipt_id text primary key,
  recorded_at timestamptz not null default clock_timestamp(),
  constraint telemetry_event_collision_receipt_id_check
    check (collision_receipt_id ~ '^[0-9a-f]{64}$')
);

alter table ctx.telemetry_event_collision_receipt owner to ctx_migration;
comment on table ctx.telemetry_event_collision_receipt is
  'Collision receipts become cleanup-eligible after nine days; bounded scheduled cleanup may retain them longer and stores only a privacy-safe SHA-256 digest and server receipt time.';
comment on column ctx.telemetry_event_collision_receipt.collision_receipt_id is
  'Domain-separated SHA-256 digest of the stable environment, message kind, event ID, and payload fingerprint collision identity.';
comment on column ctx.telemetry_event_collision_receipt.recorded_at is
  'Server time used only to determine bounded cleanup eligibility.';

create index if not exists telemetry_event_collision_receipt_recorded_at_idx
  on ctx.telemetry_event_collision_receipt (
    recorded_at,
    collision_receipt_id
  );

create or replace function ctx.delete_expired_telemetry_event_collision_receipts(
  p_before timestamptz default clock_timestamp() - interval '9 days',
  p_limit integer default 128
)
returns bigint
language plpgsql
volatile
security definer
set search_path = pg_catalog, ctx
as $delete_expired_collision_receipts$
declare
  deleted_count bigint;
begin
  if p_before is null or p_limit is null or p_limit < 1 or p_limit > 512 then
    raise exception using
      errcode = '22023',
      message = 'invalid telemetry collision receipt cleanup bound';
  end if;

  if p_before > clock_timestamp() - interval '9 days' then
    raise exception using
      errcode = '22023',
      message = 'telemetry collision receipt cleanup must retain nine days';
  end if;

  with expired as materialized (
    select receipt.collision_receipt_id
    from ctx.telemetry_event_collision_receipt as receipt
    where receipt.recorded_at < p_before
    order by receipt.recorded_at, receipt.collision_receipt_id
    limit p_limit
    for update skip locked
  ), deleted as (
    delete from ctx.telemetry_event_collision_receipt as receipt
    using expired
    where receipt.collision_receipt_id = expired.collision_receipt_id
    returning 1
  )
  select count(*)::bigint into deleted_count
  from deleted;

  return deleted_count;
end
$delete_expired_collision_receipts$;

alter function ctx.delete_expired_telemetry_event_collision_receipts(
  timestamptz, integer
)
  owner to ctx_migration;
comment on function ctx.delete_expired_telemetry_event_collision_receipts(
  timestamptz, integer
) is
  'Rejects cleanup cutoffs newer than nine days, then deletes at most the caller-supplied bound of eligible collision receipt digests; the default selects receipts older than nine days and deletes at most 128 rows per call.';

create or replace function ctx.record_telemetry_event_collision(
  p_collision_receipt_id text,
  p_analytics_environment text,
  p_ingest_endpoint text,
  p_event_family text,
  p_app_version text,
  p_field_shape_fingerprint text,
  p_field_shape_overflow boolean,
  p_provider_classification text,
  p_size_bucket text
)
returns boolean
language plpgsql
volatile
security definer
set search_path = pg_catalog, ctx
as $record_event_collision$
declare
  inserted_count bigint;
begin
  if p_collision_receipt_id is null
    or p_collision_receipt_id !~ '^[0-9a-f]{64}$'
  then
    raise exception using
      errcode = '23514',
      message = 'invalid telemetry collision receipt';
  end if;

  perform ctx.delete_expired_telemetry_event_collision_receipts(
    clock_timestamp() - interval '9 days',
    128
  );

  insert into ctx.telemetry_event_collision_receipt (
    collision_receipt_id
  ) values (
    p_collision_receipt_id
  )
  on conflict (collision_receipt_id) do nothing;
  get diagnostics inserted_count = row_count;

  if inserted_count = 1 then
    perform ctx.record_telemetry_ingest_rejection(
      p_analytics_environment,
      p_ingest_endpoint,
      p_event_family,
      'event_collision',
      'event_id_collision',
      p_app_version,
      p_field_shape_fingerprint,
      p_field_shape_overflow,
      p_provider_classification,
      p_size_bucket
    );
  end if;

  return inserted_count = 1;
end
$record_event_collision$;

alter function ctx.record_telemetry_event_collision(
  text, text, text, text, text, text, boolean, text, text
)
  owner to ctx_migration;
comment on function ctx.record_telemetry_event_collision(
  text, text, text, text, text, text, boolean, text, text
) is
  'Atomically retains one privacy-safe collision receipt and increments the bounded event-collision aggregate only when that receipt is new.';

revoke all on table ctx.telemetry_event_collision_receipt
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_ingest, ctx_telemetry_retention,
    ctx_product_health_readonly;
revoke all on function ctx.delete_expired_telemetry_event_collision_receipts(
  timestamptz, integer
)
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_ingest, ctx_product_health_readonly;
revoke all on function ctx.record_telemetry_event_collision(
  text, text, text, text, text, text, boolean, text, text
)
  from public, ctx_analytics_readonly, ctx_control_plane,
    ctx_telemetry_retention, ctx_product_health_readonly;
grant execute on function ctx.delete_expired_telemetry_event_collision_receipts(
  timestamptz, integer
)
  to ctx_migration, ctx_telemetry_retention;
grant execute on function ctx.record_telemetry_event_collision(
  text, text, text, text, text, text, boolean, text, text
)
  to ctx_migration, ctx_telemetry_ingest;

commit;
