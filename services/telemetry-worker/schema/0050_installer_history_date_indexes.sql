-- Run as ctx_migration, outside a transaction, before deploying nine-day
-- history catch-up. Both branches of the existing daily installer query need
-- a leading date key; the older dimension-leading indexes cannot provide it.
set lock_timeout = '5s';
set statement_timeout = '5min';

do $precondition$
begin
  if current_user <> 'ctx_migration'
    or to_regprocedure('ctx.materialize_product_telemetry_history(date)') is null
  then
    raise exception 'installer history indexes require ctx_migration and the daily materializer';
  end if;
end
$precondition$;

create index concurrently if not exists install_attempt_event_received_at_idx
  on ctx.install_attempt_event (received_at)
  where received_at is not null;

create index concurrently if not exists install_attempt_event_legacy_occurred_at_idx
  on ctx.install_attempt_event (occurred_at)
  where received_at is null;

do $verify$
begin
  if (
    select count(*) from pg_index
    where indexrelid in (
      to_regclass('ctx.install_attempt_event_received_at_idx'),
      to_regclass('ctx.install_attempt_event_legacy_occurred_at_idx')
    )
    and indrelid = 'ctx.install_attempt_event'::regclass
    and indisvalid and indisready
  ) <> 2 then
    raise exception 'installer history indexes are not valid and ready; inspect before retry';
  end if;
end
$verify$;
