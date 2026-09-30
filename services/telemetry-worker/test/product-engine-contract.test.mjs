import { readFileSync } from "node:fs";
import { expect, test } from "vitest";
import { buildRejectionDiagnostics } from "../src/rejection-diagnostics";
import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";
import { decodeTelemetryQueueMessage, encodeTelemetryQueueMessage } from "../src/telemetry-queue";
import { ENV, INGEST_OPTIONS, jsonRequest, v1Batch, workerHarness } from "./worker-test-fixtures";

const load=(name)=>JSON.parse(readFileSync(new URL(`../../../contracts/telemetry-v1/fixtures/${name}.json`,import.meta.url),"utf8"));
const names=["graph_completed","sift_summary","server_summary","sharing_summary","remote_completed","product_runtime"];
const fixtures=Object.fromEntries(names.map((name)=>[name,load(`${name}.valid`)]));
const batch=(event)=>({...v1Batch([event]),app_version:"2.2.4"});

test.each(names)("producer fixture retains every typed fact through HTTP and Queue: %s",async(name)=>{
  const event=fixtures[name],h=workerHarness();
  const response=await h.worker.fetch(jsonRequest("/functions/v1/analytics",batch(event)),ENV);
  expect(response.status,await response.text()).toBe(204);
  const [message]=await h.queueMessages();
  expect(message.row.properties).toEqual({...event.properties,operation:event.operation,outcome:event.outcome});
  expect(message.row.surface).toBe(event.surface);
  expect(message.row.client_profile_id_hash).toMatch(/^[0-9a-f]{64}$/u);
  expect(message.row.provider_id).toBeNull();
  expect(h.queueBodies).toHaveLength(1);
  await expect(decodeTelemetryQueueMessage(await encodeTelemetryQueueMessage(message))).resolves.toEqual(message);
  expect(JSON.stringify(event).length).toBeLessThan(8*1024);
});

test("invalid fixtures are rejected by HTTP and by Queue revalidation",async()=>{
  for(const invalid of load("product_engine.invalid")){
    const event=structuredClone(fixtures[invalid.fixture]);
    Object.assign(event.properties,invalid.properties??{});
    for(const key of invalid.remove??[])delete event.properties[key];
    const rejected=workerHarness();
    expect((await rejected.worker.fetch(jsonRequest("/functions/v1/analytics",batch(event)),ENV)).status).toBe(422);
    expect(rejected.queueBodies).toHaveLength(0);
    const accepted=workerHarness();
    await accepted.worker.fetch(jsonRequest("/functions/v1/analytics",batch(fixtures[invalid.fixture])),ENV);
    const [message]=await accepted.queueMessages();
    message.row.properties={...event.properties,operation:event.operation,outcome:event.outcome};
    await expect(decodeTelemetryQueueMessage(await encodeTelemetryQueueMessage(message))).resolves.toBeNull();
  }
});

test("raw content, numeric facts and dynamic names never reach Queue or rejection diagnostics",async()=>{
  const canary="synthetic-private-command-query-path-token-tenant-canary";
  for(const name of names){
    const event=fixtures[name];
    for(const [key,value] of [["argv",[canary]],["error",canary],["path",canary],["query",canary],["tenant",canary],[canary,canary],["duration_ns",123],["token_count",123]]){
      const payload=batch({...event,properties:{...event.properties,[key]:value}}),h=workerHarness();
      const response=await h.worker.fetch(jsonRequest("/functions/v1/analytics",payload),ENV);
      expect(response.status).toBe(422);expect(h.queueBodies).toHaveLength(0);
      expect(await response.text()).not.toContain(canary);
      expect(JSON.stringify(h.observeRejection.mock.calls)).not.toContain(canary);
      expect(JSON.stringify(buildRejectionDiagnostics(payload,1024,"telemetry_batch"))).not.toContain(canary);
    }
  }
});

test("MCP Sift, optional sidecars, expansions and unavailable evidence remain distinct",async()=>{
  const event=structuredClone(fixtures.sift_summary);event.surface="mcp";event.properties.sift_entry="mcp";
  const plan=await buildTelemetryIngestPlan(batch(event),INGEST_OPTIONS);
  expect(plan.rows[0].properties.sift_outcome).toBe("fail_open");
  expect(plan.rows[0].properties.sift_child).toBe("exited_nonzero");
  expect(plan.rows[0].properties.sift_tokens_change).toBe("increased");
  const old=load("hosted_operation_failure.valid");
  expect((await buildTelemetryIngestPlan(batch(old),INGEST_OPTIONS)).rows).toHaveLength(1);
  const remote=structuredClone(fixtures.remote_completed);remote.properties.output_delivery="unknown";
  expect((await buildTelemetryIngestPlan(batch(remote),INGEST_OPTIONS)).rows[0].activity_class).toBe("product_activity");
});

test("every closed Graph action stays distinct and compound diagnostics fit the event ceiling",async()=>{
  const {GRAPH_OPERATIONS}=await import("../src/product-operation-contract");
  const {GRAPH_COUNTS,GRAPH_BOOLEANS,GRAPH_TIMINGS,GRAPH_BYTES,GRAPH_ENUMS}=await import("../src/product-fact-contract");
  const props={...fixtures.graph_completed.properties,execution_result:"failure",product_failure_stage:"execute",product_failure_class:"other"};
  for(const k of GRAPH_COUNTS)props[k]="100k-1m";
  for(const k of GRAPH_BOOLEANS)props[k]=false;
  for(const k of GRAPH_TIMINGS)props[k]="100ms-250ms";
  for(const k of GRAPH_BYTES)props[k]="50gb-100gb";
  for(const [k,values]of Object.entries(GRAPH_ENUMS))props[k]=values.reduce((a,b)=>a.length>b.length?a:b);
  for(const action of GRAPH_OPERATIONS){
    const event={...fixtures.graph_completed,outcome:"failure",properties:{...props,graph_operation:action}};
    expect(new TextEncoder().encode(JSON.stringify(event)).byteLength).toBeLessThan(8*1024);
    const plan=await buildTelemetryIngestPlan(batch(event),INGEST_OPTIONS);
    expect(plan.rows[0].properties.graph_operation).toBe(action);
  }
});

test("server correlated measurements preserve denominators and reject partial groups",async()=>{
  const event=structuredClone(fixtures.server_summary);
  Object.assign(event.properties,{
    execution_observed_count_bucket:"2-5",execution_failed_count_bucket:"1",
    read_observed_count_bucket:"2-5",read_returned_count_bucket:"6-20",read_nonempty_count_bucket:"2-5",read_continuation_requested_count_bucket:"1",
    read_complete_measured_count_bucket:"2-5",read_complete_bucket:"1",
    read_coverage_lag_measured_count_bucket:"1",read_coverage_lag_bucket:"6-20",
  });
  const plan=await buildTelemetryIngestPlan(batch(event),INGEST_OPTIONS);
  expect(plan.rows[0].properties).toMatchObject(event.properties);
  delete event.properties.read_complete_measured_count_bucket;
  await expect(buildTelemetryIngestPlan(batch(event),INGEST_OPTIONS)).rejects.toThrow();
});

test.each(load("sift_presented_pairs"))("paired serializer buckets preserve counts and expansion: $name",async(pair)=>{
  const event=structuredClone(fixtures.sift_summary);
  for(const key of Object.keys(event.properties))if(key.startsWith("sift_tokens_")||key.startsWith("sift_bytes_")||key==="sift_token_basis")delete event.properties[key];
  delete event.properties.latency_measured_count_bucket;
  for(const key of Object.keys(event.properties))if(key.startsWith("sift_latency_"))delete event.properties[key];
  Object.assign(event.properties,pair.expected,{
    observed_count_bucket:pair.samples+2<=5?"2-5":"6-20",
    complete_measurement_count_bucket:pair.expected[`sift_${pair.unit}_measured_count_bucket`],
  });
  const h=workerHarness();
  const response=await h.worker.fetch(jsonRequest("/functions/v1/analytics",batch(event)),ENV);
  expect(response.status,await response.text()).toBe(204);
  const [message]=await h.queueMessages();
  expect(message.row.properties).toMatchObject(pair.expected);
  await expect(decodeTelemetryQueueMessage(await encodeTelemetryQueueMessage(message))).resolves.toEqual(message);
  const measured=`sift_${pair.unit}_measured_count_bucket`;
  for(const bad of [0,"0","1m+"]){
    const broken=structuredClone(event);broken.properties[measured]=bad;
    await expect(buildTelemetryIngestPlan(batch(broken),INGEST_OPTIONS)).rejects.toThrow();
    const tampered=structuredClone(message);tampered.row.properties[measured]=bad;
    await expect(decodeTelemetryQueueMessage(await encodeTelemetryQueueMessage(tampered))).resolves.toBeNull();
  }
});


test("paired buckets reject provable direction and delta contradictions without center estimates",async()=>{
  for(const patch of [
    {sift_tokens_input_bucket:"1",sift_tokens_output_bucket:"6-20",sift_tokens_delta_bucket:"6-20",sift_tokens_change:"reduced",sift_tokens_savings_fraction_bucket:"50pct-75pct"},
    {sift_tokens_input_bucket:"6-20",sift_tokens_output_bucket:"1",sift_tokens_delta_bucket:"2-5",sift_tokens_change:"increased",sift_tokens_savings_fraction_bucket:"increased"},
    {sift_tokens_input_bucket:"1",sift_tokens_output_bucket:"1",sift_tokens_delta_bucket:"1",sift_tokens_change:"increased",sift_tokens_savings_fraction_bucket:"increased"},
    {sift_tokens_delta_bucket:"1m+"},
  ]){
    const event=structuredClone(fixtures.sift_summary);Object.assign(event.properties,patch);
    await expect(buildTelemetryIngestPlan(batch(event),INGEST_OPTIONS)).rejects.toThrow();
    const h=workerHarness();await h.worker.fetch(jsonRequest("/functions/v1/analytics",batch(fixtures.sift_summary)),ENV);
    const [message]=await h.queueMessages();Object.assign(message.row.properties,patch);
    await expect(decodeTelemetryQueueMessage(await encodeTelemetryQueueMessage(message))).resolves.toBeNull();
  }
});
