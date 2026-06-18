import fs from "node:fs";

const files = process.argv.slice(2);
if (files.length === 0) {
  throw new Error("expected schema files as argv");
}

const seen = new Set();
for (const file of files) {
  const normalized = String(file || "").trim();
  if (!normalized) continue;
  seen.add(normalized);
  const parsed = JSON.parse(fs.readFileSync(normalized, "utf8"));
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error(`${normalized} must parse to an object`);
  }
  if (typeof parsed.$schema !== "string" && typeof parsed.$ref !== "string") {
    throw new Error(`${normalized} must declare $schema or $ref`);
  }
}

if (seen.size !== files.length) {
  throw new Error("schema file arguments must be unique");
}
