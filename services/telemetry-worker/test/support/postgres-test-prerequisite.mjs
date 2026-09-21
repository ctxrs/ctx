import { execFileSync } from "node:child_process";
import { accessSync, constants } from "node:fs";
import path from "node:path";

// The behavioral target is external: its Linux runner provisions PostgreSQL.
// Missing or incompatible tools invalidate evidence instead of skipping SQL.
export function requirePostgres() {
  const directory = process.env.CTX_TEST_POSTGRES_BIN;
  if (!directory || !path.isAbsolute(directory)) {
    throw new Error("CTX_TEST_POSTGRES_BIN must name an absolute PostgreSQL 16+ bin directory");
  }
  for (const tool of ["initdb", "pg_ctl", "postgres", "psql"]) {
    try {
      accessSync(path.join(directory, tool), constants.X_OK);
    } catch (cause) {
      throw new Error(`required PostgreSQL test tool is unavailable: ${tool} in ${directory}`, { cause });
    }
  }
  const version = execFileSync(path.join(directory, "postgres"), ["--version"], {
    encoding: "utf8",
    timeout: 5000,
    maxBuffer: 4096,
  });
  const major = /^postgres \(PostgreSQL\) (\d+)\./u.exec(version)?.[1];
  if (!major || Number(major) < 16) {
    throw new Error(`PostgreSQL 16+ is required for SQL assertions: ${version.trim()}`);
  }
  return directory;
}
