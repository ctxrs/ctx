begin;

set local lock_timeout = '5s';
set local statement_timeout = '30s';

do $precondition$
begin
  if current_user <> 'ctx_migration'
    or not exists (
      select 1 from pg_proc
      where oid = to_regprocedure('ctx.materialize_product_telemetry_history(date)')
        and pg_get_userbyid(proowner) = 'ctx_migration'
        and md5(prosrc) = '7da1e052fd9bdf62376ed25bb3870743'
    )
  then
    raise exception 'telemetry history sort memory requires ctx_migration and the reviewed materializer';
  end if;
end
$precondition$;

-- One day contains millions of receipts. Bound each sort to 64 MiB instead of
-- PostgreSQL's 4 MiB default; the Worker now issues one day per request.
alter function ctx.materialize_product_telemetry_history(date)
  set work_mem = '64MB';

commit;
