import { DURATION_BUCKETS, SHARED_PROPERTY_KEYS, rejectUnknownKeys, requireBoolean, validateSharedProperties, type TelemetryScalar } from "./telemetry-contract";
import { PRODUCT_COUNTS, PRODUCT_BYTES, SERVER_OPERATIONS, vocabulary, invalid, noGreater, partition } from "./product-operation-contract";

type Props = Record<string,TelemetryScalar>;
const BASE = ["observation_window_bucket","collection_scope","collection_limited","observed_count_bucket","failed_count_bucket"];
const histogramKeys = (prefix:string) => [`${prefix}latency_measured_count_bucket`,...Array.from({length:14},(_,i)=>`${prefix}latency_${i}_count_bucket`)];
const totalKeys = (name:string) => [`${name}_measured_count_bucket`,`${name}_bucket`];
const READ_COUNTS = ["read_observed_count_bucket","read_returned_count_bucket","read_nonempty_count_bucket","read_continuation_requested_count_bucket"];
const READ_TOTALS = ["read_bytes","read_has_more","read_complete","read_exhaustive","read_response_limited","read_snippets_truncated","read_coverage_lag"];
const READ_KEYS = [...READ_COUNTS,...READ_TOTALS.flatMap(totalKeys),...histogramKeys("read_query_")];
const HANDOFF = ["handoff_complete_count_bucket","handoff_failed_count_bucket","handoff_unknown_count_bucket"];
const EXECUTION = ["execution_observed_count_bucket","execution_failed_count_bucket",...histogramKeys("execution_")];
const INDEX_TOTALS = ["index_processed_operations","index_records","index_bytes","index_coverage_lag","index_reads_available"];
const POPULATIONS:Record<string,string[]> = {
  request:["response_class","server_body_outcome",...totalKeys("response_bytes"),...HANDOFF,...EXECUTION,...READ_KEYS],execution:[],read:READ_KEYS,
  upload:["upload_bytes_bucket","replayed_count_bucket"],
  publication:["publication_kind","replayed_count_bucket",...totalKeys("publication_bytes"),...totalKeys("publication_records")],
  index:["index_activated_count_bucket",...INDEX_TOTALS.flatMap(totalKeys)],
};
const SHARING_COUNTS = ["selection_count_bucket","recovered_receipts_count_bucket","already_accepted_count_bucket","progress_after_failure_count_bucket"];
const SHARING_TOTALS = ["sharing_bytes","sharing_records","retry_attempts"];
const SHARING_FIELDS = ["sharing_operation","sharing_phase","sharing_tick","sharing_failure","sharing_selection","selection_complete",...SHARING_COUNTS,...SHARING_TOTALS.flatMap(totalKeys),...histogramKeys("retry_delay_")];
const SIFT_TOTALS=["sift_provider_input_tokens","sift_provider_output_tokens"];
export const SIFT_SEMANTIC_KEYS=["sift_semantic_mode","sift_semantic_disposition","sift_semantic_provider","sift_semantic_observed_count_bucket","sift_semantic_requests_count_bucket","sift_semantic_cache_hits_count_bucket",...SIFT_TOTALS.flatMap(totalKeys),...histogramKeys("sift_provider_")];
export const WINDOW_KEYS=[...BASE,...histogramKeys(""),"server_operation","server_population","server_failure",...Object.values(POPULATIONS).flat(),...SHARING_FIELDS];
// All cohort vocabularies are closed. These names contain no destination identity.
const SharingOperation = new Set(["worker_started", "worker_start_failed", "worker_stopped", "tick", "selection", "queued", "transfer", "accepted", "settled", "retry"]);
const SharingPhase = new Set(["settings", "admission", "capture", "begin_upload", "upload_status", "upload_chunk", "publish", "receipt", "settlement", "checkpoint"]);
const SharingTick = new Set(["disabled", "paused", "idle", "progress", "failed"]);
const SharingFailure = new Set(["configuration", "credentials", "state", "not_connected", "destination_changed", "policy_conflict", "policy_denied", "busy", "unavailable", "unauthorized", "forbidden", "not_found", "conflict", "staging_expired", "too_large", "rate_limited", "protocol", "http_rejected", "archive"]);
const SharingSelection = new Set(["selected", "unselected_source", "changed_profile", "outside_work_roots", "unknown_work_root", "backfill_excluded", "future_excluded", "needs_review"]);
const SiftSemanticMode = new Set(["off", "shadow", "select"]);
const SiftSemanticDisposition = new Set(["off", "project_not_allowed", "invalid_selection", "outside_project", "fallback", "rejected", "marginal", "not_smaller", "shadow_selected", "storage_unavailable", "selected"]);
const SiftProviderOutcome = new Set(["not_attempted", "missing_credential", "oversized", "unavailable", "http_failure", "invalid_response", "success", "memoized"]);
function count(p:Record<string,unknown>,out:Props,key:string):void {out[key]=vocabulary(p[key],PRODUCT_COUNTS);}
function histogram(p:Record<string,unknown>,out:Props,prefix:string,denominator:TelemetryScalar):void {
  const keys=histogramKeys(prefix);if (!keys.some((k)=>Object.hasOwn(p,k))) return;
  for (const key of keys) count(p,out,key);
  partition(keys.slice(1).map((k)=>out[k]),out[keys[0]!]);noGreater(out[keys[0]!],denominator);
}
function measured(p:Record<string,unknown>,out:Props,name:string,denominator:TelemetryScalar,boolean=false):void {
  const [sample,bucket]=totalKeys(name) as [string,string];
  if (!Object.hasOwn(p,sample) && !Object.hasOwn(p,bucket)) return;
  count(p,out,sample);if(out[sample]==="0")invalid();noGreater(out[sample],denominator);
  out[bucket]=vocabulary(p[bucket],name.endsWith("_bytes")?PRODUCT_BYTES:PRODUCT_COUNTS);
  if(boolean) noGreater(out[bucket],out[sample]);
}
function counts(p:Record<string,unknown>,out:Props,prefix:string,denominator?:TelemetryScalar):void {
  const n=`${prefix}observed_count_bucket`,f=`${prefix}failed_count_bucket`;
  count(p,out,n);count(p,out,f);if(out[n]==="0")invalid();noGreater(out[f],out[n]);
  if(denominator!==undefined)noGreater(out[n],denominator);histogram(p,out,prefix,out[n]!);
}
function read(p:Record<string,unknown>,out:Props,required:boolean):void {
  if (!required && !READ_KEYS.some((k)=>Object.hasOwn(p,k)))return;
  for(const key of READ_COUNTS)count(p,out,key);
  if(out.read_observed_count_bucket==="0")invalid();noGreater(out.read_observed_count_bucket,out.observed_count_bucket);
  if(required && out.read_observed_count_bucket!==out.observed_count_bucket)invalid();
  noGreater(out.read_nonempty_count_bucket,out.read_observed_count_bucket);noGreater(out.read_nonempty_count_bucket,out.read_returned_count_bucket);
  noGreater(out.read_continuation_requested_count_bucket,out.read_observed_count_bucket);
  for(const name of READ_TOTALS)measured(p,out,name,out.read_observed_count_bucket!,["read_has_more","read_complete","read_exhaustive","read_response_limited"].includes(name));
  histogram(p,out,"read_query_",out.read_observed_count_bucket!);
}
export function parseWindow(p:Record<string,unknown>,operation:string):Props {
  const server=operation==="server_summary";
  const population=server?vocabulary(p.server_population,new Set(Object.keys(POPULATIONS))):"";
  const keys=server?["server_operation","server_population","server_failure",...POPULATIONS[population]!]:SHARING_FIELDS;
  rejectUnknownKeys(p,new Set([...SHARED_PROPERTY_KEYS,...BASE,...histogramKeys(""),...keys]),"unknown_product_property");
  const out=validateSharedProperties(p);
  out.observation_window_bucket=vocabulary(p.observation_window_bucket,DURATION_BUCKETS);
  out.collection_scope=vocabulary(p.collection_scope,new Set(["best_effort_observed"]));
  out.collection_limited=requireBoolean(p.collection_limited,"invalid_product_value");counts(p,out,"");
  if(!server){sharing(p,out);return out;}
  out.server_operation=vocabulary(p.server_operation,SERVER_OPERATIONS);out.server_population=population;
  if(Object.hasOwn(p,"server_failure")) {
    out.server_failure=vocabulary(p.server_failure,new Set(["unauthorized", "forbidden", "not_found", "conflict", "cancelled", "expired", "invalid", "capacity", "request_capacity", "work_capacity", "timeout", "interrupted", "body", "body_too_large", "method", "unavailable", "index", "io", "catalog", "json", "core", "identity", "archive", "other"]));
    if(out.failed_count_bucket!==out.observed_count_bucket)invalid();
  }
  if(population==="request"){
    if(Object.hasOwn(p,"response_class"))out.response_class=vocabulary(p.response_class,new Set(["2xx","3xx","4xx","5xx","not_produced","unavailable","other"]));
    if(Object.hasOwn(p,"server_body_outcome"))out.server_body_outcome=vocabulary(p.server_body_outcome,new Set(["suppressed","complete","failed","dropped"]));
    measured(p,out,"response_bytes",out.observed_count_bucket!);
    for(const key of HANDOFF)count(p,out,key);partition(HANDOFF.map((k)=>out[k]),out.observed_count_bucket);
    if(EXECUTION.some((k)=>Object.hasOwn(p,k)))counts(p,out,"execution_",out.observed_count_bucket);
    read(p,out,false);
  }else if(population==="read")read(p,out,true);
  else if(population==="upload"||population==="publication"){
    count(p,out,"replayed_count_bucket");noGreater(out.replayed_count_bucket,out.observed_count_bucket);
    if(population==="upload")out.upload_bytes_bucket=vocabulary(p.upload_bytes_bucket,PRODUCT_BYTES);
    else{
      out.publication_kind=vocabulary(p.publication_kind,new Set(["published","withdrawn","removed","cancelled","already_accepted"]));
      for(const name of ["publication_bytes","publication_records"])measured(p,out,name,out.observed_count_bucket!);
    }
  }else if(population==="index"){
    count(p,out,"index_activated_count_bucket");noGreater(out.index_activated_count_bucket,out.observed_count_bucket);
    for(const name of INDEX_TOTALS)measured(p,out,name,out.observed_count_bucket!,name==="index_reads_available");
  }
  return out;
}
function sharing(p:Record<string,unknown>,out:Props):void {
  out.sharing_operation=vocabulary(p.sharing_operation,SharingOperation);
  for(const [key,values] of [["sharing_phase",SharingPhase],["sharing_tick",SharingTick],["sharing_failure",SharingFailure],["sharing_selection",SharingSelection]] as const)
    if(Object.hasOwn(p,key))out[key]=vocabulary(p[key],values);
  for(const key of SHARING_COUNTS)if(Object.hasOwn(p,key)){count(p,out,key);if(key!=="selection_count_bucket")noGreater(out[key],out.observed_count_bucket);}
  if(Object.hasOwn(p,"selection_complete"))out.selection_complete=requireBoolean(p.selection_complete,"invalid_product_value");
  for(const name of SHARING_TOTALS)measured(p,out,name,out.observed_count_bucket!);
  histogram(p,out,"retry_delay_",out.observed_count_bucket!);
  if(p.sharing_operation==="tick"){
    if(!Object.hasOwn(p,"sharing_phase")||!Object.hasOwn(p,"sharing_tick"))invalid();
    if((p.sharing_tick==="failed")!==Object.hasOwn(p,"sharing_failure"))invalid();
  }else if(Object.hasOwn(p,"sharing_tick")||Object.hasOwn(p,"progress_after_failure_count_bucket"))invalid();
  if(p.sharing_operation==="retry"){
    if(!Object.hasOwn(p,"sharing_phase")||!Object.hasOwn(p,"sharing_failure"))invalid();
  }else if(Object.hasOwn(p,"retry_attempts_bucket")||Object.hasOwn(p,"retry_delay_latency_measured_count_bucket"))invalid();
  if(p.sharing_operation==="selection"){
    if(!Object.hasOwn(p,"sharing_selection")||!Object.hasOwn(p,"selection_count_bucket")||!Object.hasOwn(p,"selection_complete"))invalid();
  }else if(Object.hasOwn(p,"sharing_selection")||Object.hasOwn(p,"selection_count_bucket")||Object.hasOwn(p,"selection_complete"))invalid();
  if(Object.hasOwn(p,"recovered_receipts_count_bucket")&&p.sharing_operation!=="accepted")invalid();
  if(Object.hasOwn(p,"already_accepted_count_bucket")&&p.sharing_operation!=="settled")invalid();
}
export function parseSiftSemantic(p:Record<string,unknown>,denominator:TelemetryScalar|undefined):Props {
  const out:Props={};if(!SIFT_SEMANTIC_KEYS.some((k)=>Object.hasOwn(p,k)))return out;
  out.sift_semantic_mode=vocabulary(p.sift_semantic_mode,SiftSemanticMode);
  out.sift_semantic_disposition=vocabulary(p.sift_semantic_disposition,SiftSemanticDisposition);
  out.sift_semantic_provider=vocabulary(p.sift_semantic_provider,SiftProviderOutcome);
  for(const key of ["sift_semantic_observed_count_bucket","sift_semantic_requests_count_bucket","sift_semantic_cache_hits_count_bucket"])count(p,out,key);
  if(out.sift_semantic_observed_count_bucket==="0")invalid();noGreater(out.sift_semantic_observed_count_bucket,denominator);
  noGreater(out.sift_semantic_requests_count_bucket,out.sift_semantic_observed_count_bucket);noGreater(out.sift_semantic_cache_hits_count_bucket,out.sift_semantic_observed_count_bucket);
  for(const name of SIFT_TOTALS)measured(p,out,name,out.sift_semantic_requests_count_bucket!);
  histogram(p,out,"sift_provider_",out.sift_semantic_requests_count_bucket!);
  return out;
}
