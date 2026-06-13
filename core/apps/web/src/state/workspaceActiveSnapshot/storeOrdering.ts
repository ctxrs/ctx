import type { WorkspaceActiveSnapshotItem } from "./storeTypes";

export function findWorkspaceActiveSnapshotInsertIndex(
  tasks: Map<string, WorkspaceActiveSnapshotItem>,
  order: string[],
  sortAt: number,
  id: string,
): number {
  let low = 0;
  let high = order.length;
  while (low < high) {
    const mid = Math.floor((low + high) / 2);
    const midId = order[mid];
    const midItem = tasks.get(midId);
    const midSort = midItem?.sortAtMs ?? 0;
    if (sortAt > midSort || (sortAt === midSort && id > midId)) {
      high = mid;
    } else {
      low = mid + 1;
    }
  }
  return low;
}
