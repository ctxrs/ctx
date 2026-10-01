import { expect } from "vitest";
import { sql } from "./postgres.mjs";

// Execute the production adapter's parameterized statements unchanged, using
// one local PostgreSQL transaction. Only parameter transport differs from Neon.
export function postgresQueryClient(postgres) {
  return {
    async query(statement, params = []) {
      sql(postgres, bindParams(statement, params));
      return [];
    },
    async transaction(build, options) {
      expect(options).toEqual({ isolationLevel: "ReadCommitted" });
      const statements = [];
      const result = await Promise.all(build({
        async query(statement, params = []) {
          statements.push(bindParams(statement, params));
          return [];
        },
      }));
      sql(postgres, `begin isolation level read committed;
        set local role ctx_telemetry_ingest;
        ${statements.join(";\n")}; commit;`);
      return result;
    },
  };
}

function bindParams(statement, params) {
  return statement.replace(/\$(\d+)/gu, (_placeholder, number) => {
    const value = params[Number(number) - 1];
    if (value === null) return "null";
    if (typeof value === "boolean") return value ? "true" : "false";
    if (typeof value === "number" && Number.isFinite(value)) return String(value);
    if (typeof value === "string") return `'${value.replaceAll("'", "''")}'`;
    throw new Error("unsupported_test_sql_parameter");
  });
}
