-- Additive restricted operator reads. No event storage, identity or retention change.
begin;
set local lock_timeout='5s';
set local statement_timeout='30s';
do $precondition$
begin
  if current_user <> 'ctx_migration' or to_regclass('ctx.analytics_canonical_telemetry_events') is null
    or not exists(select 1 from pg_roles where rolname='ctx_analytics_readonly') then
    raise exception 'product telemetry requires ctx_migration, canonical telemetry and analytics reader';
  end if;
end
$precondition$;

create or replace function ctx.analytics_product_health_window(
  p_environment text, p_from timestamptz, p_until timestamptz, p_versions text[] default null
) returns table (
  population text, aggregation_level text, product text, app_version text, surface text,
  os text, arch text, event_name text, operation text, action text, outcome text,
  failure_stage text, failure_class text, sift_host text, sift_mode text, cohort jsonb,
  observed_samples_bucket text, complete_measurements_bucket text,
  partial_measurements_bucket text, unmeasured_invocations_bucket text,
  receipt_count bigint, success_count bigint, failure_count bigint,
  execution_observed_count bigint, execution_success_count bigint,
  delivery_observed_count bigint, delivery_complete_count bigint,
  result_observed_count bigint, nonempty_result_count bigint,
  measured_savings_summary_count bigint, observed_profile_count bigint, observed_data_root_count bigint
) language plpgsql stable security definer
set search_path=pg_catalog,ctx,pg_temp
as $function$
begin
  if p_environment is null or p_environment not in ('production','staging')
    or p_from is null or p_until is null or not isfinite(p_from) or not isfinite(p_until)
    or p_until <= p_from or p_until - p_from > interval '24 hours' then
    raise exception using errcode='22023', message='expected production/staging and a finite positive window of at most 24 hours';
  end if;
  if exists (select 1 from unnest(p_versions) v where v is null or btrim(v)='') then
    raise exception using errcode='22023', message='version filters must contain nonempty values';
  end if;
  return query
  with bounded as (
    select e.*,
      case when e.operation='search' then 'search'
        when e.operation='graph' or e.properties->>'runtime_kind' in ('graph_serve','graph_watch') then 'graph'
        when e.operation='sift_summary' then 'sift' when e.operation='sharing_summary' then 'sharing' when e.operation in ('remote','remote_connect','remote_share','remote_sync','remote_pause','remote_resume','remote_status','remote_remove') then 'remote'
        when e.operation in ('archive_export','archive_restore','archive_verify') then 'archive' else 'server' end as product,
      case when e.operation='sift_summary' then 'observed_sift_summaries'
        when e.operation='server_summary' then 'observed_server_' || (e.properties->>'server_population') || '_summaries'
        when e.operation='sharing_summary' then 'observed_sharing_summaries'
        when e.operation='product_runtime' then 'observed_runtime'
        when e.surface='server' then 'observed_server_requests'
        else 'observed_foreground_operations' end as population,
      case when e.operation='graph' then e.properties->>'graph_operation'
        when e.operation='sift_summary' then e.properties->>'sift_operation'
        when e.operation in ('server_request','server_summary') then e.properties->>'server_operation'
        when e.operation='remote' then e.properties->>'remote_operation'
        when e.operation='sharing_summary' then e.properties->>'sharing_operation'
        when e.operation='product_runtime' then e.properties->>'runtime_phase'
        else e.operation end as action,
      jsonb_strip_nulls(jsonb_build_object('graph_invocation',e.properties->'graph_invocation','graph_failure_phase',e.properties->'graph_failure_phase','graph_failure_kind',e.properties->'graph_failure_kind','server_population',e.properties->'server_population','server_failure',e.properties->'server_failure','response_class',e.properties->'response_class','server_body_outcome',e.properties->'server_body_outcome','sift_entry',e.properties->'sift_entry','sift_terminal',e.properties->'sift_terminal','sift_outcome',e.properties->'sift_outcome','sift_delivery',e.properties->'sift_delivery','sift_skip',e.properties->'sift_skip','sift_failure_phase',e.properties->'sift_failure_phase','sift_failure_kind',e.properties->'sift_failure_kind','sift_child',e.properties->'sift_child','sift_missingness',e.properties->'sift_missingness','sift_semantic_mode',e.properties->'sift_semantic_mode','sift_semantic_disposition',e.properties->'sift_semantic_disposition','sift_semantic_provider',e.properties->'sift_semantic_provider','sharing_phase',e.properties->'sharing_phase','sharing_tick',e.properties->'sharing_tick','sharing_failure',e.properties->'sharing_failure','sharing_selection',e.properties->'sharing_selection')) as cohort
    from ctx.analytics_canonical_telemetry_events e
    where e.analytics_environment=p_environment
      and e.occurred_at >= p_from and e.occurred_at < p_until
      and (p_versions is null or e.app_version=any(p_versions))
      and e.schema_version=1 and e.event_version=1 and e.plane='product'
      and e.app_version is not null and e.app_version !~ '^0\.0\.0'
      and coalesce(e.provider_id,'') <> 'fake' and coalesce(e.properties->>'smoke_run_id','')=''
      and ((p_environment='production' and e.is_eligible_product_telemetry)
        or (p_environment='staging' and e.traffic_class='synthetic'))
      and (
        (e.event_name='operation_completed' and (
          (e.surface in ('cli','mcp') and e.operation in ('search','graph','remote'))
          or (e.surface='server' and e.operation='server_request')
          or (e.surface='cli' and e.operation in ('server_init','server_invite','server_grant',
            'server_revoke','server_withdraw','server_backup','server_restore','server_collection_create',
            'server_user_list','server_user_credentials','server_user_create','server_user_credential',
            'server_publications','server_status','archive_export','archive_restore','archive_verify',
            'remote_connect','remote_share','remote_sync','remote_pause','remote_resume','remote_status','remote_remove'))))
        or (e.event_name='runtime_observation' and (
          (e.surface in ('cli','mcp') and e.operation='sift_summary')
          or (e.surface='server' and e.operation='server_summary')
          or (e.surface='daemon' and e.operation='sharing_summary')
          or (e.operation='product_runtime' and (
            (e.surface='server' and e.properties->>'runtime_kind'='server')
            or (e.surface='mcp' and e.properties->>'runtime_kind'='graph_serve')
            or (e.surface='cli' and e.properties->>'runtime_kind'='graph_watch')))))
      )
  ), facts as (
    select b.*,
      coalesce(b.properties->>'product_failure_stage', b.properties->>'hosted_failure_stage', b.properties->>'search_failure_phase') as failure_stage,
      coalesce(b.properties->>'product_failure_class', b.properties->>'failure_type') as failure_class,
      b.properties->>'sift_host' as sift_host, b.properties->>'sift_mode' as sift_mode,
      b.properties->>'observed_count_bucket' as observed_samples_bucket,
      b.properties->>'complete_measurement_count_bucket' as complete_measurements_bucket,
      b.properties->>'partial_measurement_count_bucket' as partial_measurements_bucket,
      b.properties->>'unmeasured_count_bucket' as unmeasured_invocations_bucket,
      case when b.operation='search' and jsonb_typeof(b.properties->'search_output_served')='boolean'
        then case when (b.properties->>'search_output_served')::boolean then 'known_complete' else 'failed' end
        else b.properties->>'output_delivery' end as delivery,
      case when b.operation='search' and jsonb_typeof(b.properties->'zero_result')='boolean'
        then (b.properties->>'zero_result')::boolean
        when jsonb_typeof(b.properties->'result_empty')='boolean'
        then (b.properties->>'result_empty')::boolean end as empty
    from bounded b
  )
  select
    case when p_environment='staging' then 'synthetic_qualification:' else 'production:' end || f.population,
    case when grouping(f.operation)=1 then 'product' else 'operation' end,
    f.product, f.app_version, f.surface, f.os, f.arch, f.event_name, f.operation, f.action, f.outcome,
    f.failure_stage, f.failure_class, f.sift_host, f.sift_mode, f.cohort,
    f.observed_samples_bucket, f.complete_measurements_bucket,
    f.partial_measurements_bucket, f.unmeasured_invocations_bucket,
    count(*)::bigint, count(*) filter(where f.outcome='success')::bigint,
    count(*) filter(where f.outcome='failure')::bigint,
    count(*) filter(where f.properties->>'execution_result' in ('success','failure'))::bigint,
    count(*) filter(where f.properties->>'execution_result'='success')::bigint,
    count(*) filter(where f.delivery in ('known_complete','failed'))::bigint,
    count(*) filter(where f.delivery='known_complete')::bigint,
    count(*) filter(where f.empty is not null)::bigint,
    count(*) filter(where f.empty is false)::bigint,
    count(*) filter(where f.operation='sift_summary' and
      (f.properties ? 'sift_tokens_input_bucket' or f.properties ? 'sift_bytes_input_bucket'))::bigint,
    count(distinct (f.identity_key_version,f.client_profile_id_hash)) filter(where f.client_profile_id_hash is not null)::bigint,
    count(distinct (f.identity_key_version,f.data_root_id_hash)) filter(where f.data_root_id_hash is not null)::bigint
  from facts f
  group by grouping sets (
    (f.product,f.population),
    (f.product,f.population,f.app_version,f.surface,f.os,f.arch,f.event_name,f.operation,f.action,f.outcome,
     f.failure_stage,f.failure_class,f.sift_host,f.sift_mode,f.cohort,f.observed_samples_bucket,
     f.complete_measurements_bucket,f.partial_measurements_bucket,f.unmeasured_invocations_bucket)
  );
end
$function$;

create or replace function ctx.analytics_product_measurements_window(
  p_environment text, p_from timestamptz, p_until timestamptz, p_versions text[] default null
) returns table (
  population text, product text, app_version text, surface text, operation text, action text,
  sift_host text, sift_mode text, cohort jsonb, count_unit text, dimension text, bucket text,
  eligible_count bigint, measured_count bigint, bucket_count bigint
) language plpgsql stable security definer
set search_path=pg_catalog,ctx,pg_temp
as $function$
begin
  if p_environment is null or p_environment not in ('production','staging')
    or p_from is null or p_until is null or not isfinite(p_from) or not isfinite(p_until)
    or p_until <= p_from or p_until - p_from > interval '24 hours' then
    raise exception using errcode='22023', message='expected production/staging and a finite positive window of at most 24 hours';
  end if;
  if exists (select 1 from unnest(p_versions) v where v is null or btrim(v)='') then
    raise exception using errcode='22023', message='version filters must contain nonempty values';
  end if;
  return query
  with bounded as (
    select e.*,
      case when e.operation='search' then 'search'
        when e.operation='graph' or e.properties->>'runtime_kind' in ('graph_serve','graph_watch') then 'graph'
        when e.operation='sift_summary' then 'sift' when e.operation='sharing_summary' then 'sharing' when e.operation in ('remote','remote_connect','remote_share','remote_sync','remote_pause','remote_resume','remote_status','remote_remove') then 'remote'
        when e.operation in ('archive_export','archive_restore','archive_verify') then 'archive' else 'server' end as product,
      case when e.operation='sift_summary' then 'observed_sift_summaries'
        when e.operation='server_summary' then 'observed_server_' || (e.properties->>'server_population') || '_summaries'
        when e.operation='sharing_summary' then 'observed_sharing_summaries'
        when e.operation='product_runtime' then 'observed_runtime'
        when e.surface='server' then 'observed_server_requests'
        else 'observed_foreground_operations' end as population,
      case when e.operation='graph' then e.properties->>'graph_operation'
        when e.operation='sift_summary' then e.properties->>'sift_operation'
        when e.operation in ('server_request','server_summary') then e.properties->>'server_operation'
        when e.operation='remote' then e.properties->>'remote_operation'
        when e.operation='sharing_summary' then e.properties->>'sharing_operation'
        when e.operation='product_runtime' then e.properties->>'runtime_phase'
        else e.operation end as action,
      jsonb_strip_nulls(jsonb_build_object('graph_invocation',e.properties->'graph_invocation','graph_failure_phase',e.properties->'graph_failure_phase','graph_failure_kind',e.properties->'graph_failure_kind','server_population',e.properties->'server_population','server_failure',e.properties->'server_failure','response_class',e.properties->'response_class','server_body_outcome',e.properties->'server_body_outcome','sift_entry',e.properties->'sift_entry','sift_terminal',e.properties->'sift_terminal','sift_outcome',e.properties->'sift_outcome','sift_delivery',e.properties->'sift_delivery','sift_skip',e.properties->'sift_skip','sift_failure_phase',e.properties->'sift_failure_phase','sift_failure_kind',e.properties->'sift_failure_kind','sift_child',e.properties->'sift_child','sift_missingness',e.properties->'sift_missingness','sift_semantic_mode',e.properties->'sift_semantic_mode','sift_semantic_disposition',e.properties->'sift_semantic_disposition','sift_semantic_provider',e.properties->'sift_semantic_provider','sharing_phase',e.properties->'sharing_phase','sharing_tick',e.properties->'sharing_tick','sharing_failure',e.properties->'sharing_failure','sharing_selection',e.properties->'sharing_selection')) as cohort
    from ctx.analytics_canonical_telemetry_events e
    where e.analytics_environment=p_environment
      and e.occurred_at >= p_from and e.occurred_at < p_until
      and (p_versions is null or e.app_version=any(p_versions))
      and e.schema_version=1 and e.event_version=1 and e.plane='product'
      and e.app_version is not null and e.app_version !~ '^0\.0\.0'
      and coalesce(e.provider_id,'') <> 'fake' and coalesce(e.properties->>'smoke_run_id','')=''
      and ((p_environment='production' and e.is_eligible_product_telemetry)
        or (p_environment='staging' and e.traffic_class='synthetic'))
      and (
        (e.event_name='operation_completed' and (
          (e.surface in ('cli','mcp') and e.operation in ('search','graph','remote'))
          or (e.surface='server' and e.operation='server_request')
          or (e.surface='cli' and e.operation in ('server_init','server_invite','server_grant',
            'server_revoke','server_withdraw','server_backup','server_restore','server_collection_create',
            'server_user_list','server_user_credentials','server_user_create','server_user_credential',
            'server_publications','server_status','archive_export','archive_restore','archive_verify',
            'remote_connect','remote_share','remote_sync','remote_pause','remote_resume','remote_status','remote_remove'))))
        or (e.event_name='runtime_observation' and (
          (e.surface in ('cli','mcp') and e.operation='sift_summary')
          or (e.surface='server' and e.operation='server_summary')
          or (e.surface='daemon' and e.operation='sharing_summary')
          or (e.operation='product_runtime' and (
            (e.surface='server' and e.properties->>'runtime_kind'='server')
            or (e.surface='mcp' and e.properties->>'runtime_kind'='graph_serve')
            or (e.surface='cli' and e.properties->>'runtime_kind'='graph_watch')))))
      )
  ), ranked as (
    select b.*, row_number() over (
      partition by b.identity_key_version,b.client_profile_id_hash,b.data_root_id_hash,
        b.properties->>'runtime_kind'
      order by b.received_at desc,b.event_id desc
    ) as runtime_rank
    from bounded b
    where b.operation='product_runtime' and b.properties->>'runtime_phase' in ('ready','liveness')
  ), observations as (
    select b.*, 'terminal_event'::text as count_unit from bounded b where b.event_name='operation_completed'
    union all
    select b.*, 'summary'::text from bounded b where b.operation in ('sift_summary','server_summary','sharing_summary')
    union all
    select b.*, 'runtime_grain'::text from bounded b join ranked r on r.event_id=b.event_id where r.runtime_rank=1
  ), projected as (
    select e.population,e.product,e.app_version,e.surface,e.operation,e.action,
      e.properties->>'sift_host' as sift_host,e.properties->>'sift_mode' as sift_mode,
      e.cohort,e.count_unit,v.dimension,v.observed_bucket,v.vocabulary
    from observations e
    cross join lateral (values
      ('duration_bucket','terminal',e.duration_bucket,array['unknown','lt_100ms','lt_1s','lt_5s','lt_30s','lt_2m','lt_10m','lt_1h','gte_1h']::text[]),
      ('native_total_duration_bucket','terminal',e.properties->>'native_total_duration_bucket',array['lt_1ms','1ms-5ms','5ms-10ms','10ms-25ms','25ms-50ms','50ms-100ms','100ms-250ms','250ms-1s','1s-5s','5s-30s','30s-2m','2m-10m','10m-1h','1h+']::text[]),
      ('prepare_duration_bucket','terminal',e.properties->>'prepare_duration_bucket',array['lt_1ms','1ms-5ms','5ms-10ms','10ms-25ms','25ms-50ms','50ms-100ms','100ms-250ms','250ms-1s','1s-5s','5s-30s','30s-2m','2m-10m','10m-1h','1h+']::text[]),
      ('work_duration_bucket','terminal',e.properties->>'work_duration_bucket',array['lt_1ms','1ms-5ms','5ms-10ms','10ms-25ms','25ms-50ms','50ms-100ms','100ms-250ms','250ms-1s','1s-5s','5s-30s','30s-2m','2m-10m','10m-1h','1h+']::text[]),
      ('output_duration_bucket','terminal',e.properties->>'output_duration_bucket',array['lt_1ms','1ms-5ms','5ms-10ms','10ms-25ms','25ms-50ms','50ms-100ms','100ms-250ms','250ms-1s','1s-5s','5s-30s','30s-2m','2m-10m','10m-1h','1h+']::text[]),
      ('refresh_duration_bucket','search',e.properties->>'refresh_duration_bucket',array['unknown','lt_100ms','lt_1s','lt_5s','lt_30s','lt_2m','lt_10m','lt_1h','gte_1h']::text[]),
      ('query_duration_bucket','search',e.properties->>'query_duration_bucket',array['unknown','lt_100ms','lt_1s','lt_5s','lt_30s','lt_2m','lt_10m','lt_1h','gte_1h']::text[]),
      ('render_duration_bucket','search',e.properties->>'render_duration_bucket',array['unknown','lt_100ms','lt_1s','lt_5s','lt_30s','lt_2m','lt_10m','lt_1h','gte_1h']::text[]),
      ('search_output_duration_bucket','search',e.properties->>'search_output_duration_bucket',array['unknown','lt_100ms','lt_1s','lt_5s','lt_30s','lt_2m','lt_10m','lt_1h','gte_1h']::text[]),
      ('result_count_bucket','terminal',e.properties->>'result_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_node_count_bucket','terminal',e.properties->>'graph_node_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_edge_count_bucket','terminal',e.properties->>'graph_edge_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_files_processed_bucket','terminal',e.properties->>'graph_files_processed_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('search_candidate_rows_total_bucket','search',e.properties->>'search_candidate_rows_total_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('search_candidate_records_decoded_bucket','search',e.properties->>'search_candidate_records_decoded_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('search_retrieval_round_count_bucket','search',e.properties->>'search_retrieval_round_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('search_query_execution_count_bucket','search',e.properties->>'search_query_execution_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('search_final_candidate_pool_bucket','search',e.properties->>'search_final_candidate_pool_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('result_truncated','terminal',e.properties->>'result_truncated',array['true','false']::text[]),
      ('search_candidate_pool_truncated','search',e.properties->>'search_candidate_pool_truncated',array['true','false']::text[]),
      ('search_candidate_core_bytes_decoded_bucket','search',e.properties->>'search_candidate_core_bytes_decoded_bucket',array['0','lt_100kb','100kb-1mb','1mb-10mb','10mb-100mb','100mb-1gb','1gb-2gb','2gb-5gb','5gb-10gb','10gb-25gb','25gb-50gb','50gb-100gb','100gb+']::text[]),
      ('search_backend_requested','search',e.properties->>'search_backend_requested',array['hybrid','lexical','semantic']::text[]),
      ('search_backend_effective','search',e.properties->>'search_backend_effective',array['hybrid','lexical','semantic']::text[]),
      ('response_class','server',e.properties->>'response_class',array['2xx','3xx','4xx','5xx','not_produced','unavailable','other']::text[]),
      ('response_handoff','server',e.properties->>'response_handoff',array['known_complete','failed','unknown','not_attempted']::text[]),
      ('observed_count_bucket','summary',e.properties->>'observed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_failed_count_bucket','summary',e.properties->>'execution_failed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('output_failed_count_bucket','summary',e.properties->>'output_failed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('complete_measurement_count_bucket','summary',e.properties->>'complete_measurement_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('partial_measurement_count_bucket','summary',e.properties->>'partial_measurement_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('unmeasured_count_bucket','summary',e.properties->>'unmeasured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_measured_count_bucket','summary',e.properties->>'latency_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_0_count_bucket','summary',e.properties->>'sift_latency_0_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_1_count_bucket','summary',e.properties->>'sift_latency_1_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_2_count_bucket','summary',e.properties->>'sift_latency_2_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_3_count_bucket','summary',e.properties->>'sift_latency_3_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_4_count_bucket','summary',e.properties->>'sift_latency_4_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_5_count_bucket','summary',e.properties->>'sift_latency_5_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_6_count_bucket','summary',e.properties->>'sift_latency_6_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_7_count_bucket','summary',e.properties->>'sift_latency_7_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_8_count_bucket','summary',e.properties->>'sift_latency_8_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_9_count_bucket','summary',e.properties->>'sift_latency_9_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_10_count_bucket','summary',e.properties->>'sift_latency_10_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_11_count_bucket','summary',e.properties->>'sift_latency_11_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_12_count_bucket','summary',e.properties->>'sift_latency_12_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_latency_13_count_bucket','summary',e.properties->>'sift_latency_13_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_bytes_measured_count_bucket','summary',e.properties->>'sift_bytes_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_bytes_input_bucket','summary',e.properties->>'sift_bytes_input_bucket',array['0','lt_100kb','100kb-1mb','1mb-10mb','10mb-100mb','100mb-1gb','1gb-2gb','2gb-5gb','5gb-10gb','10gb-25gb','25gb-50gb','50gb-100gb','100gb+']::text[]),
      ('sift_bytes_output_bucket','summary',e.properties->>'sift_bytes_output_bucket',array['0','lt_100kb','100kb-1mb','1mb-10mb','10mb-100mb','100mb-1gb','1gb-2gb','2gb-5gb','5gb-10gb','10gb-25gb','25gb-50gb','50gb-100gb','100gb+']::text[]),
      ('sift_bytes_delta_bucket','summary',e.properties->>'sift_bytes_delta_bucket',array['0','lt_100kb','100kb-1mb','1mb-10mb','10mb-100mb','100mb-1gb','1gb-2gb','2gb-5gb','5gb-10gb','10gb-25gb','25gb-50gb','50gb-100gb','100gb+']::text[]),
      ('sift_bytes_change','summary',e.properties->>'sift_bytes_change',array['increased','unchanged','reduced']::text[]),
      ('sift_bytes_savings_fraction_bucket','summary',e.properties->>'sift_bytes_savings_fraction_bucket',array['increased','unchanged','lt_10pct','10pct-25pct','25pct-50pct','50pct-75pct','75pct-90pct','90pct-100pct','100pct']::text[]),
      ('sift_tokens_measured_count_bucket','summary',e.properties->>'sift_tokens_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_tokens_input_bucket','summary',e.properties->>'sift_tokens_input_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_tokens_output_bucket','summary',e.properties->>'sift_tokens_output_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_tokens_delta_bucket','summary',e.properties->>'sift_tokens_delta_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_tokens_change','summary',e.properties->>'sift_tokens_change',array['increased','unchanged','reduced']::text[]),
      ('sift_tokens_savings_fraction_bucket','summary',e.properties->>'sift_tokens_savings_fraction_bucket',array['increased','unchanged','lt_10pct','10pct-25pct','25pct-50pct','50pct-75pct','75pct-90pct','90pct-100pct','100pct']::text[]),
      ('uptime_bucket','runtime',e.properties->>'uptime_bucket',array['lt_1h','1h-1d','1d-7d','7d+']::text[]),
      ('active_requests_bucket','runtime',e.properties->>'active_requests_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('pending_work_bucket','runtime',e.properties->>'pending_work_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_polls_count_bucket','graph',e.properties->>'graph_polls_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_retries_count_bucket','graph',e.properties->>'graph_retries_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_query_unresolved_count_bucket','graph',e.properties->>'graph_query_unresolved_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_query_seeds_count_bucket','graph',e.properties->>'graph_query_seeds_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_index_parsed_count_bucket','graph',e.properties->>'graph_index_parsed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_index_rejected_count_bucket','graph',e.properties->>'graph_index_rejected_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_index_unchanged_count_bucket','graph',e.properties->>'graph_index_unchanged_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_index_deleted_count_bucket','graph',e.properties->>'graph_index_deleted_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_index_diagnostics_count_bucket','graph',e.properties->>'graph_index_diagnostics_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_analysis_communities_count_bucket','graph',e.properties->>'graph_analysis_communities_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_analysis_passes_count_bucket','graph',e.properties->>'graph_analysis_passes_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_analysis_unsatisfied_constraints_count_bucket','graph',e.properties->>'graph_analysis_unsatisfied_constraints_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_reserved_generations_count_bucket','graph',e.properties->>'graph_semantic_reserved_generations_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_reserved_output_tokens_count_bucket','graph',e.properties->>'graph_semantic_reserved_output_tokens_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_receipts_count_bucket','graph',e.properties->>'graph_semantic_receipts_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_input_known_count_bucket','graph',e.properties->>'graph_semantic_input_known_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_input_reporters_count_bucket','graph',e.properties->>'graph_semantic_input_reporters_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_output_known_count_bucket','graph',e.properties->>'graph_semantic_output_known_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_output_reporters_count_bucket','graph',e.properties->>'graph_semantic_output_reporters_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_total_known_count_bucket','graph',e.properties->>'graph_semantic_total_known_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_total_reporters_count_bucket','graph',e.properties->>'graph_semantic_total_reporters_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_cache_read_known_count_bucket','graph',e.properties->>'graph_semantic_cache_read_known_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_cache_read_reporters_count_bucket','graph',e.properties->>'graph_semantic_cache_read_reporters_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_cache_create_known_count_bucket','graph',e.properties->>'graph_semantic_cache_create_known_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_cache_create_reporters_count_bucket','graph',e.properties->>'graph_semantic_cache_create_reporters_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_reasoning_known_count_bucket','graph',e.properties->>'graph_semantic_reasoning_known_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_semantic_reasoning_reporters_count_bucket','graph',e.properties->>'graph_semantic_reasoning_reporters_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('graph_bound_seed','graph',e.properties->>'graph_bound_seed',array['true','false']::text[]),
      ('graph_bound_node','graph',e.properties->>'graph_bound_node',array['true','false']::text[]),
      ('graph_bound_work','graph',e.properties->>'graph_bound_work',array['true','false']::text[]),
      ('graph_bound_depth','graph',e.properties->>'graph_bound_depth',array['true','false']::text[]),
      ('graph_bound_unresolved','graph',e.properties->>'graph_bound_unresolved',array['true','false']::text[]),
      ('graph_bound_token','graph',e.properties->>'graph_bound_token',array['true','false']::text[]),
      ('graph_bound_other','graph',e.properties->>'graph_bound_other',array['true','false']::text[]),
      ('graph_index_fresh','graph',e.properties->>'graph_index_fresh',array['true','false']::text[]),
      ('graph_analysis_pagerank_converged','graph',e.properties->>'graph_analysis_pagerank_converged',array['true','false']::text[]),
      ('graph_artifact_snapshot_cache_hit','graph',e.properties->>'graph_artifact_snapshot_cache_hit',array['true','false']::text[]),
      ('graph_artifact_analysis_cache_hit','graph',e.properties->>'graph_artifact_analysis_cache_hit',array['true','false']::text[]),
      ('graph_artifact_committed','graph',e.properties->>'graph_artifact_committed',array['true','false']::text[]),
      ('graph_semantic_configured','graph',e.properties->>'graph_semantic_configured',array['true','false']::text[]),
      ('graph_semantic_usage_unavailable','graph',e.properties->>'graph_semantic_usage_unavailable',array['true','false']::text[]),
      ('graph_query_duration_bucket','graph',e.properties->>'graph_query_duration_bucket',array['lt_1ms','1ms-5ms','5ms-10ms','10ms-25ms','25ms-50ms','50ms-100ms','100ms-250ms','250ms-1s','1s-5s','5s-30s','30s-2m','2m-10m','10m-1h','1h+']::text[]),
      ('graph_index_capture_duration_bucket','graph',e.properties->>'graph_index_capture_duration_bucket',array['lt_1ms','1ms-5ms','5ms-10ms','10ms-25ms','25ms-50ms','50ms-100ms','100ms-250ms','250ms-1s','1s-5s','5s-30s','30s-2m','2m-10m','10m-1h','1h+']::text[]),
      ('graph_index_detect_duration_bucket','graph',e.properties->>'graph_index_detect_duration_bucket',array['lt_1ms','1ms-5ms','5ms-10ms','10ms-25ms','25ms-50ms','50ms-100ms','100ms-250ms','250ms-1s','1s-5s','5s-30s','30s-2m','2m-10m','10m-1h','1h+']::text[]),
      ('graph_index_extract_duration_bucket','graph',e.properties->>'graph_index_extract_duration_bucket',array['lt_1ms','1ms-5ms','5ms-10ms','10ms-25ms','25ms-50ms','50ms-100ms','100ms-250ms','250ms-1s','1s-5s','5s-30s','30s-2m','2m-10m','10m-1h','1h+']::text[]),
      ('graph_index_commit_duration_bucket','graph',e.properties->>'graph_index_commit_duration_bucket',array['lt_1ms','1ms-5ms','5ms-10ms','10ms-25ms','25ms-50ms','50ms-100ms','100ms-250ms','250ms-1s','1s-5s','5s-30s','30s-2m','2m-10m','10m-1h','1h+']::text[]),
      ('graph_analysis_duration_bucket','graph',e.properties->>'graph_analysis_duration_bucket',array['lt_1ms','1ms-5ms','5ms-10ms','10ms-25ms','25ms-50ms','50ms-100ms','100ms-250ms','250ms-1s','1s-5s','5s-30s','30s-2m','2m-10m','10m-1h','1h+']::text[]),
      ('graph_artifact_bytes_bucket','graph',e.properties->>'graph_artifact_bytes_bucket',array['0','lt_100kb','100kb-1mb','1mb-10mb','10mb-100mb','100mb-1gb','1gb-2gb','2gb-5gb','5gb-10gb','10gb-25gb','25gb-50gb','50gb-100gb','100gb+']::text[]),
      ('graph_invocation','graph',e.properties->>'graph_invocation',array['cli','scoped_search','unified_mcp','native_stdio','native_http','library']::text[]),
      ('graph_phase','graph',e.properties->>'graph_phase',array['parse','prepare','discover','open','capture','detect','extract','commit','post_commit','query','snapshot','analysis','render','artifact_write','output_write','output_flush','registration','bind','protocol','admission','worker','shutdown']::text[]),
      ('graph_output_boundary','graph',e.properties->>'graph_output_boundary',array['unobserved','cli_flush','stdio_flush','http_body']::text[]),
      ('graph_failure_phase','graph',e.properties->>'graph_failure_phase',array['parse','prepare','discover','open','capture','detect','extract','commit','post_commit','query','snapshot','analysis','render','artifact_write','output_write','output_flush','registration','bind','protocol','admission','worker','shutdown']::text[]),
      ('graph_failure_kind','graph',e.properties->>'graph_failure_kind',array['invalid_input','missing_index','not_found','permission','io','store_open','store_busy','invalid_store','unsupported_store','concurrent_change','endpoint_not_found','endpoint_ambiguous','work_limit','response_limit','input_rejected','unknown_project','capacity','worker','serialize','broken_pipe','authentication','protocol','unknown']::text[]),
      ('graph_query_path','graph',e.properties->>'graph_query_path',array['found','not_found_within_scope','incomplete']::text[]),
      ('graph_index_disposition','graph',e.properties->>'graph_index_disposition',array['no_op','committed']::text[]),
      ('graph_analysis_algorithm','graph',e.properties->>'graph_analysis_algorithm',array['leiden','louvain']::text[]),
      ('graph_analysis_convergence','graph',e.properties->>'graph_analysis_convergence',array['converged','not_converged','unknown']::text[]),
      ('graph_artifact_format','graph',e.properties->>'graph_artifact_format',array['snapshot_json','graphify_json','graph_ml','cypher','mermaid','svg','html','markdown','canvas','callflow_html','tree_html','wiki','obsidian']::text[]),
      ('sift_entry','summary',e.properties->>'sift_entry',array['mcp','direct','completion_hook','pre_hook','json_protocol','pi_session_v1','pi_session_v2']::text[]),
      ('sift_terminal','summary',e.properties->>'sift_terminal',array['invocation','protocol_request','protocol_session']::text[]),
      ('sift_outcome','summary',e.properties->>'sift_outcome',array['success','skipped','fail_open','failure']::text[]),
      ('sift_delivery','summary',e.properties->>'sift_delivery',array['not_applicable','not_attempted','flushed','unchanged','failed']::text[]),
      ('sift_skip','summary',e.properties->>'sift_skip',array['explicit_raw','disabled','excluded','settings_unavailable','interactive','small','binary','streaming_deadline','capture_limit','envelope_limit','unsupported_host','unsupported_tool','unsupported_event','unsupported_shell','unsupported_syntax','unsupported_metadata','malformed_input','already_wrapped','tokenizer_unavailable','not_smaller','no_selection']::text[]),
      ('sift_failure_phase','summary',e.properties->>'sift_failure_phase',array['arguments','settings','input','process_setup','spawn','child_read','child_wait','codec','render','protocol','output','other']::text[]),
      ('sift_failure_kind','summary',e.properties->>'sift_failure_kind',array['invalid_input','not_found','permission_denied','broken_pipe','interrupted','timed_out','io','tokenizer','other']::text[]),
      ('sift_child','summary',e.properties->>'sift_child',array['not_applicable','unknown','exited_zero','exited_nonzero','signalled','cancelled','spawn_not_found','spawn_denied','spawn_failed']::text[]),
      ('sift_missingness','summary',e.properties->>'sift_missingness',array['inherited','streaming','small','binary','view_not_tokenized','tokenizer_unavailable','incomplete','not_applicable','unknown']::text[]),
      ('remote_page_count_bucket','remote',e.properties->>'remote_page_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('remote_limit_count_bucket','remote',e.properties->>'remote_limit_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('remote_coverage_lag_count_bucket','remote',e.properties->>'remote_coverage_lag_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('remote_client_limited','remote',e.properties->>'remote_client_limited',array['true','false']::text[]),
      ('remote_complete','remote',e.properties->>'remote_complete',array['true','false']::text[]),
      ('remote_exhaustive','remote',e.properties->>'remote_exhaustive',array['true','false']::text[]),
      ('remote_has_more','remote',e.properties->>'remote_has_more',array['true','false']::text[]),
      ('remote_response_limited','remote',e.properties->>'remote_response_limited',array['true','false']::text[]),
      ('remote_query_duration_bucket','remote',e.properties->>'remote_query_duration_bucket',array['lt_1ms','1ms-5ms','5ms-10ms','10ms-25ms','25ms-50ms','50ms-100ms','100ms-250ms','250ms-1s','1s-5s','5s-30s','30s-2m','2m-10m','10m-1h','1h+']::text[]),
      ('sift_provider_input_tokens_measured_count_bucket','summary',e.properties->>'sift_provider_input_tokens_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_input_tokens_bucket','summary',e.properties->>'sift_provider_input_tokens_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_output_tokens_measured_count_bucket','summary',e.properties->>'sift_provider_output_tokens_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_output_tokens_bucket','summary',e.properties->>'sift_provider_output_tokens_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_semantic_observed_count_bucket','summary',e.properties->>'sift_semantic_observed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_semantic_requests_count_bucket','summary',e.properties->>'sift_semantic_requests_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_semantic_cache_hits_count_bucket','summary',e.properties->>'sift_semantic_cache_hits_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_measured_count_bucket','summary',e.properties->>'sift_provider_latency_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_0_count_bucket','summary',e.properties->>'sift_provider_latency_0_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_1_count_bucket','summary',e.properties->>'sift_provider_latency_1_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_2_count_bucket','summary',e.properties->>'sift_provider_latency_2_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_3_count_bucket','summary',e.properties->>'sift_provider_latency_3_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_4_count_bucket','summary',e.properties->>'sift_provider_latency_4_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_5_count_bucket','summary',e.properties->>'sift_provider_latency_5_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_6_count_bucket','summary',e.properties->>'sift_provider_latency_6_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_7_count_bucket','summary',e.properties->>'sift_provider_latency_7_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_8_count_bucket','summary',e.properties->>'sift_provider_latency_8_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_9_count_bucket','summary',e.properties->>'sift_provider_latency_9_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_10_count_bucket','summary',e.properties->>'sift_provider_latency_10_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_11_count_bucket','summary',e.properties->>'sift_provider_latency_11_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_12_count_bucket','summary',e.properties->>'sift_provider_latency_12_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sift_provider_latency_13_count_bucket','summary',e.properties->>'sift_provider_latency_13_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('observed_count_bucket','server_summary',e.properties->>'observed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('failed_count_bucket','server_summary',e.properties->>'failed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_measured_count_bucket','server_summary',e.properties->>'latency_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_0_count_bucket','server_summary',e.properties->>'latency_0_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_1_count_bucket','server_summary',e.properties->>'latency_1_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_2_count_bucket','server_summary',e.properties->>'latency_2_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_3_count_bucket','server_summary',e.properties->>'latency_3_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_4_count_bucket','server_summary',e.properties->>'latency_4_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_5_count_bucket','server_summary',e.properties->>'latency_5_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_6_count_bucket','server_summary',e.properties->>'latency_6_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_7_count_bucket','server_summary',e.properties->>'latency_7_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_8_count_bucket','server_summary',e.properties->>'latency_8_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_9_count_bucket','server_summary',e.properties->>'latency_9_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_10_count_bucket','server_summary',e.properties->>'latency_10_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_11_count_bucket','server_summary',e.properties->>'latency_11_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_12_count_bucket','server_summary',e.properties->>'latency_12_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_13_count_bucket','server_summary',e.properties->>'latency_13_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('collection_limited','server_summary',e.properties->>'collection_limited',array['true','false']::text[]),
      ('observed_count_bucket','sharing_summary',e.properties->>'observed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('failed_count_bucket','sharing_summary',e.properties->>'failed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_measured_count_bucket','sharing_summary',e.properties->>'latency_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_0_count_bucket','sharing_summary',e.properties->>'latency_0_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_1_count_bucket','sharing_summary',e.properties->>'latency_1_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_2_count_bucket','sharing_summary',e.properties->>'latency_2_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_3_count_bucket','sharing_summary',e.properties->>'latency_3_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_4_count_bucket','sharing_summary',e.properties->>'latency_4_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_5_count_bucket','sharing_summary',e.properties->>'latency_5_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_6_count_bucket','sharing_summary',e.properties->>'latency_6_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_7_count_bucket','sharing_summary',e.properties->>'latency_7_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_8_count_bucket','sharing_summary',e.properties->>'latency_8_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_9_count_bucket','sharing_summary',e.properties->>'latency_9_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_10_count_bucket','sharing_summary',e.properties->>'latency_10_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_11_count_bucket','sharing_summary',e.properties->>'latency_11_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_12_count_bucket','sharing_summary',e.properties->>'latency_12_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('latency_13_count_bucket','sharing_summary',e.properties->>'latency_13_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('collection_limited','sharing_summary',e.properties->>'collection_limited',array['true','false']::text[]),
      ('handoff_complete_count_bucket','server_summary',e.properties->>'handoff_complete_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('handoff_failed_count_bucket','server_summary',e.properties->>'handoff_failed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('handoff_unknown_count_bucket','server_summary',e.properties->>'handoff_unknown_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_observed_count_bucket','server_summary',e.properties->>'execution_observed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_failed_count_bucket','server_summary',e.properties->>'execution_failed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_observed_count_bucket','server_summary',e.properties->>'read_observed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_returned_count_bucket','server_summary',e.properties->>'read_returned_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_nonempty_count_bucket','server_summary',e.properties->>'read_nonempty_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_continuation_requested_count_bucket','server_summary',e.properties->>'read_continuation_requested_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('replayed_count_bucket','server_summary',e.properties->>'replayed_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('index_activated_count_bucket','server_summary',e.properties->>'index_activated_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_bytes_measured_count_bucket','server_summary',e.properties->>'read_bytes_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_bytes_bucket','server_summary',e.properties->>'read_bytes_bucket',array['0','lt_100kb','100kb-1mb','1mb-10mb','10mb-100mb','100mb-1gb','1gb-2gb','2gb-5gb','5gb-10gb','10gb-25gb','25gb-50gb','50gb-100gb','100gb+']::text[]),
      ('read_has_more_measured_count_bucket','server_summary',e.properties->>'read_has_more_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_has_more_bucket','server_summary',e.properties->>'read_has_more_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_complete_measured_count_bucket','server_summary',e.properties->>'read_complete_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_complete_bucket','server_summary',e.properties->>'read_complete_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_exhaustive_measured_count_bucket','server_summary',e.properties->>'read_exhaustive_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_exhaustive_bucket','server_summary',e.properties->>'read_exhaustive_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_response_limited_measured_count_bucket','server_summary',e.properties->>'read_response_limited_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_response_limited_bucket','server_summary',e.properties->>'read_response_limited_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_snippets_truncated_measured_count_bucket','server_summary',e.properties->>'read_snippets_truncated_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_snippets_truncated_bucket','server_summary',e.properties->>'read_snippets_truncated_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_coverage_lag_measured_count_bucket','server_summary',e.properties->>'read_coverage_lag_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_coverage_lag_bucket','server_summary',e.properties->>'read_coverage_lag_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('publication_bytes_measured_count_bucket','server_summary',e.properties->>'publication_bytes_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('publication_bytes_bucket','server_summary',e.properties->>'publication_bytes_bucket',array['0','lt_100kb','100kb-1mb','1mb-10mb','10mb-100mb','100mb-1gb','1gb-2gb','2gb-5gb','5gb-10gb','10gb-25gb','25gb-50gb','50gb-100gb','100gb+']::text[]),
      ('publication_records_measured_count_bucket','server_summary',e.properties->>'publication_records_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('publication_records_bucket','server_summary',e.properties->>'publication_records_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('index_processed_operations_measured_count_bucket','server_summary',e.properties->>'index_processed_operations_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('index_processed_operations_bucket','server_summary',e.properties->>'index_processed_operations_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('index_records_measured_count_bucket','server_summary',e.properties->>'index_records_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('index_records_bucket','server_summary',e.properties->>'index_records_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('index_bytes_measured_count_bucket','server_summary',e.properties->>'index_bytes_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('index_bytes_bucket','server_summary',e.properties->>'index_bytes_bucket',array['0','lt_100kb','100kb-1mb','1mb-10mb','10mb-100mb','100mb-1gb','1gb-2gb','2gb-5gb','5gb-10gb','10gb-25gb','25gb-50gb','50gb-100gb','100gb+']::text[]),
      ('index_coverage_lag_measured_count_bucket','server_summary',e.properties->>'index_coverage_lag_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('index_coverage_lag_bucket','server_summary',e.properties->>'index_coverage_lag_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('index_reads_available_measured_count_bucket','server_summary',e.properties->>'index_reads_available_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('index_reads_available_bucket','server_summary',e.properties->>'index_reads_available_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_measured_count_bucket','server_summary',e.properties->>'execution_latency_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_0_count_bucket','server_summary',e.properties->>'execution_latency_0_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_1_count_bucket','server_summary',e.properties->>'execution_latency_1_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_2_count_bucket','server_summary',e.properties->>'execution_latency_2_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_3_count_bucket','server_summary',e.properties->>'execution_latency_3_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_4_count_bucket','server_summary',e.properties->>'execution_latency_4_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_5_count_bucket','server_summary',e.properties->>'execution_latency_5_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_6_count_bucket','server_summary',e.properties->>'execution_latency_6_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_7_count_bucket','server_summary',e.properties->>'execution_latency_7_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_8_count_bucket','server_summary',e.properties->>'execution_latency_8_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_9_count_bucket','server_summary',e.properties->>'execution_latency_9_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_10_count_bucket','server_summary',e.properties->>'execution_latency_10_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_11_count_bucket','server_summary',e.properties->>'execution_latency_11_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_12_count_bucket','server_summary',e.properties->>'execution_latency_12_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('execution_latency_13_count_bucket','server_summary',e.properties->>'execution_latency_13_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_measured_count_bucket','server_summary',e.properties->>'read_query_latency_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_0_count_bucket','server_summary',e.properties->>'read_query_latency_0_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_1_count_bucket','server_summary',e.properties->>'read_query_latency_1_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_2_count_bucket','server_summary',e.properties->>'read_query_latency_2_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_3_count_bucket','server_summary',e.properties->>'read_query_latency_3_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_4_count_bucket','server_summary',e.properties->>'read_query_latency_4_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_5_count_bucket','server_summary',e.properties->>'read_query_latency_5_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_6_count_bucket','server_summary',e.properties->>'read_query_latency_6_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_7_count_bucket','server_summary',e.properties->>'read_query_latency_7_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_8_count_bucket','server_summary',e.properties->>'read_query_latency_8_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_9_count_bucket','server_summary',e.properties->>'read_query_latency_9_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_10_count_bucket','server_summary',e.properties->>'read_query_latency_10_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_11_count_bucket','server_summary',e.properties->>'read_query_latency_11_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_12_count_bucket','server_summary',e.properties->>'read_query_latency_12_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('read_query_latency_13_count_bucket','server_summary',e.properties->>'read_query_latency_13_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('response_bytes_measured_count_bucket','server_summary',e.properties->>'response_bytes_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('response_bytes_bucket','server_summary',e.properties->>'response_bytes_bucket',array['0','lt_100kb','100kb-1mb','1mb-10mb','10mb-100mb','100mb-1gb','1gb-2gb','2gb-5gb','5gb-10gb','10gb-25gb','25gb-50gb','50gb-100gb','100gb+']::text[]),
      ('upload_bytes_bucket','server_summary',e.properties->>'upload_bytes_bucket',array['0','lt_100kb','100kb-1mb','1mb-10mb','10mb-100mb','100mb-1gb','1gb-2gb','2gb-5gb','5gb-10gb','10gb-25gb','25gb-50gb','50gb-100gb','100gb+']::text[]),
      ('publication_kind','server_summary',e.properties->>'publication_kind',array['published','withdrawn','removed','cancelled','already_accepted']::text[]),
      ('selection_count_bucket','sharing_summary',e.properties->>'selection_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('recovered_receipts_count_bucket','sharing_summary',e.properties->>'recovered_receipts_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('already_accepted_count_bucket','sharing_summary',e.properties->>'already_accepted_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('progress_after_failure_count_bucket','sharing_summary',e.properties->>'progress_after_failure_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sharing_bytes_measured_count_bucket','sharing_summary',e.properties->>'sharing_bytes_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sharing_bytes_bucket','sharing_summary',e.properties->>'sharing_bytes_bucket',array['0','lt_100kb','100kb-1mb','1mb-10mb','10mb-100mb','100mb-1gb','1gb-2gb','2gb-5gb','5gb-10gb','10gb-25gb','25gb-50gb','50gb-100gb','100gb+']::text[]),
      ('sharing_records_measured_count_bucket','sharing_summary',e.properties->>'sharing_records_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('sharing_records_bucket','sharing_summary',e.properties->>'sharing_records_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_attempts_measured_count_bucket','sharing_summary',e.properties->>'retry_attempts_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_attempts_bucket','sharing_summary',e.properties->>'retry_attempts_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_measured_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_measured_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_0_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_0_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_1_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_1_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_2_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_2_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_3_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_3_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_4_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_4_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_5_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_5_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_6_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_6_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_7_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_7_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_8_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_8_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_9_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_9_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_10_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_10_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_11_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_11_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_12_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_12_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('retry_delay_latency_13_count_bucket','sharing_summary',e.properties->>'retry_delay_latency_13_count_bucket',array['0','1','2-5','6-20','21-100','101-1k','1k-10k','10k-100k','100k-1m','1m+']::text[]),
      ('selection_complete','sharing_summary',e.properties->>'selection_complete',array['true','false']::text[])
    ) v(dimension,kind,observed_bucket,vocabulary)
    where (v.kind='terminal' and e.count_unit='terminal_event')
      or (v.kind='search' and e.operation='search')
      or (v.kind='server' and e.operation='server_request')
      or (v.kind='summary' and e.operation='sift_summary')
      or (v.kind='graph' and e.operation='graph')
      or (v.kind='remote' and e.operation='remote')
      or (v.kind='server_summary' and e.operation='server_summary')
      or (v.kind='sharing_summary' and e.operation='sharing_summary')
      or (v.kind='runtime' and e.count_unit='runtime_grain')
  )
  select case when p_environment='staging' then 'synthetic_qualification:' else 'production:' end || d.population,
    d.product,d.app_version,d.surface,d.operation,d.action,d.sift_host,d.sift_mode,d.cohort,d.count_unit,
    d.dimension,legal.bucket,count(*)::bigint,
    count(*) filter(where d.observed_bucket=any(d.vocabulary))::bigint,
    count(*) filter(where d.observed_bucket=legal.bucket)::bigint
  from projected d cross join lateral unnest(d.vocabulary) legal(bucket)
  group by d.population,d.product,d.app_version,d.surface,d.operation,d.action,d.sift_host,d.sift_mode,
    d.cohort,d.count_unit,d.dimension,legal.bucket;
end
$function$;

alter function ctx.analytics_product_health_window(text,timestamptz,timestamptz,text[]) owner to ctx_migration;
revoke all on function ctx.analytics_product_health_window(text,timestamptz,timestamptz,text[]) from public,ctx_analytics_readonly,ctx_control_plane,
  ctx_telemetry_ingest,ctx_telemetry_retention,ctx_product_health_readonly;
grant execute on function ctx.analytics_product_health_window(text,timestamptz,timestamptz,text[]) to ctx_analytics_readonly;
comment on function ctx.analytics_product_health_window(text,timestamptz,timestamptz,text[]) is
  'Restricted 24h occurrence window. Staging is synthetic qualification only. Counts describe observed profiles/roots, not machines. Summary receipts are not invocation counts; bucket centers cannot recover totals/rates. Missing measurements remain unobserved. Effective overrides and late commits can change results; no completeness watermark.';
alter function ctx.analytics_product_measurements_window(text,timestamptz,timestamptz,text[]) owner to ctx_migration;
revoke all on function ctx.analytics_product_measurements_window(text,timestamptz,timestamptz,text[]) from public,ctx_analytics_readonly,ctx_control_plane,
  ctx_telemetry_ingest,ctx_telemetry_retention,ctx_product_health_readonly;
grant execute on function ctx.analytics_product_measurements_window(text,timestamptz,timestamptz,text[]) to ctx_analytics_readonly;
comment on function ctx.analytics_product_measurements_window(text,timestamptz,timestamptz,text[]) is
  'Restricted 24h occurrence window. Staging is synthetic qualification only. Counts describe observed profiles/roots, not machines. Summary receipts are not invocation counts; bucket centers cannot recover totals/rates. Missing measurements remain unobserved. Effective overrides and late commits can change results; no completeness watermark.';

commit;
