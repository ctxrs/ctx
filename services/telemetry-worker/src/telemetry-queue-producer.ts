import { TelemetryIngestError } from "./telemetry-ingest";
import {
  encodeTelemetryQueueMessage,
  TelemetryQueueMessageTooLargeError,
  type TelemetryQueueMessage,
} from "./telemetry-queue";

export const TELEMETRY_QUEUE_BATCH_MAX_MESSAGES = 100;
// Cloudflare limits a batch to 256 KB. 240,000 encoded body bytes preserves
// 16,000 bytes for the batch-array/request wrapper (320 bytes per current
// 50-event request) while remaining below either decimal or binary 256 KB.
export const TELEMETRY_QUEUE_BATCH_MAX_ENCODED_BYTES = 240_000;

export type TelemetryQueueBatchEntry = Readonly<{
  body: Uint8Array<ArrayBuffer>;
  contentType: "bytes";
}>;

export type TelemetryQueueProducer = Readonly<{
  sendBatch(messages: Iterable<TelemetryQueueBatchEntry>): Promise<unknown>;
}>;

export function telemetryQueueBatchChunks(
  messages: readonly TelemetryQueueBatchEntry[],
): readonly (readonly TelemetryQueueBatchEntry[])[] {
  const chunks: TelemetryQueueBatchEntry[][] = [];
  let chunk: TelemetryQueueBatchEntry[] = [];
  let chunkBytes = 0;
  for (const message of messages) {
    if (message.body.byteLength > TELEMETRY_QUEUE_BATCH_MAX_ENCODED_BYTES) {
      throw new TelemetryIngestError(413, "queue_message_too_large");
    }
    if (
      chunk.length === TELEMETRY_QUEUE_BATCH_MAX_MESSAGES
      || chunkBytes + message.body.byteLength > TELEMETRY_QUEUE_BATCH_MAX_ENCODED_BYTES
    ) {
      chunks.push(chunk);
      chunk = [];
      chunkBytes = 0;
    }
    chunk.push(message);
    chunkBytes += message.body.byteLength;
  }
  if (chunk.length > 0) chunks.push(chunk);
  return chunks;
}

export async function confirmTelemetryQueueBatchAdmission(
  queue: TelemetryQueueProducer,
  batches: readonly (readonly TelemetryQueueBatchEntry[])[],
): Promise<void> {
  const results = await Promise.allSettled(batches.map((batch) => queue.sendBatch(batch)));
  const failed = results.find((result): result is PromiseRejectedResult => result.status === "rejected");
  if (failed) throw failed.reason;
}

export async function admitQueueMessages(
  queue: TelemetryQueueProducer,
  messages: readonly TelemetryQueueMessage[],
): Promise<void> {
  try {
    const encoded = await Promise.all(messages.map(async (message) => ({
      body: new Uint8Array(await encodeTelemetryQueueMessage(message)),
      contentType: "bytes" as const,
    })));
    await confirmTelemetryQueueBatchAdmission(queue, telemetryQueueBatchChunks(encoded));
  } catch (error) {
    if (error instanceof TelemetryQueueMessageTooLargeError) {
      throw new TelemetryIngestError(413, error.message);
    }
    throw error;
  }
}

export async function admitQueueMessage(
  queue: TelemetryQueueProducer,
  message: TelemetryQueueMessage,
): Promise<void> {
  await admitQueueMessages(queue, [message]);
}
