import { describe, expect, it } from "vitest";
import { parseMobilePairingQrPayload, PAIRING_REQUEST_ENCRYPTION } from "./mobileSecureClient";

const validPayload = (overrides: Record<string, unknown> = {}) => JSON.stringify({
  type: "context_mobile_e2ee",
  version: 1,
  tunnel_id: "tunnel-1",
  base_url: "https://tunnel.ctx.rs/t/tunnel-1",
  pairing_token: "pair-token",
  daemon_public_key: "daemon-public-key",
  pairing_request_encryption: PAIRING_REQUEST_ENCRYPTION,
  ...overrides,
});

describe("parseMobilePairingQrPayload", () => {
  it("accepts managed HTTPS QR payloads", () => {
    expect(parseMobilePairingQrPayload(validPayload())).toMatchObject({
      tunnelId: "tunnel-1",
      baseUrl: "https://tunnel.ctx.rs/t/tunnel-1",
      pairingToken: "pair-token",
    });
  });

  it("rejects managed QR payloads with cleartext base URLs", () => {
    expect(() => parseMobilePairingQrPayload(validPayload({
      base_url: "http://tunnel.ctx.rs/t/tunnel-1",
    }))).toThrow("Mobile pairing QR payload must use HTTPS.");
  });
});
