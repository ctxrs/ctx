import { readFileSync } from "node:fs";
import { expect, test } from "vitest";
import { NeonTelemetryDatabase } from "../src/database";
import { createTelemetryWorker } from "../src/worker";
import { BASE_SCHEMA, scalar, sql, sqlFileAs, startPostgres } from "./support/postgres.mjs";
import { CANONICAL_TELEMETRY_SCHEMA } from "./support/canonical-telemetry.mjs";
import { postgresQueryClient } from "./support/postgres-query-client.mjs";
import { ENV, NOW, jsonRequest, operationEvent, v1Batch } from "./worker-test-fixtures";
const load=(name)=>JSON.parse(readFileSync(new URL(`../../../contracts/telemetry-v1/fixtures/${name}.valid.json`,import.meta.url),"utf8"));
const quote=(v)=>`'${v.replaceAll("'","''")}'`;
const from="2026-07-22T00:00:00Z",until="2026-07-23T00:00:00Z";
const call=(name,environment="staging",versions="null",start=quote(from),end=quote(until))=>`ctx.analytics_product_${name}_window(${quote(environment)},${start},${end},${versions})`;
function read(pg,expression,predicate="true") {
 const result=JSON.parse(scalar(pg,`begin read only;set local role ctx_analytics_readonly;set local statement_timeout='10s';
  select jsonb_build_object(
    'rows',coalesce(jsonb_agg(r) filter(where ${predicate}),'[]'::jsonb),
    'identity_field_exposed',coalesce(bool_or(to_jsonb(r)::text ~ 'client_profile_id_hash|data_root_id_hash|payload_fingerprint'),false)
  ) from ${expression} r;commit;`));
 // Inspect every returned row for privacy, even when only selected dimensions
 // are materialized for assertions. Keep the default bounded psql buffer.
 expect(result.identity_field_exposed).toBe(false);
 return result.rows;
}

test("product producer -> HTTP -> Queue -> PostgreSQL -> restricted windows",async()=>{
 const pg=startPostgres();
 try{
  sql(pg,BASE_SCHEMA);
  expect(()=>sqlFileAs(pg,"0054_product_telemetry_windows.sql","ctx_migration")).toThrow(/canonical telemetry/u);
  sqlFileAs(pg,"0044_privacy_safe_telemetry_history.sql","ctx_migration");
  sqlFileAs(pg,"0044a_privacy_safe_telemetry_event_contract.sql","neondb_owner");
  sql(pg,CANONICAL_TELEMETRY_SCHEMA);
  expect(()=>sqlFileAs(pg,"0054_product_telemetry_windows.sql","neondb_owner")).toThrow(/ctx_migration/u);
  sqlFileAs(pg,"0054_product_telemetry_windows.sql","ctx_migration");
  sqlFileAs(pg,"0054_product_telemetry_windows.sql","ctx_migration");
  sql(pg,"grant select,insert on ctx.telemetry_event to ctx_telemetry_ingest");
  const events=[...["graph_completed","sift_summary","server_summary","sharing_summary","remote_completed","product_runtime"].map(load),operationEvent()].map((e,i)=>({...e,event_id:`16000000-0000-4000-8000-${String(i+1).padStart(12,"0")}`}));
  const bodies=[];const database=new NeonTelemetryDatabase(postgresQueryClient(pg));let databaseCalls=0;
  const worker=createTelemetryWorker({now:()=>NOW,createDatabaseClient(){databaseCalls++;return database;}});
  const env={...ENV,TELEMETRY_ANALYTICS_ENVIRONMENT:"staging",TELEMETRY_INGEST_QUEUE:{async sendBatch(entries){bodies.push(...Array.from(entries,({body})=>body));}}};
  const response=await worker.fetch(jsonRequest("/functions/v1/analytics",{...v1Batch(events),app_version:"2.2.4"}),env);
  expect(response.status,await response.text()).toBe(204);expect(databaseCalls).toBe(0);
  for(let replay=0;replay<2;replay++){
   let ack=0,retry=0;
   await worker.queue({queue:"ctx-telemetry-ingest-staging",messages:bodies.map((body)=>({body,attempts:1,ack(){ack++;},retry(){retry++;}}))},env);
   expect(ack).toBe(events.length);expect(retry).toBe(0);expect(scalar(pg,"select count(*) from ctx.telemetry_event")).toBe(String(events.length));
  }
  const health=read(pg,call("health"));
  expect(health.filter((r)=>r.aggregation_level==="product").map((r)=>r.product).sort()).toEqual(["graph","remote","search","server","server","sharing","sift"]);
  expect(health.every((r)=>r.population.startsWith("synthetic_qualification:"))).toBe(true);
  expect(read(pg,call("health","production"))).toEqual([]);
  expect(read(pg,call("health","staging","array[]::text[]"))).toEqual([]);
  expect(read(pg,call("health","staging","array['missing']"))).toEqual([]);
  const sift=health.find((r)=>r.operation==="sift_summary");
  expect(sift).toMatchObject({receipt_count:1,observed_samples_bucket:"2-5",complete_measurements_bucket:"1",unmeasured_invocations_bucket:"1",measured_savings_summary_count:1,cohort:{sift_outcome:"fail_open",sift_child:"exited_nonzero"}});
  const metrics=read(pg,call("measurements"),`
    (r.operation in ('graph','search') and r.dimension='native_total_duration_bucket')
    or (r.operation='sift_summary' and r.dimension='sift_tokens_savings_fraction_bucket')
    or r.operation='product_runtime'`);
  const latency=metrics.filter((r)=>r.operation==="graph"&&r.dimension==="native_total_duration_bucket");
  expect(latency).toHaveLength(14);expect(latency.reduce((n,r)=>n+r.bucket_count,0)).toBe(1);
  expect(latency.find((r)=>r.bucket==="10ms-25ms")).toMatchObject({eligible_count:1,measured_count:1,bucket_count:1});
  expect(metrics.find((r)=>r.dimension==="sift_tokens_savings_fraction_bucket"&&r.bucket==="increased")).toMatchObject({count_unit:"summary",bucket_count:1});
  expect(metrics.find((r)=>r.operation==="search"&&r.dimension==="native_total_duration_bucket")).toMatchObject({measured_count:0});
  expect(metrics.find((r)=>r.operation==="product_runtime")).toMatchObject({count_unit:"runtime_grain"});
  expect(JSON.stringify([health,metrics])).not.toMatch(/client_profile_id_hash|data_root_id_hash|payload_fingerprint/u);
  for(const name of ["health","measurements"]){
   for(const [start,end] of [["null",quote(until)],[quote(from),"'infinity'"],[quote(until),quote(from)],[quote(from),"'2026-07-23T00:00:01Z'"]])
    expect(()=>read(pg,call(name,"staging","null",start,end))).toThrow(/finite positive window/u);
   expect(()=>read(pg,call(name,"staging","array[null]::text[]"))).toThrow(/version filters/u);
   for(const role of ["ctx_telemetry_ingest","ctx_control_plane","ctx_product_health_readonly","ctx_telemetry_retention"])
    expect(scalar(pg,`select has_function_privilege('${role}','ctx.analytics_product_${name}_window(text,timestamptz,timestamptz,text[])','EXECUTE')`)).toBe("f");
  }
  expect(()=>sql(pg,"set role ctx_analytics_readonly;select * from ctx.telemetry_event")).toThrow(/permission denied/u);
  // Canonical overrides remain authoritative for synthetic staging and public reads.
  sql(pg,`insert into ctx.telemetry_traffic_class_overrides(override_key,event_id,traffic_class) values ('fixture-exclusion',${quote(events[0].event_id)},'internal')`);
  expect(read(pg,call("health")).some((r)=>r.product==="graph")).toBe(false);
  sql(pg,`update ctx.telemetry_event set analytics_environment='production',traffic_class='unclassified_public' where event_id=${quote(events[4].event_id)}`);
  expect(read(pg,call("health","production")).find((r)=>r.aggregation_level==="product")).toMatchObject({product:"remote",receipt_count:1,observed_profile_count:1,observed_data_root_count:1});
  const hostedOperations=["archive_verify","remote_pause","remote_resume","remote_status","remote_remove",
    "server_collection_create","server_user_create","server_user_list","server_user_credentials","server_user_credential","server_publications","server_status"];
  const hostedEvents=hostedOperations.map((operation,i)=>({...load("hosted_operation_completed"),operation,
    event_id:`17000000-0000-4000-8000-${String(i+1).padStart(12,"0")}`}));
  const before=bodies.length;
  const hostedResponse=await worker.fetch(jsonRequest("/functions/v1/analytics",{...v1Batch(hostedEvents),app_version:"2.2.4"}),env);
  expect(hostedResponse.status,await hostedResponse.text()).toBe(204);
  let hostedAck=0,hostedRetry=0;
  await worker.queue({queue:"ctx-telemetry-ingest-staging",messages:bodies.slice(before).map((body)=>({body,attempts:1,ack(){hostedAck++;},retry(){hostedRetry++;}}))},env);
  expect(hostedAck).toBe(hostedEvents.length);expect(hostedRetry).toBe(0);
  const hostedHealth=read(pg,call("health")).filter((r)=>hostedOperations.includes(r.operation));
  expect(hostedHealth.map((r)=>r.operation).sort()).toEqual([...hostedOperations].sort());
  expect(hostedHealth.find((r)=>r.operation==="archive_verify")).toMatchObject({product:"archive",receipt_count:1});
  expect(hostedHealth.filter((r)=>r.operation.startsWith("remote_")).every((r)=>r.product==="remote")).toBe(true);
 }finally{pg.cleanup();}
},30_000);
