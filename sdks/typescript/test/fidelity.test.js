import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createLocalAgentHistoryClient } from "../src/index.js";

const fixture = async (name) => JSON.parse(await readFile(new URL(`../../../../contracts/agent-history-v1/fixtures/cli/${name}`, import.meta.url), "utf8"));

test("current event and session preserve exact opaque JSON and presence", async () => {
  const event = await fixture("opaque-event.json");
  for (const value of [event.structured_content, null, undefined]) {
    const current = { ...event, structured_content: value };
    const raw = { event: current, events: [current], session: { ctx_session_id: "session-1" } };
    const client = createLocalAgentHistoryClient({ runner: async () => ({ stdout: JSON.stringify(raw) }) });
    for (const actual of [(await client.showEvent("event-1")).event.event, (await client.showSession("session-1")).session.events[0]]) {
      assert.deepEqual(actual.activity, event.activity);
      assert.equal(Object.hasOwn(actual, "structuredContent"), value !== undefined);
      assert.deepEqual(actual.structuredContent, value);
      assert.equal(actual.content.policyStatus, "selected");
    }
  }
});

test("literal queries follow every option", async () => {
  for (const query of ["--help", "--refresh=off", "-needle", "two words", "a'雪"]) {
    let args;
    const client = createLocalAgentHistoryClient({ runner: async (request) => { args = request.args; return { stdout: '{"results":[]}' }; } });
    await client.search(query, { refresh: "off", terms: ["--help"] });
    assert.deepEqual(args.slice(-2), ["--", query]);
    assert.ok(args.includes("--term=--help"));
  }
});

test("producer retry decisions and details survive CLI errors", async () => {
  for (const producer of await fixture("producer-errors.json")) {
    const client = createLocalAgentHistoryClient({ runner: async () => ({ exitCode: 1, stderr: JSON.stringify(producer) }) });
    await assert.rejects(client.showEvent("event-1"), error => {
      assert.equal(error.retryable, producer.retryable);
      assert.deepEqual(error.details.producerError, producer);
      return true;
    });
  }
  const client = createLocalAgentHistoryClient({ runner: async () => ({ exitCode: 1, stderr: "not JSON" }) });
  await assert.rejects(client.showEvent("event-1"), error => error.retryable === false && error.stderr === "not JSON");
});
