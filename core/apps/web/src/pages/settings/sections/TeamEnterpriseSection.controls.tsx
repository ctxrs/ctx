import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../../../components/ui/select";
import type { MembershipRole, TeamEnterpriseCloudState } from "../teamEnterpriseSettingsApi";
import { formatRoleLabel, type StatusPillTone } from "./TeamEnterpriseSection.helpers";

export const TEAM_ENTERPRISE_CONTROL_ROW_STYLE = {
  display: "flex",
  gap: 8,
  flexWrap: "wrap" as const,
  justifyContent: "flex-end" as const,
};

export function StatusPill({
  label,
  tone = "neutral",
  mono = false,
}: {
  label: string;
  tone?: StatusPillTone;
  mono?: boolean;
}) {
  const toneClassName =
    tone === "ok"
      ? " settings-pill-ok"
      : tone === "warn"
        ? " settings-pill-warn"
        : tone === "err"
          ? " settings-pill-err"
          : "";
  return <span className={`settings-pill${toneClassName}${mono ? " wb-mono" : ""}`}>{label}</span>;
}

export function OrganizationSwitcher({
  cloudState,
  disabled,
  onSelectOrg,
}: {
  cloudState: TeamEnterpriseCloudState;
  disabled: boolean;
  onSelectOrg: (orgId: string) => void | Promise<void>;
}) {
  if (cloudState.orgs.length === 0 || !cloudState.activeOrgId) {
    return <StatusPill label="No org" tone="warn" />;
  }
  if (cloudState.orgs.length === 1) {
    return (
      <div style={TEAM_ENTERPRISE_CONTROL_ROW_STYLE}>
        <StatusPill label={cloudState.activeOrg?.name ?? cloudState.activeOrgId} />
        <StatusPill label={formatRoleLabel(cloudState.activeOrg?.role)} />
      </div>
    );
  }
  return (
    <Select value={cloudState.activeOrgId} disabled={disabled} onValueChange={(value) => void onSelectOrg(value)}>
      <SelectTrigger className="settings-control settings-select tw-min-w-[14rem]" aria-label="Active organization">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {cloudState.orgs.map((org) => (
          <SelectItem key={org.id} value={org.id}>
            {org.name}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

export function RoleSelect({
  value,
  disabled,
  onChange,
}: {
  value: MembershipRole;
  disabled: boolean;
  onChange: (value: MembershipRole) => void;
}) {
  return (
    <Select value={value} disabled={disabled} onValueChange={(next) => onChange(next as MembershipRole)}>
      <SelectTrigger className="settings-control settings-select tw-min-w-[8rem]" aria-label="Invite role">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="member">Member</SelectItem>
        <SelectItem value="admin">Admin</SelectItem>
        <SelectItem value="owner">Owner</SelectItem>
      </SelectContent>
    </Select>
  );
}

export function PolicySelect<T extends string>({
  ariaLabel,
  value,
  disabled,
  onChange,
  options,
}: {
  ariaLabel: string;
  value: T;
  disabled: boolean;
  onChange: (value: T) => void;
  options: Array<{ value: T; label: string }>;
}) {
  return (
    <Select value={value} disabled={disabled} onValueChange={(next) => onChange(next as T)}>
      <SelectTrigger className="settings-control settings-select tw-min-w-[12rem]" aria-label={ariaLabel}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {options.map((option) => (
          <SelectItem key={option.value} value={option.value}>
            {option.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
