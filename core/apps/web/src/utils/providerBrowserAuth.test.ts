import { describe, expect, it } from "vitest";

import { extractClaudeSetupTokenAuthUrlFromCliOutput } from "../../e2e/utils/providerBrowserAuth";

describe("extractClaudeSetupTokenAuthUrlFromCliOutput", () => {
  it("prefers the OSC hyperlink target over the rendered terminal text", () => {
    const cliOutput = [
      "\u001b]8;;https://claude.ai/oauth/authorize?code=true&client_id=test-client&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&state=test-state\u0007",
      "https://claude.ai/oauth/authorize?code=true&client_id=test-client&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&state=test-state",
      "\u001b]8;;\u0007",
      "\r\nPaste code here if prompted >",
    ].join("");

    expect(extractClaudeSetupTokenAuthUrlFromCliOutput(cliOutput)).toBe(
      "https://claude.ai/oauth/authorize?code=true&client_id=test-client&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&state=test-state",
    );
  });

  it("chooses the last hyperlink candidate whose decoded redirect URI is valid", () => {
    const cliOutput = [
      "\u001b]8;;https://claude.ai/oauth/authorize?code=true&client_id=test-client&response_type=code&redirect_uri=https:/platform.claude.com/oauth/code/callback&scope=user%3Ainference&state=bad-state\u0007",
      "bad",
      "\u001b]8;;\u0007",
      "\u001b]8;;https://claude.ai/oauth/authorize?code=true&client_id=test-client&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&state=good-state\u0007",
      "good",
      "\u001b]8;;\u0007",
    ].join("");

    expect(extractClaudeSetupTokenAuthUrlFromCliOutput(cliOutput)).toBe(
      "https://claude.ai/oauth/authorize?code=true&client_id=test-client&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&state=good-state",
    );
  });

  it("falls back to the rendered URL when OSC hyperlinks are absent", () => {
    const cliOutput = [
      "\u001b[38;2;255;255;255mOpening browser to sign in\u001b[39m\r\n",
      "https://claude.ai/oauth/authorize?code=true&client_id=test-client&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&state=test-state\r\n",
      "Paste code here if prompted >",
    ].join("");

    expect(extractClaudeSetupTokenAuthUrlFromCliOutput(cliOutput)).toBe(
      "https://claude.ai/oauth/authorize?code=true&client_id=test-client&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&state=test-state",
    );
  });

  it("prefers a valid rendered localhost callback URL over an invalid OSC hyperlink target", () => {
    const cliOutput = [
      "\u001b]8;;https://claude.ai/oauth/authorize?code=true&client_id=test-client&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=user%3Ainference&state=bad-state\u0007",
      "https://claude.ai/oauth/authorize?code=true&client_id=test-client&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&state=good-state",
      "\u001b]8;;\u0007",
      "\r\nPaste code here if prompted >",
    ].join("");

    expect(extractClaudeSetupTokenAuthUrlFromCliOutput(cliOutput)).toBe(
      "https://claude.ai/oauth/authorize?code=true&client_id=test-client&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback&scope=user%3Ainference&state=good-state",
    );
  });
});
