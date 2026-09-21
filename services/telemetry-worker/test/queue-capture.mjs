import { decodeTelemetryQueueMessage } from "../src/telemetry-queue";

export function withCapturedQueue(worker, captures) {
  return {
    ...worker,
    async fetch(request, env, context) {
      const rows = [];
      const queue = {
        async sendBatch(entries) {
          for (const { body } of entries) {
            const message = await decodeTelemetryQueueMessage(body);
            if (message == null) throw new Error("invalid_test_queue_message");
            if (message.kind === "telemetry_row") rows.push(message.row);
            if (message.kind === "install_stage_row") captures.installWrites?.push(message.row);
            if (message.kind === "blame_product_receipt") captures.receiptWrites?.push(message.receipt);
          }
        },
      };
      const response = await worker.fetch(
        request,
        { ...env, TELEMETRY_INGEST_QUEUE: queue },
        context,
      );
      if (rows.length > 0) captures.telemetryWrites?.push(rows);
      return response;
    },
  };
}
