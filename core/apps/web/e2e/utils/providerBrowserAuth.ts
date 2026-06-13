import { execFileSync, spawn, type ChildProcess } from "child_process";
import { cpSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "fs";
import { createServer } from "net";
import { tmpdir } from "os";
import path from "path";
import { chromium, type BrowserContext, type Locator, type Page } from "playwright/test";
import { parseBoolishString } from "../../src/utils/boolish";

type VisibleDomState = {
  url: string;
  title: string;
  bodyText: string;
  inputs: Array<{
    tag: string;
    type: string;
    name: string;
    autocomplete: string;
    inputmode: string;
    label: string;
    maxLength: number;
  }>;
  buttons: string[];
};

type DriveProgress = {
  clickedGoogleEntry: boolean;
  selectedAccount: boolean;
  usedEmail: boolean;
  usedPassword: boolean;
  grantedConsent: boolean;
};

type DriveStateArgs = {
  page: Page;
  state: VisibleDomState;
  progress: DriveProgress;
  hasEmailInput: boolean;
  hasPasswordInput: boolean;
  hasOtpInput: boolean;
  isGoogleHost: boolean;
};

type DriveStateResult =
  | { done: true; result: Record<string, unknown> }
  | { handled: true; page?: Page }
  | null;

type GoogleDriveOptions = {
  page: Page;
  authUrl: string;
  email: string;
  password: string;
  providerLabel: string;
  skipInitialGoto?: boolean;
  timeoutMs?: number;
  pollMs?: number;
  onState?: (args: DriveStateArgs) => Promise<DriveStateResult>;
};

export type ClaudeSetupTokenOptions = {
  context: BrowserContext;
  email: string;
  password: string;
  label?: string;
  timeoutMs?: number;
  pollMs?: number;
};

export type ClaudeSetupTokenSuccess = {
  authUrl: string;
  setupToken: string;
};

export type ClaudeManagedSetupTokenOptions = {
  context: BrowserContext;
  authUrl: string;
  email: string;
  password: string;
  openUrl?: ((authUrl: string) => Promise<Page>) | null;
  timeoutMs?: number;
  pollMs?: number;
};

export type BrowserAuthContextHandle = {
  context: BrowserContext;
  openUrl?: (authUrl: string) => Promise<Page>;
  dispose: () => Promise<void>;
};

type MacOsScreenTextMatch = {
  text: string;
  x: number;
  y: number;
};

const DEFAULT_TIMEOUT_MS = 5 * 60_000;
const DEFAULT_POLL_MS = 1_000;
const DEFAULT_CLAUDE_POST_GOOGLE_SETTLE_MS = 90_000;
const DEFAULT_AUTH_WINDOW_SIZE = { width: 1920, height: 1080 } as const;
const TEXT_LIMIT = 4_000;
const BROWSER_AUTH_DEBUG_ENABLED = process.env.CTX_E2E_PROVIDER_BROWSER_AUTH_DEBUG === "1";
const GOOGLE_CHROME_EXECUTABLE = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const CLAUDE_SETUP_TOKEN_SUCCESS_URL_PATTERN = /^https:\/\/platform\.claude\.com\/oauth\/code\/success\b/i;
const MACOS_VISION_FIND_TEXT_SWIFT = String.raw`
import Vision
import AppKit
import Foundation

struct Match: Encodable {
    let text: String
    let x: Int
    let y: Int
}

func normalize(_ raw: String) -> String {
    raw
        .lowercased()
        .replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression)
        .trimmingCharacters(in: .whitespacesAndNewlines)
}

let args = CommandLine.arguments
guard args.count >= 3 else {
    fputs("missing args\n", stderr)
    exit(1)
}

let imageUrl = URL(fileURLWithPath: args[1])
let target = normalize(args[2])
guard let image = NSImage(contentsOf: imageUrl),
      let tiff = image.tiffRepresentation,
      let bitmap = NSBitmapImageRep(data: tiff),
      let cgImage = bitmap.cgImage else {
    fputs("failed to load image\n", stderr)
    exit(1)
}

let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = false
let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
try handler.perform([request])

let width = Double(cgImage.width)
let height = Double(cgImage.height)
let scale = NSScreen.main?.backingScaleFactor ?? 1.0
var best: Match?
var bestRank = Int.max

for observation in request.results ?? [] {
    guard let candidate = observation.topCandidates(1).first?.string else {
        continue
    }
    let normalized = normalize(candidate)
    let isExact = normalized == target
    guard isExact else {
        continue
    }
    let rank = 0
    guard rank <= bestRank else {
        continue
    }
    let box = observation.boundingBox
    let centerX = ((box.origin.x + box.size.width / 2.0) * width) / scale
    let centerY = ((1.0 - box.origin.y - box.size.height / 2.0) * height) / scale
    best = Match(text: candidate, x: Int(centerX.rounded()), y: Int(centerY.rounded()))
    bestRank = rank
}

if let best {
    let data = try JSONEncoder().encode(best)
    FileHandle.standardOutput.write(data)
}
`;

const AUTH_INPUT_SELECTOR_GROUPS = Object.freeze({
  email: [
    "input[type='email']",
    "input[name='email']",
    "input[name='identifier']",
    "input[autocomplete='email']",
    "input[name*='email' i]",
    "input[aria-label*='email' i]",
    "input[placeholder*='email' i]",
  ],
  password: [
    "input[type='password']",
    "input[autocomplete='current-password']",
    "input[autocomplete='password']",
    "input[name*='password' i]",
    "input[aria-label*='password' i]",
  ],
});

const GOOGLE_INTERACTIVE_CHALLENGE_PATTERNS = Object.freeze([
  /captcha/i,
  /verify you are human/i,
  /not a robot/i,
]);

const GOOGLE_ADDITIONAL_VERIFICATION_PATTERNS = Object.freeze([
  /verify it'?s you/i,
  /confirm it'?s you/i,
  /check your phone/i,
  /tap yes/i,
  /use your phone/i,
  /choose how you want to sign in/i,
  /get a verification code/i,
  /enter a phone number/i,
  /2-step verification/i,
]);

const GOOGLE_ERROR_PATTERNS = Object.freeze([
  /couldn'?t sign you in/i,
  /wrong password/i,
  /incorrect password/i,
  /couldn'?t find your google account/i,
  /account disabled/i,
  /something went wrong/i,
]);

const CLAUDE_LOGIN_ERROR_PATTERNS = Object.freeze([
  /there was an error logging you in/i,
  /problem persists contact support/i,
]);

const CLAUDE_SETUP_TOKEN_PATTERN = /sk-ant-oat[A-Za-z0-9._-]+/;
const CLAUDE_SETUP_TOKEN_AUTH_URL_PATTERN = /https:\/\/claude\.ai\/oauth\/authorize[^\s\x00-\x1F\x7F]+/i;
const CLAUDE_SETUP_TOKEN_OSC_AUTH_URL_PATTERN = /\u001B]8;;(https:\/\/claude\.ai\/oauth\/authorize[^\u0007]+)\u0007/i;
const CLAUDE_SETUP_TOKEN_PTY_BRIDGE = String.raw`
import fcntl
import os
import pty
import select
import subprocess
import struct
import sys
import termios

argv = sys.argv[1:]
if not argv:
    raise SystemExit("missing claude setup-token command")

master_fd, slave_fd = pty.openpty()
fcntl.ioctl(slave_fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 400, 0, 0))
proc = subprocess.Popen(argv, stdin=slave_fd, stdout=slave_fd, stderr=slave_fd, close_fds=True)
os.close(slave_fd)

stdin_fd = sys.stdin.fileno()
stdout_fd = sys.stdout.fileno()
stdin_open = True

while True:
    read_fds = [master_fd]
    if stdin_open:
        read_fds.append(stdin_fd)
    ready, _, _ = select.select(read_fds, [], [], 0.1)

    if master_fd in ready:
        try:
            data = os.read(master_fd, 4096)
        except OSError:
            data = b""
        if data:
            os.write(stdout_fd, data)

    if stdin_open and stdin_fd in ready:
        try:
            data = os.read(stdin_fd, 4096)
        except OSError:
            data = b""
        if data:
            os.write(master_fd, data)
        else:
            stdin_open = False

    if proc.poll() is not None:
        while True:
            try:
                data = os.read(master_fd, 4096)
            except OSError:
                data = b""
            if not data:
                break
            os.write(stdout_fd, data)
        break

os.close(master_fd)
raise SystemExit(proc.wait())
`;

const parseBool = (value?: string): boolean => parseBoolishString(value) === true;

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const readString = (value: unknown): string => (typeof value === "string" ? value.trim() : "");

const escapeRegExp = (value: string): string => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

const normalizeUiText = (value: unknown): string => String(value ?? "").replace(/\s+/g, " ").trim().toLowerCase();

const waitMs = async (ms: number): Promise<void> => {
  await new Promise((resolve) => {
    setTimeout(resolve, Math.max(0, ms));
  });
};

let macOsVisionScriptPath: string | null = null;

const ensureMacOsVisionScriptPath = (): string => {
  if (macOsVisionScriptPath) {
    return macOsVisionScriptPath;
  }
  const scriptDir = mkdtempSync(path.join(tmpdir(), "ctx-claude-vision-"));
  const scriptPath = path.join(scriptDir, "findText.swift");
  writeFileSync(scriptPath, MACOS_VISION_FIND_TEXT_SWIFT);
  macOsVisionScriptPath = scriptPath;
  return scriptPath;
};

const runAppleScript = (...lines: string[]): string =>
  readString(execFileSync("osascript", lines.flatMap((line) => ["-e", line]), { encoding: "utf8" }));

const openAuthUrlInRealChrome = (authUrl: string): void => {
  execFileSync("open", ["-a", "Google Chrome", authUrl], { stdio: "ignore" });
};

const activateRealChrome = (): void => {
  runAppleScript('tell application "Google Chrome" to activate');
};

const readFrontChromeUrl = (): string => {
  try {
    return runAppleScript('tell application "Google Chrome" to get URL of active tab of front window');
  } catch {
    return "";
  }
};

const captureScreenToPath = (imagePath: string): void => {
  execFileSync("screencapture", ["-x", imagePath], { stdio: "ignore" });
};

const findMacOsScreenTextMatch = (imagePath: string, targetText: string): MacOsScreenTextMatch | null => {
  const scriptPath = ensureMacOsVisionScriptPath();
  const raw = readString(execFileSync("swift", [scriptPath, imagePath, targetText], { encoding: "utf8" }));
  if (!raw) {
    return null;
  }
  const parsed = asRecord(JSON.parse(raw));
  const text = readString(parsed.text);
  const x = Number(parsed.x);
  const y = Number(parsed.y);
  if (!text || !Number.isFinite(x) || !Number.isFinite(y)) {
    return null;
  }
  return {
    text,
    x: Math.round(x),
    y: Math.round(y),
  };
};

const clickMacOsScreenPoint = (x: number, y: number): void => {
  execFileSync("cliclick", [`c:${Math.round(x)},${Math.round(y)}`], { stdio: "ignore" });
};

const sanitizeAuthUrl = (raw: string): string => {
  const text = readString(raw);
  if (!text) return "";
  try {
    const parsed = new URL(text);
    return `${parsed.protocol}//${parsed.host}${parsed.pathname || "/"}`;
  } catch {
    return text.split("#", 1)[0]?.split("?", 1)[0]?.trim() ?? "";
  }
};

const parseClaudeSetupTokenRedirectUri = (rawUrl: string): string => {
  try {
    return readString(new URL(rawUrl).searchParams.get("redirect_uri"));
  } catch {
    return "";
  }
};

const isValidClaudeSetupTokenRedirectUri = (rawUrl: string): boolean => {
  const redirectUri = parseClaudeSetupTokenRedirectUri(rawUrl);
  if (!redirectUri) return false;
  try {
    const parsed = new URL(redirectUri);
    const hostname = parsed.hostname.toLowerCase();
    if (parsed.protocol !== "http:") return false;
    if (hostname !== "localhost" && hostname !== "127.0.0.1" && hostname !== "::1" && hostname !== "[::1]") {
      return false;
    }
    if (parsed.pathname !== "/callback") return false;
    return parsed.port.length > 0;
  } catch {
    return false;
  }
};

const stripTerminalControlSequences = (raw: string): string =>
  raw
    // OSC hyperlinks like ESC ] 8 ;; URL BEL ... ESC ] 8 ;; BEL
    .replace(/\u001B\][^\u0007]*(?:\u0007|\u001B\\)/g, "")
    .replace(/\u001B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])/g, "");

const redactClaudeSetupTokenOutput = (raw: string): string =>
  stripTerminalControlSequences(raw)
    .replace(CLAUDE_SETUP_TOKEN_PATTERN, "<redacted-setup-token>")
    .replace(/[A-Za-z0-9._-]{20,}#[A-Za-z0-9._-]{20,}/g, "<redacted-authorization-code>")
    .replace(CLAUDE_SETUP_TOKEN_AUTH_URL_PATTERN, "<redacted-auth-url>")
    .replace(/\s+/g, " ")
    .trim()
    .slice(-TEXT_LIMIT);

const collectAuthStateText = (state: VisibleDomState): string =>
  [state.title, state.bodyText, ...state.buttons].map((value) => readString(value)).join(" ").toLowerCase();

const findLastMatching = <T>(items: T[], predicate: (item: T) => boolean): T | undefined => {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const candidate = items[index];
    if (candidate !== undefined && predicate(candidate)) {
      return candidate;
    }
  }
  return undefined;
};

const stateMentions = (state: VisibleDomState, pattern: RegExp): boolean => pattern.test(collectAuthStateText(state));

const readStateHost = (state: VisibleDomState): string => {
  try {
    return new URL(readString(state.url)).host.toLowerCase();
  } catch {
    return "";
  }
};

const readBrowserAuthTimeoutMs = (fallbackMs: number): number => {
  const raw = Number(process.env.CTX_E2E_PROVIDER_BROWSER_AUTH_TIMEOUT_MS ?? "");
  if (!Number.isFinite(raw) || raw <= 0) return fallbackMs;
  return Math.floor(raw);
};

const readBrowserAuthTypingDelayMs = (): number => {
  const raw = Number(process.env.CTX_E2E_PROVIDER_BROWSER_AUTH_KEY_DELAY_MS ?? "");
  if (!Number.isFinite(raw) || raw <= 0) return 80;
  return Math.floor(raw);
};

const readClaudePostGoogleSettleMs = (): number => {
  const raw = Number(process.env.CTX_E2E_CLAUDE_POST_GOOGLE_SETTLE_MS ?? "");
  if (!Number.isFinite(raw) || raw <= 0) return DEFAULT_CLAUDE_POST_GOOGLE_SETTLE_MS;
  return Math.floor(raw);
};

const logBrowserAuthDebug = (providerLabel: string, message: string, details?: unknown): void => {
  if (!BROWSER_AUTH_DEBUG_ENABLED) return;
  if (details === undefined) {
    console.log(`[provider-browser-auth:${providerLabel}] ${message}`);
    return;
  }
  console.log(`[provider-browser-auth:${providerLabel}] ${message}: ${JSON.stringify(details)}`);
};

const chromeUserDataRoot = (): string =>
  path.join(process.env.HOME ?? "", "Library", "Application Support", "Google", "Chrome");

const findLocalChromeProfileForEmail = (email: string): string | null => {
  const normalizedEmail = readString(email).toLowerCase();
  if (!normalizedEmail) return null;
  const baseDir = chromeUserDataRoot();
  for (const candidate of ["Profile 1", "Profile 5", "Default", "Profile 2", "Profile 3", "Profile 4", "Profile 6"]) {
    const preferencesPath = path.join(baseDir, candidate, "Preferences");
    if (!existsSync(preferencesPath)) continue;
    try {
      const contents = readFileSync(preferencesPath, "utf8").toLowerCase();
      if (contents.includes(normalizedEmail)) {
        return candidate;
      }
    } catch {
      continue;
    }
  }
  return null;
};

const seedLocalChromeProfile = (userDataDir: string, email: string): string => {
  const baseDir = chromeUserDataRoot();
  const profileDirectory = findLocalChromeProfileForEmail(email);
  if (!profileDirectory) {
    throw new Error(`no local Chrome profile matched ${email}; set up a signed-in Chrome profile for the shared Google account first`);
  }

  const localStatePath = path.join(baseDir, "Local State");
  const firstRunPath = path.join(baseDir, "First Run");
  const sourceProfileDir = path.join(baseDir, profileDirectory);
  if (!existsSync(localStatePath) || !existsSync(sourceProfileDir)) {
    throw new Error(`local Chrome profile seed is incomplete for ${profileDirectory}`);
  }

  cpSync(localStatePath, path.join(userDataDir, "Local State"));
  if (existsSync(firstRunPath)) {
    cpSync(firstRunPath, path.join(userDataDir, "First Run"));
  } else {
    writeFileSync(path.join(userDataDir, "First Run"), "");
  }
  cpSync(sourceProfileDir, path.join(userDataDir, profileDirectory), { recursive: true });
  return profileDirectory;
};

const reserveTcpPort = async (): Promise<number> =>
  await new Promise<number>((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close(() => reject(new Error("failed to reserve local TCP port")));
        return;
      }
      const { port } = address;
      server.close((error) => {
        if (error) {
          reject(error);
          return;
        }
        resolve(port);
      });
    });
  });

const waitForChromeDevtools = async (port: number, timeoutMs: number): Promise<void> => {
  const deadline = Date.now() + timeoutMs;
  const url = `http://127.0.0.1:${port}/json/version`;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) {
        return;
      }
    } catch {
      // keep polling until timeout
    }
    await waitMs(250);
  }
  throw new Error(`timed out waiting for Chrome DevTools on port ${port}`);
};

const waitForChildProcessExit = async (child: ChildProcess, timeoutMs: number): Promise<void> =>
  await new Promise((resolve) => {
    if (child.exitCode !== null || child.signalCode !== null) {
      resolve();
      return;
    }
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.off("close", finish);
      child.off("exit", finish);
      resolve();
    };
    const timer = setTimeout(finish, Math.max(0, timeoutMs));
    child.once("close", finish);
    child.once("exit", finish);
  });

const terminateChildProcess = async (child: ChildProcess, timeoutMs: number): Promise<void> => {
  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
  }
  await waitForChildProcessExit(child, timeoutMs);
};

const removeDirWithRetries = async (targetDir: string, attempts = 5): Promise<void> => {
  let lastError: unknown = null;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    try {
      rmSync(targetDir, { force: true, recursive: true });
      return;
    } catch (error) {
      lastError = error;
      if (!(error instanceof Error) || !/ENOTEMPTY|EBUSY/i.test(error.message) || attempt === attempts - 1) {
        throw error;
      }
      await waitMs(250 * (attempt + 1));
    }
  }
  if (lastError) {
    throw lastError;
  }
};

const launchLocalChromeProfileContext = async (
  userDataDir: string,
  profileDirectory: string,
  useStealthishMode: boolean,
): Promise<{
  context: BrowserContext;
  browserProcess: ChildProcess;
  openUrl: (authUrl: string) => Promise<Page>;
}> => {
  if (!existsSync(GOOGLE_CHROME_EXECUTABLE)) {
    throw new Error(`Google Chrome executable not found at ${GOOGLE_CHROME_EXECUTABLE}`);
  }
  const devtoolsPort = await reserveTcpPort();
  const args = [
    `--user-data-dir=${userDataDir}`,
    `--profile-directory=${profileDirectory}`,
    `--remote-debugging-port=${devtoolsPort}`,
    `--window-size=${DEFAULT_AUTH_WINDOW_SIZE.width},${DEFAULT_AUTH_WINDOW_SIZE.height}`,
    "--new-window",
    ...(useStealthishMode ? ["--disable-blink-features=AutomationControlled"] : []),
    "about:blank",
  ];
  const browserProcess = spawn(GOOGLE_CHROME_EXECUTABLE, args, {
    stdio: "ignore",
    detached: false,
  });
  await waitForChromeDevtools(devtoolsPort, 15_000);
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${devtoolsPort}`);
  const context = browser.contexts()[0];
  if (!context) {
    await browser.close().catch(() => {});
    browserProcess.kill("SIGTERM");
    throw new Error("Chrome CDP connection did not expose a browser context");
  }
  const openUrl = async (authUrl: string): Promise<Page> => {
    const beforePages = new Map(
      context.pages().map((page) => {
        try {
          return [page, page.url()];
        } catch {
          return [page, ""];
        }
      }),
    );
    const opener = spawn(
      GOOGLE_CHROME_EXECUTABLE,
      [
        `--user-data-dir=${userDataDir}`,
        `--profile-directory=${profileDirectory}`,
        "--new-window",
        authUrl,
      ],
      {
        stdio: "ignore",
        detached: false,
      },
    );
    opener.unref();
    const startedAt = Date.now();
    while (Date.now() - startedAt <= 15_000) {
      for (const page of context.pages()) {
        if (page.isClosed()) continue;
        let currentUrl = "";
        try {
          currentUrl = page.url();
        } catch {
          continue;
        }
        if (!currentUrl || currentUrl === "about:blank") continue;
        const initialUrl = beforePages.get(page) ?? "";
        if (!beforePages.has(page) || initialUrl === "about:blank" || initialUrl !== currentUrl) {
          logBrowserAuthDebug("claude-setup-token", "chrome_open_url_page_ready", {
            pageUrl: currentUrl,
            redirectUri: parseClaudeSetupTokenRedirectUri(currentUrl),
          });
          return page;
        }
      }
      await waitMs(250);
    }
    throw new Error(`timed out waiting for Chrome to open auth URL: ${authUrl}`);
  };
  return { context, browserProcess, openUrl };
};

const applyStealthishInitScript = async (context: BrowserContext): Promise<void> => {
  await context.addInitScript(() => {
    Object.defineProperty(navigator, "webdriver", {
      configurable: true,
      get: () => undefined,
    });

    const chromeValue = (window as Window & { chrome?: object }).chrome;
    if (!chromeValue) {
      Object.defineProperty(window, "chrome", {
        configurable: true,
        value: { runtime: {} },
      });
    }

    if (navigator.permissions?.query) {
      const originalQuery = navigator.permissions.query.bind(navigator.permissions);
      navigator.permissions.query = ((parameters: PermissionDescriptor) => {
        if (parameters.name === "notifications") {
          return Promise.resolve({
            name: "notifications",
            onchange: null,
            state: Notification.permission,
            addEventListener() {},
            dispatchEvent() {
              return true;
            },
            removeEventListener() {},
          } as PermissionStatus);
        }
        return originalQuery(parameters);
      }) as typeof navigator.permissions.query;
    }
  });
};

export const createClaudeBrowserAuthContext = async (
  pageContext: BrowserContext,
): Promise<BrowserAuthContextHandle> => {
  const usePersistentContext = parseBool(process.env.CTX_E2E_PROVIDER_BROWSER_AUTH_PERSISTENT);
  const useStealthishMode = parseBool(process.env.CTX_E2E_PROVIDER_BROWSER_AUTH_STEALTH);

  if (!usePersistentContext) {
    if (useStealthishMode) {
      await applyStealthishInitScript(pageContext);
    }
    return {
      context: pageContext,
      openUrl: async (authUrl: string) => {
        const page = await pageContext.newPage();
        await page.goto(authUrl, { waitUntil: "domcontentloaded", timeout: DEFAULT_TIMEOUT_MS });
        return page;
      },
      dispose: async () => {},
    };
  }

  const userDataDir = mkdtempSync(path.join(tmpdir(), "ctx-claude-browser-auth-profile-"));
  const keepProfile = parseBool(process.env.CTX_E2E_PROVIDER_BROWSER_AUTH_KEEP_PROFILE);
  const preferredChannel = readString(process.env.CTX_E2E_PROVIDER_BROWSER_AUTH_CHANNEL)
    || (process.platform === "darwin" ? "chrome" : "");
  const useLocalGoogleProfile = parseBool(process.env.CTX_E2E_PROVIDER_BROWSER_AUTH_USE_LOCAL_GOOGLE_PROFILE);
  const googleEmail = readString(process.env.GOOGLE_TEST_EMAIL);
  const profileDirectory = useLocalGoogleProfile
    ? seedLocalChromeProfile(userDataDir, googleEmail)
    : "Default";
  if (useLocalGoogleProfile) {
    const { context, browserProcess, openUrl } = await launchLocalChromeProfileContext(
      userDataDir,
      profileDirectory,
      useStealthishMode,
    );
    if (useStealthishMode) {
      await applyStealthishInitScript(context);
    }
    return {
      context,
      openUrl,
      dispose: async () => {
        await context.browser()?.close().catch(() => {});
        await terminateChildProcess(browserProcess, 5_000);
        if (!keepProfile) {
          await removeDirWithRetries(userDataDir);
        }
      },
    };
  }

  const persistentContext = await chromium.launchPersistentContext(userDataDir, {
    channel: preferredChannel || undefined,
    headless: parseBool(process.env.CTX_E2E_PROVIDER_BROWSER_AUTH_HEADLESS),
    viewport: DEFAULT_AUTH_WINDOW_SIZE,
    screen: DEFAULT_AUTH_WINDOW_SIZE,
    args: [
      ...(useStealthishMode ? ["--disable-blink-features=AutomationControlled"] : []),
      `--profile-directory=${profileDirectory}`,
      `--window-size=${DEFAULT_AUTH_WINDOW_SIZE.width},${DEFAULT_AUTH_WINDOW_SIZE.height}`,
    ],
    ...(useStealthishMode ? { ignoreDefaultArgs: ["--enable-automation"] } : {}),
  });
  if (useStealthishMode) {
    await applyStealthishInitScript(persistentContext);
  }

  return {
    context: persistentContext,
    openUrl: async (authUrl: string) => {
      const page = await persistentContext.newPage();
      await page.goto(authUrl, { waitUntil: "domcontentloaded", timeout: DEFAULT_TIMEOUT_MS });
      return page;
    },
    dispose: async () => {
      await persistentContext.close().catch(() => {});
      if (!keepProfile) {
        rmSync(userDataDir, { force: true, recursive: true });
      }
    },
  };
};

const summarizeState = (state: VisibleDomState) => ({
  url: sanitizeAuthUrl(state.url),
  title: state.title,
  buttons: state.buttons.slice(0, 8),
  inputs: state.inputs.map((entry) => ({
    type: entry.type,
    name: entry.name,
    autocomplete: entry.autocomplete,
    label: entry.label,
  })),
});

const readVisibleDomState = async (page: Page): Promise<VisibleDomState> =>
  page.evaluate((textLimit) => {
    const normalize = (value: unknown) => String(value ?? "").replace(/\s+/g, " ").trim();
    const isVisible = (element: Element | null): element is HTMLElement => {
      if (!(element instanceof HTMLElement)) return false;
      const style = window.getComputedStyle(element);
      if (!style || style.display === "none" || style.visibility === "hidden") return false;
      const rect = element.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0;
    };
    const labelTextFor = (element: Element): string => {
      const values: string[] = [];
      if (element instanceof HTMLElement) {
        values.push(element.getAttribute("aria-label") || "");
        const labelledBy = element.getAttribute("aria-labelledby");
        if (labelledBy) {
          for (const id of labelledBy.split(/\s+/g)) {
            const ref = document.getElementById(id);
            if (ref) values.push(ref.textContent || "");
          }
        }
        values.push(element.getAttribute("placeholder") || "");
        if ("labels" in element) {
          for (const label of Array.from((element as HTMLInputElement).labels || [])) {
            values.push(label.textContent || "");
          }
        }
        const parentLabel = element.closest("label");
        if (parentLabel) values.push(parentLabel.textContent || "");
      }
      return normalize(values.join(" "));
    };
    const inputs = Array.from(document.querySelectorAll("input, textarea"))
      .filter((element) => isVisible(element))
      .map((element) => ({
        tag: element.tagName.toLowerCase(),
        type: normalize(element.getAttribute("type") || "").toLowerCase(),
        name: normalize(element.getAttribute("name") || "").toLowerCase(),
        autocomplete: normalize(element.getAttribute("autocomplete") || "").toLowerCase(),
        inputmode: normalize(element.getAttribute("inputmode") || "").toLowerCase(),
        label: labelTextFor(element).toLowerCase(),
        maxLength: Number((element as HTMLInputElement).maxLength || 0),
      }));
    const buttons = Array.from(document.querySelectorAll("button, [role='button'], input[type='submit'], a"))
      .filter((element) => isVisible(element))
      .map((element) => normalize(element.textContent || element.getAttribute("value") || element.getAttribute("aria-label")))
      .filter(Boolean)
      .map((value) => value.toLowerCase());
    return {
      url: String(window.location.href || ""),
      title: normalize(document.title || "").toLowerCase(),
      bodyText: normalize(document.body?.innerText || "").slice(0, Math.max(0, Number(textLimit) || 0)).toLowerCase(),
      inputs,
      buttons,
    };
  }, TEXT_LIMIT);

const isTransientNavigationStateReadError = (error: unknown): boolean => {
  const message = error instanceof Error ? error.message : String(error);
  return /Execution context was destroyed/i.test(message)
    || /Cannot find context with specified id/i.test(message);
};

const isClosedPageReadError = (error: unknown): boolean => {
  const message = error instanceof Error ? error.message : String(error);
  return /Target page, context or browser has been closed/i.test(message)
    || /has been closed/i.test(message);
};

const resolveReplacementPage = (page: Page): Page | null => {
  const candidates = page.context().pages().filter((candidate) => {
    if (candidate.isClosed()) return false;
    try {
      const url = candidate.url();
      return Boolean(url) && url !== "about:blank";
    } catch {
      return false;
    }
  });
  if (candidates.length === 0) return null;
  return candidates.at(-1) ?? null;
};

const resolvePreferredAuthPage = (page: Page): Page | null => {
  if (!page.isClosed()) {
    try {
      const currentUrl = page.url();
      if (currentUrl && currentUrl !== "about:blank") {
        return page;
      }
    } catch {
      // fall through to replacement resolution
    }
  }
  return resolveReplacementPage(page);
};

const typeIntoBrowserAuthFieldLikeHuman = async (locator: Locator, value: string): Promise<void> => {
  const normalizedValue = readString(value);
  if (!normalizedValue) return;
  const currentValue = await locator.inputValue().catch(() => "");
  if (currentValue === normalizedValue) return;

  await locator.click({ timeout: 5_000 });
  if (currentValue) {
    await locator.press(process.platform === "darwin" ? "Meta+A" : "Control+A").catch(() => {});
    await locator.press("Backspace").catch(() => {});
  }
  await locator.pressSequentially(normalizedValue, { delay: readBrowserAuthTypingDelayMs() });
  const typedValue = await locator.inputValue().catch(() => "");
  if (typedValue === normalizedValue) {
    return;
  }

  await locator.fill("").catch(() => {});
  await locator.fill(normalizedValue);
  const filledValue = await locator.inputValue().catch(() => "");
  if (filledValue !== normalizedValue) {
    throw new Error(`browser auth field value mismatch after fill fallback: expected ${normalizedValue.length} chars, got ${filledValue.length}`);
  }
};

const tryLocatorClick = async (locator: Pick<Locator, "click">, timeout: number): Promise<boolean> => {
  try {
    await locator.click({ timeout });
    return true;
  } catch {
    return false;
  }
};

const clickVisibleTextAction = async (page: Page, candidates: string[]): Promise<boolean> => {
  for (const label of candidates) {
    const pattern = new RegExp(escapeRegExp(label), "i");
    for (const locator of [
      page.getByRole("button", { name: pattern }),
      page.getByRole("link", { name: pattern }),
      page.getByLabel(pattern),
      page.getByText(pattern, { exact: false }),
    ]) {
      const count = await locator.count().catch(() => 0);
      for (let index = 0; index < count; index += 1) {
        const candidate = locator.nth(index);
        if (!(await candidate.isVisible().catch(() => false))) continue;
        if (await tryLocatorClick(candidate, 3_000)) {
          return true;
        }
      }
    }
  }

  return page.evaluate((texts) => {
    const normalize = (value: unknown) => String(value ?? "").replace(/\s+/g, " ").trim().toLowerCase();
    const wanted = texts.map((entry) => normalize(entry)).filter(Boolean);
    const isVisible = (element: Element | null): element is HTMLElement => {
      if (!(element instanceof HTMLElement)) return false;
      const style = window.getComputedStyle(element);
      if (!style || style.display === "none" || style.visibility === "hidden") return false;
      const rect = element.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0;
    };
    const elements = Array.from(document.querySelectorAll("button, [role='button'], input[type='submit'], a, div, span"));
    for (const element of elements) {
      if (!isVisible(element)) continue;
      const text = normalize(element.textContent || element.getAttribute("value") || element.getAttribute("aria-label"));
      if (!text || !wanted.some((wantedText) => text.includes(wantedText))) continue;
      const target = element.closest("button, [role='button'], a, label, div, span") || element;
      if (!(target instanceof HTMLElement)) continue;
      target.click();
      return true;
    }
    return false;
  }, candidates);
};

const clickVisibleTextActionAndObserveNextPage = async (
  page: Page,
  candidates: string[],
  timeoutMs = 5_000,
): Promise<Page | null> => {
  const existingPages = new Set(page.context().pages());
  const popupPromise = page.waitForEvent("popup", { timeout: timeoutMs }).catch(() => null);
  const contextPagePromise = page.context().waitForEvent("page", { timeout: timeoutMs }).catch(() => null);
  const clicked = await clickVisibleTextAction(page, candidates);
  if (!clicked) return null;
  const [popupPage, contextPage] = await Promise.all([popupPromise, contextPagePromise]);
  const nextPage = popupPage
    || contextPage
    || page.context().pages().find((candidate) => !existingPages.has(candidate))
    || page;
  await nextPage.waitForLoadState("domcontentloaded", { timeout: timeoutMs }).catch(() => {});
  return nextPage;
};

const clickGoogleAccountChooserSelection = async (page: Page, email: string): Promise<boolean> => {
  const footerPattern = /privacy|terms|help|create account|forgot email|use another account/i;
  const normalizedEmail = normalizeUiText(email);
  const candidateLocators: Locator[] = [
    page.locator("[data-email]:visible"),
    page.locator("[data-identifier]:visible"),
    page.locator("[role='link']:visible"),
    page.locator("[role='button']:visible"),
    page.locator("li:visible"),
    page.locator("div[tabindex]:visible"),
    page.locator("div[jscontroller]:visible"),
  ];

  const clickMatchingCandidate = async (preferEmail: boolean): Promise<boolean> => {
    for (const locator of candidateLocators) {
      const count = await locator.count().catch(() => 0);
      for (let index = 0; index < count; index += 1) {
        const candidate = locator.nth(index);
        if (!(await candidate.isVisible().catch(() => false))) continue;
        const text = normalizeUiText(await candidate.textContent().catch(() => ""));
        if (!text || footerPattern.test(text)) continue;
        if (preferEmail && normalizedEmail && !text.includes(normalizedEmail)) continue;
        if (await tryLocatorClick(candidate, 3_000)) {
          return true;
        }
      }
    }
    return false;
  };

  return await clickMatchingCandidate(true) || await clickMatchingCandidate(false);
};

const fillBrowserAuthField = async (
  page: Page,
  kind: keyof typeof AUTH_INPUT_SELECTOR_GROUPS,
  value: string,
): Promise<boolean> => {
  const fillLocator = async (locator: Locator): Promise<boolean> => {
    const count = await locator.count().catch(() => 0);
    for (let index = 0; index < count; index += 1) {
      const candidate = locator.nth(index);
      if (!(await candidate.isVisible().catch(() => false))) continue;
      const metadata = await candidate.evaluate((element) => ({
        ariaHidden: element.getAttribute("aria-hidden") === "true",
        tabIndex: element instanceof HTMLElement ? element.tabIndex : 0,
      })).catch(() => null);
      if (metadata?.ariaHidden || (metadata?.tabIndex ?? 0) < 0) continue;
      if (!(await candidate.isEditable().catch(() => false))) continue;
      try {
        await typeIntoBrowserAuthFieldLikeHuman(candidate, value);
        return true;
      } catch {
        continue;
      }
    }
    return false;
  };

  const preferredLocators: Locator[] = kind === "email"
    ? [
      page.getByLabel(/email or phone|email/i),
      page.getByPlaceholder(/email or phone|email/i),
      page.locator("input[name='identifier']:visible"),
    ]
    : [
      page.getByLabel(/password/i),
      page.getByPlaceholder(/password/i),
    ];

  for (const locator of preferredLocators) {
    if (await fillLocator(locator)) {
      return true;
    }
  }

  for (const selector of AUTH_INPUT_SELECTOR_GROUPS[kind]) {
    if (await fillLocator(page.locator(`${selector}:visible`))) {
      return true;
    }
  }

  return false;
};

const submitFocusedAuthFieldWithEnter = async (
  page: Page,
  kind: keyof typeof AUTH_INPUT_SELECTOR_GROUPS,
): Promise<boolean> => {
  for (const selector of AUTH_INPUT_SELECTOR_GROUPS[kind]) {
    const locator = page.locator(`${selector}:visible`);
    const count = await locator.count().catch(() => 0);
    for (let index = 0; index < count; index += 1) {
      const candidate = locator.nth(index);
      if (!(await candidate.isVisible().catch(() => false))) continue;
      if (!(await candidate.isEditable().catch(() => false))) continue;
      try {
        await candidate.click({ timeout: 2_000 });
        await candidate.press("Enter", { timeout: 2_000 });
        return true;
      } catch {
        continue;
      }
    }
  }
  return false;
};

const submitVisibleAuthStep = async (page: Page, labels: string[]): Promise<boolean> => {
  for (const label of labels) {
    const pattern = new RegExp(`^\\s*${escapeRegExp(label)}\\s*$`, "i");
    for (const locator of [
      page.getByRole("button", { name: pattern }),
      page.getByRole("link", { name: pattern }),
      page.getByLabel(pattern),
    ]) {
      const count = await locator.count().catch(() => 0);
      for (let index = 0; index < count; index += 1) {
        const candidate = locator.nth(index);
        if (!(await candidate.isVisible().catch(() => false))) continue;
        if (await tryLocatorClick(candidate, 3_000)) {
          return true;
        }
      }
    }
  }
  if (await clickVisibleTextAction(page, labels)) {
    return true;
  }
  await page.keyboard.press("Enter").catch(() => {});
  return true;
};

const submitGoogleSignInStep = async (
  page: Page,
  step: "identifier" | "password",
): Promise<boolean> => {
  const submittedForm = await page.evaluate((currentStep) => {
    const selector = currentStep === "identifier" ? "input[name='identifier']" : "input[type='password']";
    const input = document.querySelector(selector);
    if (!(input instanceof HTMLElement)) return false;
    const form = input.closest("form");
    if (!(form instanceof HTMLFormElement)) return false;
    if (typeof form.requestSubmit === "function") {
      form.requestSubmit();
      return true;
    }
    form.submit();
    return true;
  }, step).catch(() => false);
  if (submittedForm) {
    return true;
  }

  const selectors = step === "identifier"
    ? ["#identifierNext button", "#identifierNext", "div#identifierNext"]
    : ["#passwordNext button", "#passwordNext", "div#passwordNext"];
  for (const selector of selectors) {
    const locator = page.locator(`${selector}:visible`);
    const count = await locator.count().catch(() => 0);
    for (let index = 0; index < count; index += 1) {
      const candidate = locator.nth(index);
      if (!(await candidate.isVisible().catch(() => false))) continue;
      if (await tryLocatorClick(candidate, 3_000)) {
        return true;
      }
      const clicked = await candidate.evaluate((element) => {
        if (!(element instanceof HTMLElement)) return false;
        element.click();
        return true;
      }).catch(() => false);
      if (clicked) {
        return true;
      }
    }
  }
  return false;
};

const classifyGoogleChallenge = ({ state, hasOtpInput }: { state: VisibleDomState; hasOtpInput: boolean }) => {
  if (!readStateHost(state).includes("accounts.google.")) {
    return "";
  }
  if (GOOGLE_INTERACTIVE_CHALLENGE_PATTERNS.some((pattern) => stateMentions(state, pattern))) {
    return "google_interactive_challenge";
  }
  if (GOOGLE_ADDITIONAL_VERIFICATION_PATTERNS.some((pattern) => stateMentions(state, pattern))) {
    return "google_additional_verification_required";
  }
  if (hasOtpInput) {
    return "google_otp_required";
  }
  return "";
};

const classifyClaudeLoginState = ({
  state,
  clickedGoogleEntry,
}: {
  state: VisibleDomState;
  clickedGoogleEntry: boolean;
}): string => {
  if (!clickedGoogleEntry && stateMentions(state, /continue with google/i)) {
    return "";
  }
  const hasError = CLAUDE_LOGIN_ERROR_PATTERNS.some((pattern) => stateMentions(state, pattern));
  if (clickedGoogleEntry && hasError && !stateMentions(state, /continue with google/i)) {
    return "claude_login_error";
  }
  return "";
};

export const extractClaudeSetupTokenAuthUrlFromCliOutput = (output: string): string => {
  const hyperlinkMatches = Array.from(
    output.matchAll(new RegExp(CLAUDE_SETUP_TOKEN_OSC_AUTH_URL_PATTERN.source, "gi")),
  )
    .map((match) => readString(match[1] ?? ""))
    .filter(Boolean);
  const validHyperlinkUrl = findLastMatching(hyperlinkMatches, (candidate) => isValidClaudeSetupTokenRedirectUri(candidate));
  if (validHyperlinkUrl) {
    return validHyperlinkUrl;
  }

  const sanitized = stripTerminalControlSequences(output);
  const start = sanitized.lastIndexOf("https://claude.ai/oauth/authorize");
  if (start === -1) return "";
  const tail = sanitized.slice(start);
  const promptIndex = tail.search(/Paste code here if prompted >/i);
  const joined = (promptIndex === -1 ? tail : tail.slice(0, promptIndex)).replace(/\s+/g, "");
  const textMatches = Array.from(
    joined.matchAll(new RegExp(CLAUDE_SETUP_TOKEN_AUTH_URL_PATTERN.source, "gi")),
  )
    .map((match) => readString(match[0] ?? ""))
    .filter(Boolean);
  const validTextUrl = findLastMatching(textMatches, (candidate) => isValidClaudeSetupTokenRedirectUri(candidate));
  if (validTextUrl) {
    return validTextUrl;
  }
  return textMatches.at(-1)
    ?? hyperlinkMatches.at(-1)
    ?? "";
};

const readClaudeSetupToken = (output: string): string => {
  const sanitized = stripTerminalControlSequences(output);
  const start = sanitized.lastIndexOf("sk-ant-oat");
  if (start === -1) return "";
  const joined = sanitized.slice(start).replace(/\s+/g, "");
  const match = CLAUDE_SETUP_TOKEN_PATTERN.exec(joined);
  return readString(match?.[0] ?? "");
};

const waitForClaudeSetupTokenCliValue = async ({
  child,
  readOutput,
  timeoutMs,
  pollMs,
  label,
  extractor,
}: {
  child: ChildProcess;
  readOutput: () => string;
  timeoutMs: number;
  pollMs: number;
  label: string;
  extractor: (output: string) => string;
}): Promise<string> => {
  const startedAt = Date.now();
  while (Date.now() - startedAt <= timeoutMs) {
    const output = readOutput();
    const value = extractor(output);
    if (value) {
      return value;
    }
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(`claude setup-token exited before ${label}: ${redactClaudeSetupTokenOutput(output) || "<no output>"}`);
    }
    await waitMs(pollMs);
  }
  throw new Error(`timed out waiting for claude setup-token ${label}: ${redactClaudeSetupTokenOutput(readOutput()) || "<no output>"}`);
};

const driveGoogleBackedBrowserLoginWithCredentials = async ({
  page,
  authUrl,
  email,
  password,
  providerLabel,
  skipInitialGoto = false,
  timeoutMs = DEFAULT_TIMEOUT_MS,
  pollMs = DEFAULT_POLL_MS,
  onState,
}: GoogleDriveOptions): Promise<Record<string, unknown>> => {
  const normalizedEmail = readString(email);
  const normalizedPassword = password;
  if (!normalizedEmail) throw new Error(`${providerLabel} Google email is required`);
  if (!normalizedPassword) throw new Error(`${providerLabel} Google password is required`);

  let activePage = page;
  if (!skipInitialGoto) {
    await activePage.goto(authUrl, { waitUntil: "domcontentloaded", timeout: timeoutMs });
    logBrowserAuthDebug(providerLabel, "post_goto_url", {
      url: activePage.url(),
      redirectUri: providerLabel === "claude-setup-token"
        ? parseClaudeSetupTokenRedirectUri(activePage.url())
        : "",
    });
  }
  const startedAt = Date.now();
  let lastState: VisibleDomState | null = null;
  let lastDebugSnapshot = "";
  const progress: DriveProgress = {
    clickedGoogleEntry: false,
    selectedAccount: false,
    usedEmail: false,
    usedPassword: false,
    grantedConsent: false,
  };

  logBrowserAuthDebug(providerLabel, "goto_auth_url", {
    authUrl,
    redirectUri: providerLabel === "claude-setup-token"
      ? parseClaudeSetupTokenRedirectUri(authUrl)
      : "",
  });

  while (Date.now() - startedAt <= timeoutMs) {
    const preferredPage = resolvePreferredAuthPage(activePage);
    if (preferredPage && preferredPage !== activePage) {
      activePage = preferredPage;
      logBrowserAuthDebug(providerLabel, "switched_preferred_auth_page", {
        url: sanitizeAuthUrl(activePage.url()),
      });
    }
    if (activePage.isClosed()) {
      const replacementPage = resolvePreferredAuthPage(activePage);
      if (replacementPage) {
        activePage = replacementPage;
        logBrowserAuthDebug(providerLabel, "switched_closed_page_context");
      } else {
        throw new Error(`${providerLabel} browser auth page closed before completion`);
      }
    }

    await activePage.waitForLoadState("domcontentloaded", { timeout: 5_000 }).catch(() => {});
    let state: VisibleDomState;
    try {
      state = await readVisibleDomState(activePage);
    } catch (error) {
      if (isTransientNavigationStateReadError(error)) {
        await waitMs(pollMs);
        continue;
      }
      if (isClosedPageReadError(error)) {
        const replacementPage = resolvePreferredAuthPage(activePage);
        if (replacementPage) {
          activePage = replacementPage;
          logBrowserAuthDebug(providerLabel, "recovered_closed_page_context");
          await waitMs(pollMs);
          continue;
        }
      }
      throw error;
    }

    lastState = state;
    const debugSnapshot = JSON.stringify({
      url: sanitizeAuthUrl(state.url),
      title: state.title,
      inputs: state.inputs.map((entry) => ({ type: entry.type, name: entry.name, label: entry.label })),
      buttons: state.buttons.slice(0, 8),
      progress,
    });
    if (debugSnapshot !== lastDebugSnapshot) {
      logBrowserAuthDebug(providerLabel, "state", JSON.parse(debugSnapshot));
      lastDebugSnapshot = debugSnapshot;
    }

    const inputs = Array.isArray(state.inputs) ? state.inputs : [];
    const hasEmailInput = inputs.some((entry) =>
      entry.type === "email" || entry.autocomplete === "email" || entry.name.includes("email") || entry.label.includes("email"));
    const hasPasswordInput = inputs.some((entry) =>
      entry.type === "password" || entry.autocomplete.includes("password") || entry.label.includes("password"));
    const hasOtpInput = inputs.some((entry) =>
      entry.autocomplete === "one-time-code"
        || entry.inputmode === "numeric"
        || entry.name.includes("otp")
        || entry.name.includes("code")
        || entry.label.includes("code")
        || entry.label.includes("verification"));
    const isGoogleHost = readStateHost(state).includes("accounts.google.");

    if (typeof onState === "function") {
      const outcome = await onState({
        page: activePage,
        state,
        progress: { ...progress },
        hasEmailInput,
        hasPasswordInput,
        hasOtpInput,
        isGoogleHost,
      });
      if (outcome && "done" in outcome && outcome.done) {
        return {
          providerLabel,
          progress: { ...progress },
          ...outcome.result,
        };
      }
      if (outcome && "handled" in outcome && outcome.handled) {
        if (outcome.page && !outcome.page.isClosed()) {
          activePage = outcome.page;
        }
        await waitMs(pollMs);
        continue;
      }
    }

    const blockedReason = classifyGoogleChallenge({ state, hasOtpInput });
    if (blockedReason) {
      throw new Error(`${providerLabel} Google OAuth blocked by ${blockedReason}: ${JSON.stringify(summarizeState(state))}`);
    }

    if (!isGoogleHost && !progress.clickedGoogleEntry) {
      const nextPage = await clickVisibleTextActionAndObserveNextPage(activePage, [
        "continue with google",
        "sign in with google",
        "continue to google",
      ]);
      if (nextPage) {
        progress.clickedGoogleEntry = true;
        activePage = nextPage;
        logBrowserAuthDebug(providerLabel, "clicked_google_entry", summarizeState(state));
        await waitMs(pollMs);
        continue;
      }
    }

    if (isGoogleHost && GOOGLE_ERROR_PATTERNS.some((pattern) => stateMentions(state, pattern))) {
      throw new Error(`${providerLabel} Google auth page reported an error: ${JSON.stringify(summarizeState(state))}`);
    }

    if (hasEmailInput && (isGoogleHost || !progress.clickedGoogleEntry)) {
      if (!progress.usedEmail) {
        const filled = await fillBrowserAuthField(activePage, "email", normalizedEmail);
        if (!filled) {
          throw new Error(`failed to fill ${providerLabel} Google email`);
        }
        progress.usedEmail = true;
        logBrowserAuthDebug(providerLabel, "submitted_email", { email: normalizedEmail });
      }
      if (isGoogleHost) {
        const identifierValue = await activePage.locator("input[name='identifier']").inputValue().catch(() => "");
        logBrowserAuthDebug(providerLabel, "google_identifier_value", {
          value: identifierValue,
          length: identifierValue.length,
        });
        if (await submitFocusedAuthFieldWithEnter(activePage, "email")) {
          logBrowserAuthDebug(providerLabel, "submitted_identifier_with_enter");
          await waitMs(pollMs);
          continue;
        }
        await activePage.keyboard.press("Tab").catch(() => {});
      }
      if (isGoogleHost && await submitGoogleSignInStep(activePage, "identifier")) {
        logBrowserAuthDebug(providerLabel, "submitted_identifier_step");
        await waitMs(pollMs);
        continue;
      }
      await submitVisibleAuthStep(activePage, ["next", "continue", "sign in"]);
      await waitMs(pollMs);
      continue;
    }

    if (hasPasswordInput && (isGoogleHost || !progress.clickedGoogleEntry)) {
      if (!progress.usedPassword) {
        const filled = await fillBrowserAuthField(activePage, "password", normalizedPassword);
        if (!filled) {
          throw new Error(`failed to fill ${providerLabel} Google password`);
        }
        progress.usedPassword = true;
        logBrowserAuthDebug(providerLabel, "submitted_password");
      }
      if (isGoogleHost) {
        const passwordValue = await activePage.locator("input[type='password']").inputValue().catch(() => "");
        logBrowserAuthDebug(providerLabel, "google_password_value", {
          length: passwordValue.length,
        });
        if (await submitFocusedAuthFieldWithEnter(activePage, "password")) {
          logBrowserAuthDebug(providerLabel, "submitted_password_with_enter");
          await waitMs(pollMs);
          continue;
        }
        await activePage.keyboard.press("Tab").catch(() => {});
      }
      if (isGoogleHost && await submitGoogleSignInStep(activePage, "password")) {
        logBrowserAuthDebug(providerLabel, "submitted_password_step");
        await waitMs(pollMs);
        continue;
      }
      await submitVisibleAuthStep(activePage, ["next", "continue", "sign in"]);
      await waitMs(pollMs);
      continue;
    }

    if (isGoogleHost && /\/accountchooser\b/i.test(state.url) && !hasEmailInput && !hasPasswordInput) {
      const selected = await clickGoogleAccountChooserSelection(activePage, normalizedEmail);
      if (selected) {
        progress.selectedAccount = true;
        logBrowserAuthDebug(providerLabel, "selected_google_account_chooser_entry");
        await waitMs(pollMs);
        continue;
      }
    }

    if (isGoogleHost && stateMentions(state, /allow|continue|accept|agree|continue as|sign in/i)) {
      const granted = await submitVisibleAuthStep(activePage, ["allow", "continue", "accept", "agree", "continue as", "sign in"]);
      if (granted) {
        progress.grantedConsent = true;
        logBrowserAuthDebug(providerLabel, "granted_consent", summarizeState(state));
        await waitMs(pollMs);
        continue;
      }
    }

    if (!hasEmailInput && !hasPasswordInput) {
      const canSelectKnownAccount = stateMentions(state, new RegExp(escapeRegExp(normalizedEmail), "i"));
      if (canSelectKnownAccount) {
        const choseExistingAccount = await clickVisibleTextAction(activePage, [normalizedEmail]);
        if (choseExistingAccount) {
          progress.selectedAccount = true;
          logBrowserAuthDebug(providerLabel, "selected_existing_account", { email: normalizedEmail });
          await waitMs(pollMs);
          continue;
        }
      }
    }

    await waitMs(pollMs);
  }

  throw new Error(`timed out driving ${providerLabel} Google OAuth browser flow: ${JSON.stringify(summarizeState(lastState ?? {
    url: "",
    title: "",
    bodyText: "",
    inputs: [],
    buttons: [],
  }))}`);
};

const driveClaudeSetupTokenBrowserFlow = async (opts: {
  page: Page;
  authUrl: string;
  email: string;
  password: string;
  skipInitialGoto?: boolean;
  timeoutMs: number;
  pollMs: number;
  providerLabel: string;
  readSetupToken?: () => string;
}): Promise<void> => {
  const claudePostGoogleSettleMs = readClaudePostGoogleSettleMs();
  let firstPostGoogleClaudePageAt: number | null = null;
  await driveGoogleBackedBrowserLoginWithCredentials({
    page: opts.page,
    authUrl: opts.authUrl,
    email: opts.email,
    password: opts.password,
    providerLabel: opts.providerLabel,
    skipInitialGoto: opts.skipInitialGoto,
    timeoutMs: opts.timeoutMs,
    pollMs: opts.pollMs,
    onState: async ({ page: activePage, state, progress }) => {
      const isClaudeHost = readStateHost(state).includes("claude.ai");
      const isClaudeLoginShell = isClaudeHost
        && /\/login\b/i.test(state.url)
        && stateMentions(state, /continue with google|continue with email|continue with sso/i);
      const claudeState = classifyClaudeLoginState({
        state,
        clickedGoogleEntry: progress.clickedGoogleEntry,
      });
      if (claudeState) {
        throw new Error(`${opts.providerLabel} browser login blocked by ${claudeState}: ${JSON.stringify(summarizeState(state))}`);
      }
      if (!progress.clickedGoogleEntry && isClaudeLoginShell && stateMentions(state, /continue with google/i)) {
        const nextPage = await clickVisibleTextActionAndObserveNextPage(activePage, [
          "continue with google",
          "sign in with google",
          "continue to google",
        ]);
        const submitted = nextPage
          ? false
          : await submitVisibleAuthStep(activePage, [
            "continue with google",
            "sign in with google",
            "continue to google",
          ]);
        if (!nextPage && !submitted) {
          throw new Error(`Claude setup-token login shell did not expose a usable Google continuation: ${JSON.stringify(summarizeState(state))}`);
        }
        return {
          handled: true,
          page: nextPage ?? activePage,
        };
      }
      const emittedSetupToken = readString(opts.readSetupToken?.());
      if (emittedSetupToken) {
        return {
          done: true,
          result: {
            finalUrl: sanitizeAuthUrl(state.url),
            setupToken: emittedSetupToken,
          },
        };
      }
      if (progress.grantedConsent && isClaudeLoginShell) {
        const now = Date.now();
        firstPostGoogleClaudePageAt ??= now;
        const elapsedMs = now - firstPostGoogleClaudePageAt;
        if (elapsedMs < claudePostGoogleSettleMs) {
          logBrowserAuthDebug(opts.providerLabel, "waiting_for_post_google_session_settle", {
            elapsedMs,
            settleMs: claudePostGoogleSettleMs,
            state: summarizeState(state),
          });
          return { handled: true };
        }
      } else if (!isClaudeLoginShell) {
        firstPostGoogleClaudePageAt = null;
      }
      if (progress.grantedConsent && isClaudeLoginShell && stateMentions(state, /continue with google/i)) {
        const nextPage = await clickVisibleTextActionAndObserveNextPage(activePage, [
          "continue with google",
          "sign in with google",
          "continue to google",
        ]);
        const submitted = nextPage
          ? false
          : await submitVisibleAuthStep(activePage, [
            "continue with google",
            "sign in with google",
            "continue to google",
          ]);
        if (!nextPage && !submitted) {
          throw new Error(`Claude setup-token select-account page did not expose a usable Google continuation: ${JSON.stringify(summarizeState(state))}`);
        }
        return {
          handled: true,
          page: nextPage ?? activePage,
        };
      }
      if (isClaudeHost && /\/oauth\/authorize\b/i.test(state.url) && stateMentions(state, /authorize|decline|switch account/i)) {
        logBrowserAuthDebug(opts.providerLabel, "authorize_surface", {
          url: state.url,
          redirectUri: parseClaudeSetupTokenRedirectUri(state.url),
          buttons: state.buttons,
        });
        const approved = await clickVisibleTextAction(activePage, ["authorize", "allow", "continue"]);
        if (!approved) {
          return { handled: true };
        }
        await waitMs(1_000);
        logBrowserAuthDebug(opts.providerLabel, "post_authorize_click", {
          url: activePage.url(),
          redirectUri: parseClaudeSetupTokenRedirectUri(activePage.url()),
        });
        return { handled: true };
      }
      if (stateMentions(state, /copy code/i)) {
        throw new Error(
          "Claude setup-token fell back to manual copy-code auth, which ctx intentionally does not support.",
        );
      }
      if (stateMentions(state, /you can close this tab|login successful|connected to claude code/i)) {
        return {
          done: true,
          result: {
            finalUrl: sanitizeAuthUrl(state.url),
            setupToken: emittedSetupToken,
          },
        };
      }
      if (stateMentions(state, /you'?re all set up for claude code|you can now close this window/i)) {
        return {
          done: true,
          result: {
            finalUrl: sanitizeAuthUrl(state.url),
            setupToken: emittedSetupToken,
          },
        };
      }
      return null;
    },
  });
};

const completeClaudeManagedSetupTokenWithRealChromeBrowser = async (
  opts: ClaudeManagedSetupTokenOptions,
): Promise<void> => {
  if (process.platform !== "darwin") {
    throw new Error("Claude setup-token real-browser automation currently requires macOS");
  }
  if (!existsSync(GOOGLE_CHROME_EXECUTABLE)) {
    throw new Error(`Google Chrome executable not found at ${GOOGLE_CHROME_EXECUTABLE}`);
  }

  const timeoutMs = readBrowserAuthTimeoutMs(opts.timeoutMs ?? DEFAULT_TIMEOUT_MS);
  const pollMs = opts.pollMs ?? DEFAULT_POLL_MS;
  const deadline = Date.now() + timeoutMs;
  const screenshotPath = path.join(tmpdir(), `ctx-claude-real-browser-${process.pid}.png`);
  let clickedAuthorize = false;
  let clickedGoogleEntry = false;
  let clickedExistingAccount = false;

  openAuthUrlInRealChrome(opts.authUrl);
  activateRealChrome();

  while (Date.now() <= deadline) {
    const frontUrl = readFrontChromeUrl();
    logBrowserAuthDebug("claude-setup-token", "real_browser_poll", {
      url: frontUrl,
      redirectUri: parseClaudeSetupTokenRedirectUri(frontUrl),
      clickedAuthorize,
      clickedGoogleEntry,
      clickedExistingAccount,
    });
    if (CLAUDE_SETUP_TOKEN_SUCCESS_URL_PATTERN.test(frontUrl)) {
      return;
    }

    captureScreenToPath(screenshotPath);
    try {
      if (!clickedGoogleEntry) {
        const continueWithGoogle = findMacOsScreenTextMatch(screenshotPath, "Continue with Google");
        if (continueWithGoogle) {
          activateRealChrome();
          clickMacOsScreenPoint(continueWithGoogle.x, continueWithGoogle.y);
          clickedGoogleEntry = true;
          await waitMs(1_000);
          continue;
        }
      }

      if (!clickedExistingAccount && opts.email) {
        const existingAccount = findMacOsScreenTextMatch(screenshotPath, opts.email);
        if (existingAccount) {
          activateRealChrome();
          clickMacOsScreenPoint(existingAccount.x, existingAccount.y);
          clickedExistingAccount = true;
          await waitMs(1_000);
          continue;
        }
      }

      if (!clickedAuthorize) {
        const authorize = findMacOsScreenTextMatch(screenshotPath, "Authorize");
        if (authorize) {
          logBrowserAuthDebug("claude-setup-token", "real_browser_authorize_hit", authorize);
          activateRealChrome();
          clickMacOsScreenPoint(authorize.x, authorize.y);
          clickedAuthorize = true;
          await waitMs(1_500);
          continue;
        }
      }
    } finally {
      rmSync(screenshotPath, { force: true });
    }

    await waitMs(pollMs);
  }

  throw new Error(`timed out completing Claude setup-token in real Chrome; final_url=${readFrontChromeUrl()}`);
};

export async function completeClaudeManagedSetupTokenWithGoogleBrowserCredentials(
  opts: ClaudeManagedSetupTokenOptions,
): Promise<void> {
  if (process.platform === "darwin") {
    await completeClaudeManagedSetupTokenWithRealChromeBrowser(opts);
    return;
  }
  const timeoutMs = readBrowserAuthTimeoutMs(opts.timeoutMs ?? DEFAULT_TIMEOUT_MS);
  const pollMs = opts.pollMs ?? DEFAULT_POLL_MS;
  logBrowserAuthDebug("claude-setup-token", "managed_open_url_mode", {
    hasOpenUrl: typeof opts.openUrl === "function",
  });
  const page = typeof opts.openUrl === "function"
    ? await opts.openUrl(opts.authUrl)
    : await opts.context.newPage();
  logBrowserAuthDebug("claude-setup-token", "managed_page_ready", {
    pageUrl: page.url(),
    redirectUri: parseClaudeSetupTokenRedirectUri(page.url()),
  });
  try {
    await driveClaudeSetupTokenBrowserFlow({
      page,
      authUrl: opts.authUrl,
      email: opts.email,
      password: opts.password,
      skipInitialGoto: typeof opts.openUrl === "function",
      timeoutMs,
      pollMs,
      providerLabel: "claude-setup-token",
    });
  } finally {
    await page.close().catch(() => {});
  }
}

export async function completeClaudeSetupTokenWithGoogleBrowserCredentials(
  opts: ClaudeSetupTokenOptions,
): Promise<ClaudeSetupTokenSuccess> {
  const timeoutMs = readBrowserAuthTimeoutMs(opts.timeoutMs ?? DEFAULT_TIMEOUT_MS);
  const pollMs = opts.pollMs ?? DEFAULT_POLL_MS;
  const pythonBinary = readString(process.env.CTX_E2E_PYTHON_BIN) || "python3";
  try {
    execFileSync(pythonBinary, ["--version"], { stdio: "ignore" });
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw new Error(`missing usable python3 for claude setup-token PTY bridge: ${detail}`);
  }

  const claudeCommand = readString(process.env.CTX_E2E_CLAUDE_SETUP_TOKEN_BIN) || "claude";
  const child = spawn(pythonBinary, ["-u", "-c", CLAUDE_SETUP_TOKEN_PTY_BRIDGE, claudeCommand, "setup-token"], {
    env: process.env,
    stdio: ["pipe", "pipe", "pipe"],
  });

  const outputChunks: string[] = [];
  const appendOutput = (chunk: string | Buffer): void => {
    const text = typeof chunk === "string" ? chunk : chunk.toString("utf8");
    if (text) {
      outputChunks.push(text);
    }
  };
  child.stdout?.setEncoding("utf8");
  child.stderr?.setEncoding("utf8");
  child.stdout?.on("data", appendOutput);
  child.stderr?.on("data", appendOutput);

  const readOutput = () => outputChunks.join("");
  const page = await opts.context.newPage();
  try {
    const authUrl = await waitForClaudeSetupTokenCliValue({
      child,
      readOutput,
      timeoutMs,
      pollMs,
      label: "auth url",
      extractor: extractClaudeSetupTokenAuthUrlFromCliOutput,
    });
    logBrowserAuthDebug("claude-setup-token", "auth_url_extracted", {
      authUrl,
      redirectUri: parseClaudeSetupTokenRedirectUri(authUrl),
    });

    await driveClaudeSetupTokenBrowserFlow({
      page,
      authUrl,
      email: opts.email,
      password: opts.password,
      timeoutMs,
      pollMs,
      providerLabel: "claude-setup-token",
      readSetupToken: () => readClaudeSetupToken(readOutput()),
    });
    const setupToken = await waitForClaudeSetupTokenCliValue({
      child,
      readOutput,
      timeoutMs,
      pollMs,
      label: "setup token",
      extractor: readClaudeSetupToken,
    });
    await waitForChildProcessExit(child, 15_000);
    return {
      authUrl,
      setupToken,
    };
  } finally {
    await page.close().catch(() => {});
    if (child.exitCode === null && child.signalCode === null) {
      await terminateChildProcess(child, 5_000);
    }
  }
}
