import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  cancelNativeMobileQrScan,
  scanNativeMobileQrPayload,
} from "./MobileQrScanner";
import {
  cancel,
  checkPermissions,
  Format,
  requestPermissions,
  scan,
} from "@tauri-apps/plugin-barcode-scanner";

vi.mock("@tauri-apps/plugin-barcode-scanner", () => ({
  Format: {
    QRCode: "QR_CODE",
  },
  cancel: vi.fn(),
  checkPermissions: vi.fn(),
  requestPermissions: vi.fn(),
  scan: vi.fn(),
}));

describe("MobileQrScanner native helpers", () => {
  const checkPermissionsMock = vi.mocked(checkPermissions);
  const requestPermissionsMock = vi.mocked(requestPermissions);
  const scanMock = vi.mocked(scan);
  const cancelMock = vi.mocked(cancel);

  beforeEach(() => {
    checkPermissionsMock.mockReset();
    requestPermissionsMock.mockReset();
    scanMock.mockReset();
    cancelMock.mockReset();
  });

  it("scans QR content through the native plugin when permission is already granted", async () => {
    checkPermissionsMock.mockResolvedValue("granted");
    scanMock.mockResolvedValue({
      content: "  {\"type\":\"context_mobile_e2ee\"}  ",
      format: Format.QRCode,
      bounds: null,
    });

    await expect(scanNativeMobileQrPayload()).resolves.toBe('{"type":"context_mobile_e2ee"}');

    expect(requestPermissionsMock).not.toHaveBeenCalled();
    expect(scanMock).toHaveBeenCalledWith({
      cameraDirection: "back",
      formats: [Format.QRCode],
    });
  });

  it("requests permission before native scanning", async () => {
    checkPermissionsMock.mockResolvedValue("prompt");
    requestPermissionsMock.mockResolvedValue("granted");
    scanMock.mockResolvedValue({
      content: "payload",
      format: Format.QRCode,
      bounds: null,
    });

    await expect(scanNativeMobileQrPayload()).resolves.toBe("payload");
    expect(requestPermissionsMock).toHaveBeenCalledTimes(1);
  });

  it("rejects when camera permission is denied", async () => {
    checkPermissionsMock.mockResolvedValue("prompt");
    requestPermissionsMock.mockResolvedValue("denied");

    await expect(scanNativeMobileQrPayload()).rejects.toThrow("Camera permission is required");
    expect(scanMock).not.toHaveBeenCalled();
  });

  it("cancels the native scanner without surfacing already-closed errors", async () => {
    cancelMock.mockRejectedValue(new Error("already closed"));

    await expect(cancelNativeMobileQrScan()).resolves.toBeUndefined();
  });
});
