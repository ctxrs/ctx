import { describe, expect, it } from "vitest";
import { readCachedValue, shouldUseCachedValue, writeCachedValue } from "./entitlementsCache";

function createStorage(): Storage {
  const map = new Map<string, string>();
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => {
      map.set(k, v);
    },
    removeItem: (k: string) => {
      map.delete(k);
    },
    clear: () => map.clear(),
    key: (i: number) => Array.from(map.keys())[i] ?? null,
    get length() {
      return map.size;
    },
  } as Storage;
}

describe("entitlementsCache", () => {
  it("writes and reads cached values", () => {
    const storage = createStorage();
    writeCachedValue(storage, "k", { plan_type: "free_local" }, 123);
    const read = readCachedValue<{ plan_type: string }>(storage, "k");
    expect(read?.fetched_at_ms).toBe(123);
    expect(read?.value.plan_type).toBe("free_local");
  });

  it("returns null for invalid JSON", () => {
    const storage = createStorage();
    storage.setItem("k", "{not json");
    expect(readCachedValue(storage, "k")).toBeNull();
  });

  it("respects TTL", () => {
    const storage = createStorage();
    writeCachedValue(storage, "k", { ok: true }, 1000);
    const cache = readCachedValue(storage, "k");
    expect(cache).not.toBeNull();
    expect(shouldUseCachedValue(cache!, 500, 1499)).toBe(true);
    expect(shouldUseCachedValue(cache!, 500, 1500)).toBe(false);
  });
});
