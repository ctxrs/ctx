import { useEffect, useMemo, useState } from "react";
import { X } from "lucide-react";
import { useLocation, useNavigate } from "react-router-dom";
import { getHealth, type Health } from "../api/client";

const POLL_INTERVAL_MS = 5_000;
const GIB = 1024 * 1024 * 1024;

type StorageState = Health["storage"];
type StorageActive = NonNullable<NonNullable<StorageState>["active"]>;

const bannerKeyFor = (storage: StorageState | null): string | null => {
  if (!storage) return null;
  return [storage.level, storage.active?.mount_point ?? "", storage.active?.path ?? ""].join("|");
};

const formatFreeGiB = (bytes: number): string => {
  const gib = bytes / GIB;
  return gib >= 10 ? gib.toFixed(0) : gib.toFixed(1);
};

const formatPathLabel = (active: StorageActive | null | undefined): string => {
  if (!active) return "local storage";
  if (!active.mount_point || active.mount_point === active.path) return active.label;
  return `${active.label} (${active.mount_point})`;
};

export default function StorageGuardBanner() {
  const navigate = useNavigate();
  const location = useLocation();
  const [storage, setStorage] = useState<StorageState | null>(null);
  const [dismissedKey, setDismissedKey] = useState<string | null>(null);
  const suppressed = location.pathname === "/__geometry_harness";

  useEffect(() => {
    if (suppressed) {
      setStorage(null);
      return;
    }
    let cancelled = false;
    const load = async () => {
      try {
        const health = await getHealth();
        if (cancelled) return;
        setStorage(health.storage ?? null);
      } catch {
        if (cancelled) return;
      }
    };

    void load();
    const timer = window.setInterval(() => {
      void load();
    }, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [suppressed]);

  const bannerKey = useMemo(() => bannerKeyFor(storage), [storage]);
  useEffect(() => {
    if (!bannerKey || !dismissedKey || bannerKey === dismissedKey) return;
    setDismissedKey(null);
  }, [bannerKey, dismissedKey]);

  if (suppressed) {
    return null;
  }

  if (!storage || storage.level === "normal" || dismissedKey === bannerKey) {
    return null;
  }

  const active = storage.active ?? null;
  const freeText = active ? `${formatFreeGiB(active.free_bytes)} GiB left on ${formatPathLabel(active)}.` : "";
  const title =
    storage.level === "emergency"
      ? "Storage emergency. Active agent sessions were interrupted."
      : "Storage is getting low.";
  const subtitle =
    storage.level === "emergency"
      ? `${freeText} CTX stopped running harnesses to protect local data.`
      : `${freeText} Active sessions will be interrupted if space drops below 1.0 GiB.`;

  return (
    <div
      className={`wb-snackbar wb-storage-guard-snackbar${storage.level === "emergency" ? " wb-storage-guard-snackbar-critical" : ""}`}
      role={storage.level === "emergency" ? "alert" : "status"}
      aria-live="polite"
      data-testid="storage-guard-snackbar"
    >
      <div className="wb-snackbar-body">
        <div className="wb-snackbar-title">{title}</div>
        <div className="wb-snackbar-subtitle">{subtitle}</div>
      </div>
      <div className="wb-snackbar-actions">
        <button
          type="button"
          className="wb-snackbar-btn"
          onClick={() => navigate("/diagnostics")}
        >
          Open Diagnostics
        </button>
      </div>
      <button
        type="button"
        className="wb-snackbar-close"
        onClick={() => setDismissedKey(bannerKey)}
        aria-label="Dismiss storage notice"
      >
        <X size={14} aria-hidden="true" />
      </button>
    </div>
  );
}
