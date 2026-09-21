#!/usr/bin/env node

import fs from "node:fs";

import {
  loadHostedManagedPairPublication,
  loadRuntimeTransportHandoff,
  validateHostedManagedPairMetadata,
} from "./hosted-managed-pair-release.mjs";
import { renderHostedSemanticMetadataExtension } from "./hosted-semantic-release.mjs";
import {
  verifyRuntimeTransportHandoff,
} from "./verify-runtime-transport-handoff.mjs";

const args = process.argv.slice(2);
const hasSemanticHandoff = args.length === 12
  && args[6] === "--semantic-artifact-dir"
  && args[8] === "--candidate-manifest-handoff"
  && args[10] === "--candidate-handoff-sha256";
const hasRuntimeHandoff = (args.length === 6 || hasSemanticHandoff)
  && args[0] === "--publication"
  && args[2] === "--runtime-handoff"
  && args[4] === "--public-ctx-repo";
if (!hasRuntimeHandoff || !hasSemanticHandoff) {
  throw new Error(
    "usage: validate-hosted-managed-pair-metadata.mjs --publication PATH "
      + "--runtime-handoff PATH --public-ctx-repo PATH "
      + "--semantic-artifact-dir PATH --candidate-manifest-handoff PATH "
      + "--candidate-handoff-sha256 HEX",
  );
}
const chunks = [];
for await (const chunk of process.stdin) chunks.push(chunk);
const metadata = Buffer.concat(chunks);
const loaded = loadHostedManagedPairPublication(args[1]);
// The existing owner validates the current release and exactly five runtime transports.
const runtimeHandoff = loadRuntimeTransportHandoff(args[3], loaded);
verifyRuntimeTransportHandoff(args[5], runtimeHandoff);
const metadataExtension = renderHostedSemanticMetadataExtension(args[7], args[9], args[11]);
validateHostedManagedPairMetadata(metadata, loaded, runtimeHandoff, metadataExtension);
fs.writeSync(process.stdout.fd, "hosted managed-pair metadata: OK\n");
