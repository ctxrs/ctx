import { SESSION_THREAD_GEOMETRY_REVISION } from "./sessionThreadGeometrySpec";
import { clearSessionMarkdownMeasurementCaches as clearMarkdownMeasurementCaches } from "./sessionMarkdownMeasurementCore";
import { clearSessionPlainTextMeasurementCaches } from "./sessionPlainTextMeasurement";
import { clearSessionTextMeasurementCaches } from "./sessionTextMeasurement";

const SESSION_TRANSCRIPT_LAYOUT_PLANNER_REVISION = "planner-v2";

export const SESSION_TRANSCRIPT_LAYOUT_ENGINE_REVISION =
  `${SESSION_TRANSCRIPT_LAYOUT_PLANNER_REVISION}:${SESSION_THREAD_GEOMETRY_REVISION}`;

export function clearSessionMarkdownMeasurementCaches(): void {
  clearMarkdownMeasurementCaches();
  clearSessionPlainTextMeasurementCaches();
  clearSessionTextMeasurementCaches();
}

export { measureSessionMarkdownDocument } from "./sessionMarkdownBlockMeasurement";
