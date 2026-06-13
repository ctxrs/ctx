import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

type DesktopCorpusCase = {
  method:
    | "desktopGetConnection"
    | "desktopCheckAppUpdate"
    | "desktopApplyAppUpdate"
    | "desktopGetLastAppUpdateAttempt";
  command: string;
  call?: {
    channel?: string;
    download_id?: string;
  };
  args?: Record<string, unknown>;
  response: unknown;
};

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const corpusDir = path.join(__dirname, "testdata", "desktop-ipc");

const corpusFiles = fs
  .readdirSync(corpusDir)
  .filter((entry) => entry.endsWith(".json"))
  .sort();

const loadCase = (name: string): DesktopCorpusCase =>
  JSON.parse(fs.readFileSync(path.join(corpusDir, name), "utf8")) as DesktopCorpusCase;

describe("desktop IPC corpus", () => {
  beforeEach(() => {
    vi.resetModules();
    invokeMock.mockReset();
  });

  for (const file of corpusFiles) {
    it(`replays ${file}`, async () => {
      const testCase = loadCase(file);
      invokeMock.mockResolvedValue(testCase.response);

      const desktop = await import("./desktop");

      let result: unknown;
      switch (testCase.method) {
        case "desktopGetConnection":
          result = await desktop.desktopGetConnection();
          break;
        case "desktopCheckAppUpdate":
          result = await desktop.desktopCheckAppUpdate(
            testCase.call?.channel ? { channel: testCase.call.channel } : undefined,
          );
          break;
        case "desktopApplyAppUpdate":
          result = await desktop.desktopApplyAppUpdate({
            channel: testCase.call?.channel,
            downloadId: testCase.call?.download_id,
          });
          break;
        case "desktopGetLastAppUpdateAttempt":
          result = await desktop.desktopGetLastAppUpdateAttempt();
          break;
        default:
          throw new Error(`unsupported corpus method: ${(testCase as { method: string }).method}`);
      }

      expect(invokeMock).toHaveBeenCalledWith(testCase.command, testCase.args);
      expect(result).toEqual(testCase.response);
    });
  }
});
