import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

const readId = (value: unknown): string => (typeof value === "string" ? value : "");

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const asArray = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);

test("workbench: stale cached events do not re-show running", async ({ page }) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1400, height: 900 });

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;

  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName,
  });

  // Choose Fake harness so the test doesn't depend on external agents.
  await selectHarnessBySearch(page, "fake", /fake/i);
  await expect(
    page.locator('button[title="Agents"] .wb-switcher-label').first(),
  ).toHaveText(/fake/i, { timeout: 20000 });

  const prompt = "stale-cache-spinner";
  await page.locator("textarea.wb-composer-textarea").first().fill(prompt);
  await page.getByRole("button", { name: "Send" }).click();

  expect(workspaceId).not.toBe("");

  let sessionIdValue = "";
  await expect
    .poll(
      async () => {
        const resp = await page.request.get(`/api/workspaces/${workspaceId}/active_snapshot`);
        if (!resp.ok()) return "";
        const data = asRecord(await resp.json());
        const task = asRecord(asArray(asRecord(data.active).tasks)[0]);
        const session = asRecord(asRecord(asArray(task.sessions)[0]).session).id;
        const primary = asRecord(task.task).primary_session_id;
        sessionIdValue = readId(session) || readId(primary);
        return sessionIdValue;
      },
      { timeout: 20000 },
    )
    .not.toBe("");
  expect(sessionIdValue).not.toBe("");

  await page.waitForFunction(async (sid) => {
    const resp = await fetch(`/api/sessions/${sid}/snapshot?include_events=1&limit=60`);
    if (!resp.ok) return false;
    const asRecordEval = (value: unknown): Record<string, unknown> => {
      if (!value || typeof value !== "object" || Array.isArray(value)) return {};
      return value as Record<string, unknown>;
    };
    const asArrayEval = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);
    const data = asRecordEval(await resp.json());
    const head = asRecordEval(data.head);
    const turns = asArrayEval(head.turns).map((turn) => asRecordEval(turn));
    const lastTurn = turns[turns.length - 1];
    const hasAssistant =
      asArrayEval(head.messages).some((message) => asRecordEval(message).role === "assistant");
    return Boolean(hasAssistant && lastTurn?.status === "completed");
  }, sessionIdValue, { timeout: 20000 });

  const snapshot = asRecord(await page.evaluate(async (sid) => {
    const resp = await fetch(`/api/sessions/${sid}/snapshot?include_events=1&limit=60`);
    if (!resp.ok) return null;
    return resp.json();
  }, sessionIdValue));
  expect(snapshot).not.toBeNull();
  const head = asRecord(snapshot.head);

  const doneLike = new Set(["done", "assistant_complete", "turn_finished", "turn_interrupted"]);
  const staleEvents = Array.isArray(head.events)
    ? head.events.filter((event) => !doneLike.has(String(asRecord(event).event_type ?? "")))
    : [];
  let nextEvents: unknown[] = staleEvents.length > 0 ? staleEvents : asArray(head.events);
  if (nextEvents.length === 0) {
    nextEvents = [
      {
        seq: Math.max(0, (head.last_event_seq ?? 1) - 1),
        id: "stale-event",
        session_id: sessionIdValue,
        run_id: null,
        turn_id: null,
        event_type: "assistant_message_inserted",
        payload_json: {},
        created_at: new Date().toISOString(),
      },
    ];
  }
  const staleHead = { ...head, events: nextEvents, last_event_seq: head.last_event_seq };

  await page.evaluate(
    async ({ sid, stale }) => {
      await new Promise<void>((resolve, reject) => {
        const req = indexedDB.open("ctx-ui", 1);
        req.onupgradeneeded = () => {
          const db = req.result;
          if (!db.objectStoreNames.contains("kv")) {
            db.createObjectStore("kv", { keyPath: "key" });
          }
        };
        req.onerror = () => reject(req.error ?? new Error("IndexedDB open failed"));
        req.onsuccess = () => {
          const db = req.result;
          const tx = db.transaction("kv", "readwrite");
          const store = tx.objectStore("kv");
          const stored = { v: 1, sessionId: sid, head: stale, updatedAtMs: Date.now() };
          store.put({ key: `wb.session_head.v1.${sid}`, value: stored, updatedAtMs: Date.now() });
          tx.oncomplete = () => resolve();
          tx.onerror = () => reject(tx.error ?? new Error("IndexedDB transaction failed"));
          tx.onabort = () => reject(tx.error ?? new Error("IndexedDB transaction aborted"));
        };
      });
    },
    { sid: sessionIdValue, stale: staleHead },
  );

  await page.reload();
  await expect(page.locator(".wb-task-spinner")).toHaveCount(0, { timeout: 5000 });
});
