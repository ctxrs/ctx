export const validSetupReceipt = ({
  schemaVersion = 3,
  initialized = true,
  mode = "ready",
  indexedSessions = 3,
  indexedItems = 30,
} = {}) => JSON.stringify({
  schema_version: schemaVersion,
  initialized,
  mode,
  indexed_sessions: indexedSessions,
  indexed_items: indexedItems,
}, null, 2);

// Native reader inputs: released CLI 1.3.1 emits 2; the new producer emits 3.
// These expected admissions are independent of either installer predicate.
export function setupSchemaReaderCases() {
  const cases = [];
  for (const schemaVersion of [2, 3]) {
    const receipt = (options = {}) => validSetupReceipt({ schemaVersion, ...options });
    cases.push({ name: `native setup schema ${schemaVersion}`, receipt: receipt(), accepted: true });
    for (const [name, invalid, parsedMode = "invalid"] of [
      ["string initialized", receipt({ initialized: "true" })],
      ["string count", receipt({ indexedSessions: "3" }), "ready"],
      ["negative count", receipt({ indexedItems: -1 }), "ready"],
      ["unknown mode", receipt({ mode: "unknown" })],
      ["missing initialized", receipt().replace(/\s*"initialized": true,/u, "")],
      ["duplicate mode", receipt().replace('"mode": "ready"', '"mode": "ready", "mode": "ready"')],
      ["trailing output", `${receipt()}\ntrailing`],
    ]) cases.push({ name: `schema ${schemaVersion}: ${name}`, receipt: invalid, accepted: false, parsedMode });
  }
  for (const schemaVersion of [0, 1, 4, -1, 2.5, "2", "3", null]) {
    cases.push({ name: `unsupported setup schema ${JSON.stringify(schemaVersion)}`,
      receipt: validSetupReceipt({ schemaVersion }), accepted: false });
  }
  return cases;
}
