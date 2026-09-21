-- Install the typed telemetry-event contract through the relation's actual
-- owner. Production retains a legacy neondb_owner-owned telemetry_event table,
-- while a canonical source replay creates it as ctx_migration. Do not weaken
-- the NOINHERIT/SET FALSE role boundary or transfer relation ownership merely
-- to apply this forward contract.

begin;

set local lock_timeout = '5s';
set local statement_timeout = '2min';
set local idle_in_transaction_session_timeout = '2min';

select pg_advisory_xact_lock(
  hashtextextended('ctx.neon.migration.0044a.telemetry_event_contract', 0)
);

do $authority$
declare
  relation_owner text;
begin
  if not exists (
    select 1
    from pg_proc as routine
    join pg_namespace as namespace on namespace.oid = routine.pronamespace
    where namespace.nspname = 'ctx'
      and routine.proname = 'delete_expired_raw_product_telemetry'
      and pg_get_userbyid(routine.proowner) = 'ctx_migration'
      and pg_get_functiondef(routine.oid) like '%return 0;%'
  ) or not exists (
    select 1
    from pg_class as relation
    join pg_namespace as namespace on namespace.oid = relation.relnamespace
    where namespace.nspname = 'ctx'
      and relation.relname = 'telemetry_diagnostic_daily'
      and relation.relkind = 'r'
      and pg_get_userbyid(relation.relowner) = 'ctx_migration'
  ) then
    raise exception
      'migration 0044a requires the committed privacy-safe telemetry history migration 0044';
  end if;

  select pg_get_userbyid(relation.relowner)
  into relation_owner
  from pg_class as relation
  join pg_namespace as namespace on namespace.oid = relation.relnamespace
  where namespace.nspname = 'ctx'
    and relation.relname = 'telemetry_event'
    and relation.relkind = 'r';

  if relation_owner is null then
    raise exception 'migration 0044a requires ctx.telemetry_event';
  end if;
  if relation_owner not in ('ctx_migration', 'neondb_owner') then
    raise exception
      'migration 0044a refuses unexpected telemetry_event owner=%',
      relation_owner;
  end if;
  if current_user <> relation_owner then
    raise exception
      'migration 0044a must run as telemetry_event owner=%; current_user=%',
      relation_owner,
      current_user;
  end if;
  if not pg_has_role('neondb_owner', 'ctx_migration', 'MEMBER')
    or pg_has_role('neondb_owner', 'ctx_migration', 'SET')
  then
    raise exception
      'migration 0044a requires neondb_owner membership in ctx_migration with SET FALSE';
  end if;
  if not (
    select bool_and(has_table_privilege(
      'ctx_migration',
      'ctx.telemetry_event',
      required_privilege
    ))
    from unnest(array[
      'SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER'
    ]) as required(required_privilege)
  ) then
    raise exception
      'migration 0044a requires complete ctx_migration telemetry_event privileges';
  end if;
end
$authority$;

create temporary table _ctx_0044a_relation_state (
  relation_owner text not null,
  relation_acl aclitem[],
  owner_can_set_migration boolean not null
) on commit drop;

insert into _ctx_0044a_relation_state (
  relation_owner,
  relation_acl,
  owner_can_set_migration
)
select
  pg_get_userbyid(relation.relowner),
  relation.relacl,
  pg_has_role('neondb_owner', 'ctx_migration', 'SET')
from pg_class as relation
join pg_namespace as namespace on namespace.oid = relation.relnamespace
where namespace.nspname = 'ctx'
  and relation.relname = 'telemetry_event'
  and relation.relkind = 'r';

-- Admit the content-free delivery-health family. It has the same random
-- profile/root grains as ordinary public telemetry and no endpoint, response,
-- error text, or request body field.
alter table ctx.telemetry_event
  drop constraint if exists telemetry_event_new_family_schema_chk,
  drop constraint if exists telemetry_event_typed_v1_contract_chk;

alter table ctx.telemetry_event
  add constraint telemetry_event_new_family_schema_chk
    check (
      event_name not in (
        'analytics_delivery_observation',
        'operation_completed',
        'provider_refresh_completed',
        'runtime_observation'
      )
      or schema_version is not distinct from 1
    ) not valid,
  add constraint telemetry_event_typed_v1_contract_chk
    check (
      schema_version is null
      or (
        schema_version = 1
        and event_version = 1
        and event_name in (
          'analytics_delivery_observation',
          'operation_completed',
          'provider_refresh_completed',
          'runtime_observation'
        )
        and event_id ~* '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'
        and received_at is not null
        and ingested_at is not null
        and payload_fingerprint is not null
        and analytics_environment is not null
        and analytics_environment ~ '^[A-Za-z0-9._:-]{1,32}$'
        and traffic_class in (
          'unclassified_public', 'synthetic', 'internal', 'load_test', 'ci'
        )
        and activity_class is not null
        and plane = 'product'
        and num_nonnulls(
          install_id_hash,
          device_id_hash,
          broker_install_id_hash,
          broker_device_id_hash,
          origin_install_id_hash,
          origin_device_id_hash
        ) = 0
        and (
          (
            event_name = 'analytics_delivery_observation'
            and client_profile_id_hash is not null
            and data_root_id_hash is not null
            and identity_key_version is not null
            and activity_class = 'operational'
          )
          or (
            event_name = 'operation_completed'
            and client_profile_id_hash is not null
            and data_root_id_hash is not null
            and identity_key_version is not null
          )
          or (
            event_name = 'provider_refresh_completed'
            and client_profile_id_hash is not null
            and data_root_id_hash is not null
            and identity_key_version is not null
            and activity_class in ('setup', 'automatic', 'operational')
          )
          or (
            event_name = 'runtime_observation'
            and data_root_id_hash is not null
            and identity_key_version is not null
            and activity_class in ('automatic', 'liveness', 'operational')
          )
        )
      )
    ) not valid;

comment on constraint telemetry_event_new_family_schema_chk
  on ctx.telemetry_event is
  'Installed by migration 0044a for the privacy-safe typed telemetry v1 families.';
comment on constraint telemetry_event_typed_v1_contract_chk
  on ctx.telemetry_event is
  'Installed by migration 0044a for the privacy-safe typed telemetry v1 contract.';

do $postcondition$
begin
  if exists (
    select 1
    from _ctx_0044a_relation_state as before_state
    join pg_class as relation on relation.oid = 'ctx.telemetry_event'::regclass
    where pg_get_userbyid(relation.relowner) <> before_state.relation_owner
      or relation.relacl is distinct from before_state.relation_acl
      or pg_has_role('neondb_owner', 'ctx_migration', 'SET')
        is distinct from before_state.owner_can_set_migration
      or pg_has_role('neondb_owner', 'ctx_migration', 'SET')
      or not (
        select bool_and(has_table_privilege(
          'ctx_migration',
          'ctx.telemetry_event',
          required_privilege
        ))
        from unnest(array[
          'SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER'
        ]) as required(required_privilege)
      )
  ) then
    raise exception 'migration 0044a changed telemetry ownership, ACL, or role boundary';
  end if;

  if (
    select count(*) <> 2
      or bool_or(constraint_row.convalidated)
      or not bool_and(
        coalesce(
          obj_description(constraint_row.oid, 'pg_constraint') like
            'Installed by migration 0044a for the privacy-safe typed telemetry v1%',
          false
        )
      )
    from pg_constraint as constraint_row
    where constraint_row.conrelid = 'ctx.telemetry_event'::regclass
      and constraint_row.conname in (
        'telemetry_event_new_family_schema_chk',
        'telemetry_event_typed_v1_contract_chk'
      )
  ) then
    raise exception 'migration 0044a failed its unvalidated constraint swap postcondition';
  end if;
end
$postcondition$;

commit;

-- VALIDATE CONSTRAINT takes SHARE UPDATE EXCLUSIVE rather than retaining the
-- ACCESS EXCLUSIVE lock from DROP/ADD. Keep this as a second transaction so a
-- production-sized scan does not queue ordinary telemetry inserts.
begin;

set local lock_timeout = '5s';
set local statement_timeout = '15min';
set local idle_in_transaction_session_timeout = '16min';

select pg_advisory_xact_lock(
  hashtextextended('ctx.neon.migration.0044a.telemetry_event_contract', 0)
);

do $validation_authority$
declare
  relation_owner text;
begin
  select pg_get_userbyid(relation.relowner)
  into relation_owner
  from pg_class as relation
  where relation.oid = 'ctx.telemetry_event'::regclass;

  if relation_owner not in ('ctx_migration', 'neondb_owner')
    or current_user <> relation_owner
  then
    raise exception
      'migration 0044a validation must run as supported telemetry_event owner=%; current_user=%',
      relation_owner,
      current_user;
  end if;
  if not pg_has_role('neondb_owner', 'ctx_migration', 'MEMBER')
    or pg_has_role('neondb_owner', 'ctx_migration', 'SET')
  then
    raise exception
      'migration 0044a validation requires neondb_owner membership in ctx_migration with SET FALSE';
  end if;
  if not (
    select bool_and(has_table_privilege(
      'ctx_migration',
      'ctx.telemetry_event',
      required_privilege
    ))
    from unnest(array[
      'SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER'
    ]) as required(required_privilege)
  ) then
    raise exception
      'migration 0044a validation requires complete ctx_migration telemetry_event privileges';
  end if;
  if (
    select count(*) <> 2
      or bool_or(constraint_row.convalidated)
      or not bool_and(
        coalesce(
          obj_description(constraint_row.oid, 'pg_constraint') like
            'Installed by migration 0044a for the privacy-safe typed telemetry v1%',
          false
        )
      )
    from pg_constraint as constraint_row
    where constraint_row.conrelid = 'ctx.telemetry_event'::regclass
      and constraint_row.conname in (
        'telemetry_event_new_family_schema_chk',
        'telemetry_event_typed_v1_contract_chk'
      )
  ) then
    raise exception
      'migration 0044a validation requires the committed unvalidated constraint swap';
  end if;
end
$validation_authority$;

-- Capture this transaction's own boundary state. Phase two deliberately does
-- not depend on session state retained across the phase-one commit.
create temporary table _ctx_0044a_validation_state (
  relation_owner text not null,
  relation_acl aclitem[],
  owner_can_set_migration boolean not null
) on commit drop;

insert into _ctx_0044a_validation_state (
  relation_owner,
  relation_acl,
  owner_can_set_migration
)
select
  pg_get_userbyid(relation.relowner),
  relation.relacl,
  pg_has_role('neondb_owner', 'ctx_migration', 'SET')
from pg_class as relation
where relation.oid = 'ctx.telemetry_event'::regclass;

alter table ctx.telemetry_event
  validate constraint telemetry_event_new_family_schema_chk,
  validate constraint telemetry_event_typed_v1_contract_chk;

do $validated_postcondition$
begin
  if exists (
    select 1
    from _ctx_0044a_validation_state as before_state
    join pg_class as relation on relation.oid = 'ctx.telemetry_event'::regclass
    where pg_get_userbyid(relation.relowner) <> before_state.relation_owner
      or relation.relacl is distinct from before_state.relation_acl
      or pg_has_role('neondb_owner', 'ctx_migration', 'SET')
        is distinct from before_state.owner_can_set_migration
      or pg_has_role('neondb_owner', 'ctx_migration', 'SET')
      or not (
        select bool_and(has_table_privilege(
          'ctx_migration',
          'ctx.telemetry_event',
          required_privilege
        ))
        from unnest(array[
          'SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER'
        ]) as required(required_privilege)
      )
  ) then
    raise exception
      'migration 0044a validation changed telemetry ownership, ACL, or role boundary';
  end if;

  if (
    select count(*) <> 2
      or not bool_and(constraint_row.convalidated)
      or not bool_and(
        coalesce(
          obj_description(constraint_row.oid, 'pg_constraint') like
            'Installed by migration 0044a for the privacy-safe typed telemetry v1%',
          false
        )
      )
    from pg_constraint as constraint_row
    where constraint_row.conrelid = 'ctx.telemetry_event'::regclass
      and constraint_row.conname in (
        'telemetry_event_new_family_schema_chk',
        'telemetry_event_typed_v1_contract_chk'
      )
  ) then
    raise exception 'migration 0044a failed its validated constraint postcondition';
  end if;
end
$validated_postcondition$;

commit;
