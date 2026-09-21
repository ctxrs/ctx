import type { InstallStageRow, TelemetryRow } from "./telemetry-ingest";
import type { BlameProductReceipt } from "./blame-product-receipt";

export type TelemetryIngestEndpoint = "telemetry_batch" | "install_stage";
export type TelemetryEventFamily =
  | "batch"
  | "analytics_delivery_observation"
  | "cli_invocation"
  | "operation_completed"
  | "provider_refresh_completed"
  | "runtime_observation"
  | "install_stage"
  | "unknown";
export type TelemetryRejectionClass =
  | "body_too_large"
  | "invalid_json"
  | "invalid_envelope"
  | "invalid_identity"
  | "invalid_event"
  | "invalid_properties"
  | "too_many_events"
  | "event_collision"
  | "queue_admission"
  | "rate_limited"
  | "database"
  | "other";
export type PersistedTelemetryRejectionClass = Exclude<
  TelemetryRejectionClass,
  "queue_admission" | "rate_limited" | "database"
>;

export type TelemetryIngestRejection = Readonly<{
  analytics_environment: "production" | "staging";
  endpoint: TelemetryIngestEndpoint;
  event_family: TelemetryEventFamily;
  rejection_class: PersistedTelemetryRejectionClass;
  rejection_code: string;
  app_version: string;
  field_shape_fingerprint: string;
  field_shape_overflow: boolean;
  provider_classification: string;
  size_bucket: string;
}>;

export type TelemetryIngestHealthSnapshot = Readonly<{
  compatibilityRejectionMax: bigint;
  deliveryDegradedCount: bigint;
  deliveryDroppedCount: bigint;
  eventCollisionCount: bigint;
  otherRejectionCount: bigint;
  providerRefreshFailureCount: bigint;
}>;

export interface TelemetryDatabase {
  insertTelemetryRows(rows: readonly TelemetryRow[]): Promise<void>;
  insertBlameProductReceipt(receipt: BlameProductReceipt): Promise<void>;
  insertInstallStageRow(row: InstallStageRow): Promise<void>;
  readIngestHealthSnapshot(
    analyticsEnvironment: "production" | "staging",
  ): Promise<TelemetryIngestHealthSnapshot>;
  recordIngestRejection?(
    rejection: TelemetryIngestRejection,
    collisionReceiptId?: string,
  ): Promise<void>;
}

export type TelemetryHistoryMaterialization = Readonly<{
  materialized_count: string;
  first_received_date: string;
  last_received_date: string;
}>;

export interface TelemetryMaintenanceDatabase {
  deleteExpiredTelemetryEventCollisionReceipts(): Promise<string>;
  materializeProductTelemetryHistory(): Promise<TelemetryHistoryMaterialization>;
}

export type TelemetryDatabaseFactory = (
  databaseUrl: string,
) => Promise<TelemetryDatabase> | TelemetryDatabase;

export type TelemetryMaintenanceDatabaseFactory = (
  databaseUrl: string,
) => Promise<TelemetryMaintenanceDatabase> | TelemetryMaintenanceDatabase;

type NeonParam = string | number | boolean | null;
type NeonRow = Record<string, unknown>;
type NeonTransactionQueryFunction = {
  query<T extends NeonRow = NeonRow>(
    query: string,
    params?: readonly NeonParam[],
  ): Promise<T[]>;
};
export type NeonQueryClient = NeonTransactionQueryFunction & {
  transaction(
    build: (sql: NeonTransactionQueryFunction) => Promise<NeonRow[]>[],
    options?: { isolationLevel: "ReadCommitted" },
  ): Promise<NeonRow[][]>;
};
type NeonModule = { neon(connectionString: string): NeonQueryClient };

type ColumnSpec<Row> = {
  name: string;
  value: (row: Row) => NeonParam;
  cast?: "jsonb" | "timestamptz";
};

export class TelemetryEventCollisionError extends Error {
  constructor() {
    super("event_id_collision");
  }
}

export const TELEMETRY_INSERT_TABLE = "ctx.telemetry_event";
export const INSTALL_ATTEMPT_INSERT_TABLE = "ctx.install_attempt_event";
const COLLISION_RECEIPT_CLEANUP_BATCH_SIZE = 128;
const COLLISION_RECEIPT_CLEANUP_MAX_BATCHES = 8;

const TELEMETRY_COLUMNS: readonly ColumnSpec<TelemetryRow>[] = [
  column("event_id", (row) => row.event_id),
  column("install_id_hash", (row) => row.install_id_hash),
  column("device_id_hash", (row) => row.device_id_hash),
  column("broker_install_id_hash", (row) => row.broker_install_id_hash),
  column("broker_device_id_hash", (row) => row.broker_device_id_hash),
  column("origin_install_id_hash", (row) => row.origin_install_id_hash),
  column("origin_device_id_hash", (row) => row.origin_device_id_hash),
  column("occurred_at", (row) => row.occurred_at, "timestamptz"),
  column("received_at", (row) => row.received_at, "timestamptz"),
  column("event_name", (row) => row.event_name),
  column("event_version", (row) => row.event_version),
  column("schema_version", (row) => row.schema_version),
  column("plane", (row) => row.plane),
  column("broker_runtime", (row) => row.broker_runtime),
  column("origin_runtime", (row) => row.origin_runtime),
  column("source", (row) => row.source),
  column("analytics_environment", (row) => row.analytics_environment),
  column("traffic_class", (row) => row.traffic_class),
  column("activity_class", (row) => row.activity_class),
  column("app_version", (row) => row.app_version),
  column("os", (row) => row.os),
  column("arch", (row) => row.arch),
  column("surface", (row) => row.surface),
  column("env_target", (row) => row.env_target),
  column("provider_id", (row) => row.provider_id),
  column("model_id", (row) => row.model_id),
  column("duration_ms", (row) => row.duration_ms),
  column("duration_bucket", (row) => row.duration_bucket),
  column("status", (row) => row.status),
  column("success", (row) => row.success),
  column("session_root_kind", (row) => row.session_root_kind),
  column("client_profile_id_hash", (row) => row.client_profile_id_hash),
  column("data_root_id_hash", (row) => row.data_root_id_hash),
  column("identity_key_version", (row) => row.identity_key_version),
  column("payload_fingerprint", (row) => row.payload_fingerprint),
  column("properties", (row) => JSON.stringify(row.properties), "jsonb"),
];

const INSTALL_COLUMNS: readonly ColumnSpec<InstallStageRow>[] = [
  column("event_id", (row) => row.event_id),
  column("event_name", (row) => row.event_name),
  column("event_version", (row) => row.event_version),
  column("schema_version", (row) => row.schema_version),
  column("install_attempt_id_hash", (row) => row.install_attempt_id_hash),
  column("payload_fingerprint", (row) => row.payload_fingerprint),
  column("occurred_at", (row) => row.occurred_at, "timestamptz"),
  column("received_at", (row) => row.received_at, "timestamptz"),
  column("analytics_environment", (row) => row.analytics_environment),
  column("traffic_class", (row) => row.traffic_class),
  column("stage", (row) => row.stage),
  column("status", (row) => row.status),
  column("error_kind", (row) => row.error_kind),
  column("platform", (row) => row.platform),
  column("arch", (row) => row.arch),
  column("script_family", (row) => row.script_family),
  column("channel", (row) => row.channel),
  column("version", (row) => row.version),
  column("duration_bucket", (row) => row.duration_bucket),
];

export async function createNeonTelemetryDatabase(
  databaseUrl: string,
): Promise<TelemetryDatabase> {
  return new NeonTelemetryDatabase(await createNeonQueryClient(databaseUrl));
}

export async function createNeonTelemetryMaintenanceDatabase(
  databaseUrl: string,
): Promise<TelemetryMaintenanceDatabase> {
  return new NeonTelemetryMaintenanceDatabase(await createNeonQueryClient(databaseUrl));
}

export class NeonTelemetryDatabase implements TelemetryDatabase {
  constructor(private readonly sql: NeonQueryClient) {}

  async readIngestHealthSnapshot(
    analyticsEnvironment: "production" | "staging",
  ): Promise<TelemetryIngestHealthSnapshot> {
    const rows = await this.sql.query<{
      compatibility_rejection_max: string;
      delivery_degraded_count: string;
      delivery_dropped_count: string;
      event_collision_count: string;
      other_rejection_count: string;
      provider_refresh_failure_count: string;
    }>(`
      SELECT
        compatibility_rejection_max::text,
        delivery_degraded_count::text,
        delivery_dropped_count::text,
        event_collision_count::text,
        other_rejection_count::text,
        provider_refresh_failure_count::text
      FROM ctx.telemetry_ingest_health_snapshot($1)
    `, [analyticsEnvironment]);
    const row = rows[0];
    if (!row) throw new Error("invalid_telemetry_ingest_health_result");
    return {
      compatibilityRejectionMax: parseCount(row.compatibility_rejection_max),
      deliveryDegradedCount: parseCount(row.delivery_degraded_count),
      deliveryDroppedCount: parseCount(row.delivery_dropped_count),
      eventCollisionCount: parseCount(row.event_collision_count),
      otherRejectionCount: parseCount(row.other_rejection_count),
      providerRefreshFailureCount: parseCount(row.provider_refresh_failure_count),
    };
  }

  async recordIngestRejection(
    rejection: TelemetryIngestRejection,
    collisionReceiptId?: string,
  ): Promise<void> {
    if (rejection.rejection_class === "event_collision") {
      if (
        rejection.rejection_code !== "event_id_collision"
        || !collisionReceiptId
        || !/^[0-9a-f]{64}$/u.test(collisionReceiptId)
      ) throw new Error("invalid_telemetry_collision_receipt");
      await this.sql.query(
        `
          SELECT ctx.record_telemetry_event_collision(
            $1, $2, $3, $4, $5, $6, $7, $8, $9
          )
        `,
        [
          collisionReceiptId,
          rejection.analytics_environment,
          rejection.endpoint,
          rejection.event_family,
          rejection.app_version,
          rejection.field_shape_fingerprint,
          rejection.field_shape_overflow,
          rejection.provider_classification,
          rejection.size_bucket,
        ],
      );
      return;
    }
    if (collisionReceiptId !== undefined) {
      throw new Error("unexpected_telemetry_collision_receipt");
    }
    await this.sql.query(
      `
        SELECT ctx.record_telemetry_ingest_rejection(
          $1, $2, $3, $4, $5, $6, $7, $8, $9, $10
        )
      `,
      [
        rejection.analytics_environment,
        rejection.endpoint,
        rejection.event_family,
        rejection.rejection_class,
        rejection.rejection_code,
        rejection.app_version,
        rejection.field_shape_fingerprint,
        rejection.field_shape_overflow,
        rejection.provider_classification,
        rejection.size_bucket,
      ],
    );
  }

  async insertTelemetryRows(rows: readonly TelemetryRow[]): Promise<void> {
    if (rows.length === 0) return;
    const { params, valuesSql } = buildValues(rows, TELEMETRY_COLUMNS);
    const fingerprints = rows.map((row) => [row.event_id, row.payload_fingerprint] as const);
    try {
      await this.sql.transaction(
        (transaction) => [
          transaction.query(
            `
              INSERT INTO ${TELEMETRY_INSERT_TABLE} (${columnNames(TELEMETRY_COLUMNS)})
              VALUES ${valuesSql}
              ON CONFLICT (event_id) DO NOTHING
            `,
            params,
          ),
          transaction.query(
            collisionCheckSql(TELEMETRY_INSERT_TABLE, fingerprints),
            flatten(fingerprints),
          ),
        ],
        { isolationLevel: "ReadCommitted" },
      );
    } catch (error) {
      if (isCollisionGuardViolation(error)) throw new TelemetryEventCollisionError();
      throw error;
    }
  }

  async insertBlameProductReceipt(receipt: BlameProductReceipt): Promise<void> {
    try {
      await this.sql.query(
        "SELECT ctx.record_blame_product_receipt($1::jsonb)",
        [JSON.stringify(receipt)],
      );
    } catch (error) {
      if (isCollisionGuardViolation(error)) throw new TelemetryEventCollisionError();
      throw error;
    }
  }

  async insertInstallStageRow(row: InstallStageRow): Promise<void> {
    const { params, valuesSql } = buildValues([row], INSTALL_COLUMNS);
    const insertSql = `
      INSERT INTO ${INSTALL_ATTEMPT_INSERT_TABLE} (${columnNames(INSTALL_COLUMNS)})
      VALUES ${valuesSql}
      ON CONFLICT (event_id) WHERE event_id IS NOT NULL DO NOTHING
    `;
    if (row.event_id === null || row.payload_fingerprint === null) {
      await this.sql.query(insertSql, params);
      return;
    }
    const fingerprints = [[row.event_id, row.payload_fingerprint] as const];
    try {
      await this.sql.transaction(
        (transaction) => [
          transaction.query(insertSql, params),
          transaction.query(
            collisionCheckSql(INSTALL_ATTEMPT_INSERT_TABLE, fingerprints),
            flatten(fingerprints),
          ),
        ],
        { isolationLevel: "ReadCommitted" },
      );
    } catch (error) {
      if (isCollisionGuardViolation(error)) throw new TelemetryEventCollisionError();
      throw error;
    }
  }
}

export class NeonTelemetryMaintenanceDatabase implements TelemetryMaintenanceDatabase {
  constructor(private readonly sql: NeonQueryClient) {}

  async deleteExpiredTelemetryEventCollisionReceipts(): Promise<string> {
    const batchSize = BigInt(COLLISION_RECEIPT_CLEANUP_BATCH_SIZE);
    let totalDeleted = 0n;

    for (
      let batchCount = 0;
      batchCount < COLLISION_RECEIPT_CLEANUP_MAX_BATCHES;
      batchCount += 1
    ) {
      const rows = await this.sql.query<{ deleted_count: string }>(`
        SELECT ctx.delete_expired_telemetry_event_collision_receipts(
          clock_timestamp() - interval '9 days',
          ${COLLISION_RECEIPT_CLEANUP_BATCH_SIZE}
        )::text AS deleted_count
      `);
      const deletedCount = rows[0]?.deleted_count;
      if (typeof deletedCount !== "string" || !/^(?:0|[1-9]\d*)$/u.test(deletedCount)) {
        throw new Error("invalid_telemetry_collision_receipt_cleanup_result");
      }
      const deletedInBatch = BigInt(deletedCount);
      if (deletedInBatch > batchSize) {
        throw new Error("invalid_telemetry_collision_receipt_cleanup_result");
      }
      totalDeleted += deletedInBatch;
      if (deletedInBatch < batchSize) break;
    }

    return totalDeleted.toString();
  }

  async materializeProductTelemetryHistory(): Promise<TelemetryHistoryMaterialization> {
    // Four days in the primary Queue plus four in its DLQ, then the next daily
    // Cron. Older gaps require explicit backfill with the same idempotent SQL.
    // One statement keeps a stable UTC anchor and commits the window atomically.
    const rows = await this.sql.query<TelemetryHistoryMaterialization>(`
      WITH days AS (
        SELECT (statement_timestamp() AT TIME ZONE 'utc')::date - age AS received_date
        FROM generate_series(1, 9) AS ages(age)
      )
      SELECT sum(ctx.materialize_product_telemetry_history(received_date))::text
          AS materialized_count,
        min(received_date)::text AS first_received_date,
        max(received_date)::text AS last_received_date
      FROM days
    `);
    const result = rows[0];
    if (
      rows.length !== 1
      || !result
      || typeof result.materialized_count !== "string"
      || !/^(?:0|[1-9]\d*)$/u.test(result.materialized_count)
      || !/^\d{4}-\d{2}-\d{2}$/u.test(result.first_received_date)
      || !/^\d{4}-\d{2}-\d{2}$/u.test(result.last_received_date)
    ) {
      throw new Error("invalid_telemetry_materialization_result");
    }
    return {
      materialized_count: result.materialized_count,
      first_received_date: result.first_received_date,
      last_received_date: result.last_received_date,
    };
  }
}

async function createNeonQueryClient(databaseUrl: string): Promise<NeonQueryClient> {
  const neonModule = await import("@neondatabase/serverless") as unknown as NeonModule;
  return neonModule.neon(databaseUrl);
}

function column<Row>(
  name: string,
  value: (row: Row) => NeonParam,
  cast?: "jsonb" | "timestamptz",
): ColumnSpec<Row> {
  return cast ? { name, value, cast } : { name, value };
}

function buildValues<Row>(
  rows: readonly Row[],
  columns: readonly ColumnSpec<Row>[],
): { params: NeonParam[]; valuesSql: string } {
  const params: NeonParam[] = [];
  const valuesSql = rows.map((row) => {
    const placeholders = columns.map((spec) => {
      params.push(spec.value(row));
      const placeholder = `$${params.length}`;
      return spec.cast ? `${placeholder}::${spec.cast}` : placeholder;
    });
    return `(${placeholders.join(", ")})`;
  }).join(", ");
  return { params, valuesSql };
}

function columnNames<Row>(columns: readonly ColumnSpec<Row>[]): string {
  return columns.map((spec) => spec.name).join(", ");
}

function collisionCheckSql(
  table: string,
  fingerprints: readonly (readonly [string, string])[],
): string {
  const values = fingerprints.map((_, index) => `($${index * 2 + 1}, $${index * 2 + 2})`).join(", ");
  return `
    WITH incoming(event_id, payload_fingerprint) AS (VALUES ${values})
    SELECT 1 / CASE
      WHEN COUNT(*) = ${fingerprints.length}
        AND BOOL_AND(
          -- Rows accepted before migration 0017 have no fingerprint. The
          -- conflict insert is immutable, so replaying one is safe; every
          -- non-null mismatch remains an event-ID collision.
          stored.payload_fingerprint IS NULL
          OR stored.payload_fingerprint = incoming.payload_fingerprint
        )
        THEN 1
      ELSE 0
    END AS collision_guard
    FROM incoming
    JOIN ${table} AS stored USING (event_id)
  `;
}

function flatten(
  fingerprints: readonly (readonly [string, string])[],
): NeonParam[] {
  return fingerprints.flatMap(([eventId, fingerprint]) => [eventId, fingerprint]);
}

function isCollisionGuardViolation(error: unknown): boolean {
  return isRecord(error) && error.code === "22012";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function parseCount(value: unknown): bigint {
  if (typeof value !== "string" || !/^(?:0|[1-9]\d*)$/u.test(value)) {
    throw new Error("invalid_telemetry_ingest_health_result");
  }
  return BigInt(value);
}
