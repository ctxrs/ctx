import { useCallback, useEffect, useRef, useState } from "react";
import { Camera, X } from "lucide-react";
import { isMobileShellApp } from "../../utils/runtime";

type BarcodeResultLike = {
  rawValue?: string;
};

type BarcodeDetectorLike = {
  detect(source: CanvasImageSource): Promise<BarcodeResultLike[]>;
};

type BarcodeDetectorConstructor = new (options: { formats: string[] }) => BarcodeDetectorLike;

type BarcodeDetectorGlobal = typeof globalThis & {
  BarcodeDetector?: BarcodeDetectorConstructor;
};

const cameraUnavailableMessage =
  "Camera scanning is unavailable on this device. Paste the QR payload instead.";

const qrUnavailableMessage =
  "QR scanning is unavailable in this webview. Paste the QR payload instead.";
const nativeScannerUnavailableMessage =
  "Native QR scanning is unavailable. Paste the QR payload instead.";

export const getNativeQrBarcodeDetector = (): BarcodeDetectorConstructor | null => {
  const candidate = (globalThis as BarcodeDetectorGlobal).BarcodeDetector;
  return typeof candidate === "function" ? candidate : null;
};

export const canUseQrCameraScanner = (): boolean =>
  isMobileShellApp()
  || (
    Boolean(getNativeQrBarcodeDetector())
    && Boolean(globalThis.navigator?.mediaDevices?.getUserMedia)
  );

export const scanNativeMobileQrPayload = async (): Promise<string> => {
  try {
    const scanner = await import("@tauri-apps/plugin-barcode-scanner");
    const permission = await scanner.checkPermissions();
    const nextPermission = permission === "granted" ? permission : await scanner.requestPermissions();
    if (nextPermission !== "granted") {
      throw new Error("Camera permission is required to scan the pairing QR.");
    }
    const result = await scanner.scan({
      cameraDirection: "back",
      formats: [scanner.Format.QRCode],
    });
    const content = result.content.trim();
    if (!content) throw new Error("Scanned QR code was empty.");
    return content;
  } catch (err) {
    if (err instanceof Error && err.message.trim()) {
      throw err;
    }
    throw new Error(nativeScannerUnavailableMessage);
  }
};

export const cancelNativeMobileQrScan = async (): Promise<void> => {
  try {
    const scanner = await import("@tauri-apps/plugin-barcode-scanner");
    await scanner.cancel();
  } catch {
    // Scanner may already be closed by the native layer.
  }
};

export function MobileQrScanner({
  onDetected,
  onCancel,
  onError,
}: {
  onDetected: (payload: string) => void;
  onCancel: () => void;
  onError: (message: string) => void;
}) {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const frameRef = useRef<number | null>(null);
  const [starting, setStarting] = useState(true);

  const cancel = useCallback(() => {
    if (isMobileShellApp()) {
      void cancelNativeMobileQrScan();
    }
    onCancel();
  }, [onCancel]);

  useEffect(() => {
    if (isMobileShellApp()) {
      let cancelled = false;
      setStarting(true);
      void scanNativeMobileQrPayload()
        .then((payload) => {
          if (!cancelled) onDetected(payload);
        })
        .catch((err: unknown) => {
          if (cancelled) return;
          onError(err instanceof Error ? err.message : nativeScannerUnavailableMessage);
        })
        .finally(() => {
          if (!cancelled) setStarting(false);
        });
      return () => {
        cancelled = true;
        void cancelNativeMobileQrScan();
      };
    }

    let cancelled = false;
    let scanning = false;

    const stop = () => {
      if (frameRef.current !== null) {
        cancelAnimationFrame(frameRef.current);
        frameRef.current = null;
      }
      const stream = streamRef.current;
      streamRef.current = null;
      stream?.getTracks().forEach((track) => track.stop());
    };

    const start = async () => {
      const Detector = getNativeQrBarcodeDetector();
      if (!Detector) {
        onError(qrUnavailableMessage);
        setStarting(false);
        return;
      }
      const getUserMedia = globalThis.navigator?.mediaDevices?.getUserMedia;
      if (!getUserMedia) {
        onError(cameraUnavailableMessage);
        setStarting(false);
        return;
      }

      try {
        const stream = await getUserMedia.call(globalThis.navigator.mediaDevices, {
          audio: false,
          video: {
            facingMode: { ideal: "environment" },
          },
        });
        if (cancelled) {
          stream.getTracks().forEach((track) => track.stop());
          return;
        }

        streamRef.current = stream;
        const video = videoRef.current;
        if (!video) {
          stream.getTracks().forEach((track) => track.stop());
          return;
        }
        video.srcObject = stream;
        await video.play();
        if (cancelled) return;

        const detector = new Detector({ formats: ["qr_code"] });
        setStarting(false);

        const scan = async () => {
          if (cancelled || scanning) return;
          scanning = true;
          try {
            const results = await detector.detect(video);
            const rawValue = results
              .map((result) => result.rawValue?.trim() ?? "")
              .find((value) => value.length > 0);
            if (rawValue) {
              stop();
              onDetected(rawValue);
              return;
            }
          } catch {
            stop();
            onError(qrUnavailableMessage);
            return;
          } finally {
            scanning = false;
          }
          if (!cancelled) {
            frameRef.current = requestAnimationFrame(scan);
          }
        };

        frameRef.current = requestAnimationFrame(scan);
      } catch {
        stop();
        setStarting(false);
        onError(cameraUnavailableMessage);
      }
    };

    void start();
    return () => {
      cancelled = true;
      stop();
    };
  }, [onDetected, onError]);

  return (
    <div className="mobile-qr-scanner" role="group" aria-label="QR scanner">
      {isMobileShellApp() ? (
        <div className="mobile-qr-scanner-native" aria-hidden="true">
          <Camera size={32} />
        </div>
      ) : (
        <>
          <video ref={videoRef} className="mobile-qr-scanner-video" muted playsInline />
          <div className="mobile-qr-scanner-frame" aria-hidden="true" />
        </>
      )}
      <div className="mobile-qr-scanner-footer">
        <div className="mobile-qr-scanner-status">
          <Camera size={15} aria-hidden="true" />
          {starting ? "Starting camera..." : "Point camera at the desktop QR"}
        </div>
        <button type="button" className="mobile-shell-btn mobile-shell-btn-secondary" onClick={cancel}>
          <X size={15} aria-hidden="true" />
          Cancel
        </button>
      </div>
    </div>
  );
}
