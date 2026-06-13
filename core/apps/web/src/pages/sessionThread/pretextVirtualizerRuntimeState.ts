import type { PretextVirtualizerSnapshot } from "@pretext-virtualizer/core";
import type { WorkbenchListItem } from "../SessionPage.types";
import {
  noteSessionPretextRuntimeSnapshot,
  type SessionPretextRuntimeRecord,
} from "./pretextSessionRuntimeCache";
import { noteSessionTranscriptWarmViewport } from "./sessionTranscriptWarmState";

export function getRenderedItemsFromSnapshot(
  snapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
  listItems: readonly WorkbenchListItem[],
): readonly WorkbenchListItem[] {
  return snapshot.visibleItems.map(
    (visibleItem) => listItems[visibleItem.index] ?? visibleItem.item,
  );
}

export function commitSessionThreadRuntimeSnapshot(
  runtime: SessionPretextRuntimeRecord,
  snapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
  listItems: readonly WorkbenchListItem[],
  preparedKeys?: {
    sourceKey?: string | null;
    layoutKey?: string | null;
  },
): void {
  noteSessionPretextRuntimeSnapshot(runtime, snapshot, listItems, preparedKeys);
  noteSessionTranscriptWarmViewport({
    width: snapshot.viewportWidth,
    height: snapshot.viewportHeight,
  });
}
