import { hmacSha256Hex } from "./hash";
import type { TelemetryRow } from "./telemetry-ingest";

export type BlameProductReceipt = Readonly<{
  activity_class: "product_activity" | "product_value";
  analytics_environment: "production" | "staging";
  app_version: string;
  arch: string;
  duration_bucket: string;
  event_id: string;
  identity_key_version: number;
  occurred_at: string;
  os: string;
  outcome: string;
  properties: TelemetryRow["properties"];
  received_at: string;
  replay_fingerprint: string;
  subject_hash: string;
  traffic_class: "unclassified_public" | "synthetic";
}>;

export function hasCurrentBlameProductRow(rows: readonly TelemetryRow[]): boolean {
  return rows.some(isCurrentBlameProductRow);
}

export async function buildBlameProductReceipt(
  rows: readonly TelemetryRow[],
  coordinate: string | undefined,
  identityHmacKey: string,
  identityKeyVersion: number,
): Promise<BlameProductReceipt | undefined> {
  if (coordinate == null) return undefined;
  const currentRows = rows.filter(isCurrentBlameProductRow);
  if (rows.length !== 1 || currentRows.length !== 1) {
    throw new Error("invalid_blame_proof_event_count");
  }
  const row = currentRows[0]!;
  if (
    row.client_profile_id_hash != null
    || row.data_root_id_hash != null
    || row.identity_key_version != null
  ) return undefined;
  if (row.activity_class !== "product_activity" && row.activity_class !== "product_value") {
    throw new Error("invalid_blame_product_activity_class");
  }
  return {
    activity_class: row.activity_class,
    analytics_environment: row.analytics_environment,
    app_version: row.app_version,
    arch: row.arch,
    duration_bucket: row.duration_bucket,
    event_id: row.event_id,
    identity_key_version: identityKeyVersion,
    occurred_at: row.occurred_at,
    os: row.os,
    outcome: row.status,
    properties: row.properties,
    received_at: row.received_at,
    replay_fingerprint: row.payload_fingerprint,
    subject_hash: await hmacSha256Hex(
      identityHmacKey,
      `ctx.telemetry.blame-subject.v1.key-${identityKeyVersion}`,
      coordinate,
    ),
    traffic_class: row.traffic_class,
  };
}

function isCurrentBlameProductRow(row: TelemetryRow): boolean {
  return row.event_name === "operation_completed"
    && row.surface === "pro_host"
    && row.properties.operation === "blame"
    && (
      (
        row.properties.blame_schema_version === 1
        && row.properties.blame_semantics_version === 1
      )
      || row.properties.blame_schema_version === 2
    );
}
