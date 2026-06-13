import type { ReactNode } from "react";
import { Switch } from "../../components/ui/switch";
import { clampPct } from "./SettingsPage.utils";

export function Toggle({
  checked,
  disabled,
  onChange,
  ariaLabel,
}: {
  checked: boolean;
  disabled?: boolean;
  onChange: (next: boolean) => void;
  ariaLabel: string;
}) {
  return (
    <Switch
      checked={checked}
      disabled={disabled}
      aria-label={ariaLabel}
      onCheckedChange={onChange}
    />
  );
}

export function Row({
  title,
  description,
  control,
}: {
  title: string;
  description?: string;
  control: ReactNode;
}) {
  return (
    <div className={`settings-row ${description ? "" : "settings-row-single"}`}>
      <div className="settings-row-left">
        <div className="settings-row-title">{title}</div>
        {description ? <div className="settings-row-desc">{description}</div> : null}
      </div>
      <div className="settings-row-right">{control}</div>
    </div>
  );
}

export function Card({ children, title }: { title?: string; children: ReactNode }) {
  return (
    <div className="settings-card">
      {title ? <div className="settings-card-title">{title}</div> : null}
      <div className="settings-card-rows">{children}</div>
    </div>
  );
}

export function Metric({
  label,
  value,
  sublabel,
  pct,
}: {
  label: string;
  value: string;
  sublabel?: string;
  pct?: number | null;
}) {
  const safePct = pct === null || pct === undefined ? 0 : clampPct(pct);
  return (
    <div className="settings-metric">
      <div className="settings-metric-header">
        <div className="settings-metric-label">{label}</div>
        <div className="settings-metric-value">{value}</div>
      </div>
      <div className="settings-meter-track" role="presentation">
        <div className="settings-meter-fill" style={{ width: `${safePct}%` }} />
      </div>
      {sublabel ? <div className="settings-metric-sub">{sublabel}</div> : null}
    </div>
  );
}
