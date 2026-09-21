import { describe, expect, test } from "vitest";

import worker, { type Env, type ReleaseBucket, type ReleaseGetOptions, type ReleaseObject, type ReleaseRange } from "../src/worker";

describe("release api worker", () => {
  test("v2 selects current metadata while original aliases retain the bridge", async () => {
    const pointer = (version: string) => fakeJson({
      channel: "stable", contract: "ctx-cli-release-pointer", schema_version: 1, version,
      metadata_object: `releases/stable/${version}/ctx-release-metadata.env`, metadata_sha256: "a".repeat(64),
      signature_object: `releases/stable/${version}/ctx-release-metadata.env.sig`, signature_sha256: "b".repeat(64),
    });
    const objects = {
      "releases/stable/current.json": pointer("1.3.2"),
      "releases/stable/current-v2.json": pointer("1.3.3"),
      "releases/stable/1.3.2/ctx-release-metadata.env": "bridge",
      "releases/stable/1.3.2/ctx-release-metadata.env.sig": "bridge-signature",
      "releases/stable/1.3.3/ctx-release-metadata.env": "current",
      "releases/stable/1.3.3/ctx-release-metadata.env.sig": "current-signature",
    };
    for (const [prefix, expected] of [["functions/v1/", "bridge"], ["", "bridge"], ["functions/v2/", "current"]]) {
      for (const suffix of ["", ".sig"]) {
        const bucket = new FakeReleaseBucket(objects);
        const response = await worker.fetch(new Request(
          `https://cli.ctx.rs/${prefix}releases/stable/ctx-release-metadata.env${suffix}`,
        ), testEnv(bucket));
        expect(response.status).toBe(200);
        expect(await response.text()).toBe(`${expected}${suffix ? "-signature" : ""}`);
        expect(bucket.getCalls[0]).toBe(`releases/stable/${prefix === "functions/v2/" ? "current-v2.json" : "current.json"}`);
      }
    }
    const bucket = new FakeReleaseBucket(objects);
    const head = await worker.fetch(new Request(
      "https://cli.ctx.rs/functions/v2/releases/stable/ctx-release-metadata.env", { method: "HEAD" },
    ), testEnv(bucket));
    expect(head.status).toBe(200);
    expect(await head.text()).toBe("");
    expect(bucket.headCalls).toEqual(["releases/stable/1.3.3/ctx-release-metadata.env"]);
  });

  test("v2 never falls back to the original pointer or direct metadata", async () => {
    for (const broken of [undefined, "not-json", fakeJson({ channel: "staging" })]) {
      const bucket = new FakeReleaseBucket({
        "releases/stable/ctx-release-metadata.env": "old-direct",
        ...(broken === undefined ? {} : { "releases/stable/current-v2.json": broken }),
      });
      const response = await worker.fetch(new Request(
        "https://cli.ctx.rs/functions/v2/releases/stable/ctx-release-metadata.env",
      ), testEnv(bucket));
      expect(response.status).toBe(broken === undefined ? 404 : 500);
      expect(bucket.getCalls).toEqual(["releases/stable/current-v2.json"]);
    }
  });

  test("v2 is limited to stable metadata; existing staging and immutable routes remain v1", async () => {
    const bucket = new FakeReleaseBucket({ "releases/staging/ctx-release-metadata.env": "staging" });
    for (const path of [
      "releases/staging/ctx-release-metadata.env", "releases/dogfood-test/ctx-release-metadata.env",
      "releases/stable/1.3.2/ctx-release-metadata.env", "releases/stable/current.json",
      "download/stable/1.3.2/ctx", "provider-matrix/stable/latest.json",
    ]) {
      const response = await worker.fetch(new Request(`https://cli.ctx.rs/functions/v2/${path}`), testEnv(bucket));
      expect(response.status).toBe(404);
    }
    expect(bucket.getCalls).toEqual([]);
    const staging = await worker.fetch(new Request(
      "https://cli.ctx.rs/functions/v1/releases/staging/ctx-release-metadata.env",
    ), testEnv(bucket));
    expect(await staging.text()).toBe("staging");
  });

  test("serves release manifests from R2 with compatibility cache headers", async () => {
    const bucket = new FakeReleaseBucket({
      "releases/stable/latest.json": fakeJson({
        channel: "stable",
        latest_version: "1.2.3",
      }),
    });

    const response = await worker.fetch(
      new Request("https://api.ctx.rs/functions/v1/releases/stable/latest.json", {
        headers: { origin: "https://ctx.rs" },
      }),
      testEnv(bucket),
    );

    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toBe("application/json; charset=utf-8");
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(response.headers.get("access-control-allow-origin")).toBe("https://ctx.rs");
    expect(await response.json()).toEqual({ channel: "stable", latest_version: "1.2.3" });
    expect(bucket.getCalls).toEqual(["releases/stable/latest.json"]);
    expect(bucket.headCalls).toEqual([]);
  });

  test("selects signed CLI metadata through one atomic current pointer", async () => {
    const pointer = fakeJson({
      channel: "stable",
      contract: "ctx-cli-release-pointer",
      metadata_object: "releases/stable/1.0.0/ctx-release-metadata.env",
      metadata_sha256: "a".repeat(64),
      schema_version: 1,
      signature_object: "releases/stable/1.0.0/ctx-release-metadata.env.sig",
      signature_sha256: "b".repeat(64),
      version: "1.0.0",
    });
    const bucket = new FakeReleaseBucket({
      "releases/stable/current.json": pointer,
      "releases/stable/1.0.0/ctx-release-metadata.env": "versioned-metadata",
      "releases/stable/1.0.0/ctx-release-metadata.env.sig": "versioned-signature",
      "releases/stable/ctx-release-metadata.env": "stale-metadata",
    });

    const metadata = await worker.fetch(
      new Request("https://cli.ctx.rs/functions/v1/releases/stable/ctx-release-metadata.env"),
      testEnv(bucket),
    );
    const signature = await worker.fetch(
      new Request("https://cli.ctx.rs/functions/v1/releases/stable/ctx-release-metadata.env.sig"),
      testEnv(bucket),
    );

    expect(await metadata.text()).toBe("versioned-metadata");
    expect(await signature.text()).toBe("versioned-signature");
    expect(bucket.getCalls).toEqual([
      "releases/stable/current.json",
      "releases/stable/1.0.0/ctx-release-metadata.env",
      "releases/stable/current.json",
      "releases/stable/1.0.0/ctx-release-metadata.env.sig",
    ]);
  });

  test("falls back to the legacy direct metadata object before a pointer exists", async () => {
    const bucket = new FakeReleaseBucket({
      "releases/stable/ctx-release-metadata.env": "legacy-metadata",
    });
    const response = await worker.fetch(
      new Request("https://cli.ctx.rs/functions/v1/releases/stable/ctx-release-metadata.env"),
      testEnv(bucket),
    );
    expect(await response.text()).toBe("legacy-metadata");
    expect(bucket.getCalls).toEqual([
      "releases/stable/current.json",
      "releases/stable/ctx-release-metadata.env",
    ]);
  });

  test("preserves HEAD semantics without reading the object body", async () => {
    const bucket = new FakeReleaseBucket({
      "releases/stable/latest.json": fakeJson({ channel: "stable" }),
    });

    const response = await worker.fetch(
      new Request("https://api.ctx.rs/functions/v1/releases/stable/latest.json", { method: "HEAD" }),
      testEnv(bucket),
    );

    expect(response.status).toBe(200);
    expect(await response.text()).toBe("");
    expect(response.headers.get("content-length")).toBe(String('{"channel":"stable"}'.length));
    expect(bucket.getCalls).toEqual([]);
    expect(bucket.headCalls).toEqual(["releases/stable/latest.json"]);
  });

  test("serves release manifests without mutating nested artifact paths", async () => {
    const bucket = new FakeReleaseBucket({
      "releases/stable/latest.json": fakeJson({
        platforms: {
          "linux-x64": {
            cli: {
              url_path: "/download/stable/1.2.3/ctx",
            },
          },
        },
      }),
    });

    const response = await worker.fetch(
      new Request("https://api.ctx.rs/functions/v1/releases/stable/latest.json", {
        headers: { referer: "https://ctx.rs/download" },
      }),
      testEnv(bucket),
    );

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      platforms: {
        "linux-x64": {
          cli: {
            url_path: "/download/stable/1.2.3/ctx",
          },
        },
      },
    });
  });

  test("redirects downloads to the configured R2 public base without installer attribution", async () => {
    const response = await worker.fetch(
      new Request(
        "https://api.ctx.rs/functions/v1/download/stable/1.2.3/ctx_1.2.3_macos-arm64.dmg?ctx_download_id=dl-1&utm_campaign=launch",
      ),
      testEnv(new FakeReleaseBucket({})),
    );

    expect(response.status).toBe(302);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(response.headers.get("location")).toBe(
      "https://r2.ctx.test/artifacts/stable/1.2.3/ctx_1.2.3_macos-arm64.dmg",
    );
    expect(await response.text()).toBe("");
  });

  test("keeps api.ade.ctx.rs release download routes compatible", async () => {
    const response = await worker.fetch(
      new Request("https://api.ade.ctx.rs/functions/v1/download/stable/1.2.3/ctx_1.2.3_macos-arm64.dmg"),
      testEnv(new FakeReleaseBucket({})),
    );

    expect(response.status).toBe(302);
    expect(response.headers.get("location")).toBe(
      "https://r2.ctx.test/artifacts/stable/1.2.3/ctx_1.2.3_macos-arm64.dmg",
    );
  });

  test("defaults download redirects to the request origin when no base is configured", async () => {
    const response = await worker.fetch(
      new Request("https://cli.ctx.rs/functions/v1/download/stable/0.1.0/ctx"),
      { RELEASES_BUCKET: new FakeReleaseBucket({}) },
    );

    expect(response.status).toBe(302);
    expect(response.headers.get("location")).toBe(
      "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/stable/0.1.0/ctx",
    );
  });

  test("keeps nested download object paths compatible with managed runtime artifacts", async () => {
    const response = await worker.fetch(
      new Request("https://api.ctx.rs/functions/v1/download/managed-runtimes/node/24.15.0/node-v24.15.0-linux-x64.tar.gz"),
      testEnv(new FakeReleaseBucket({})),
    );

    expect(response.status).toBe(302);
    expect(response.headers.get("location")).toBe(
      "https://r2.ctx.test/artifacts/managed-runtimes/node/24.15.0/node-v24.15.0-linux-x64.tar.gz",
    );
  });

  test("serves provider matrix latest JSON from R2", async () => {
    const bucket = new FakeReleaseBucket({
      "providers/stable/latest.json": fakeJson({
        providers: {
          codex: { version: "1.0.0" },
        },
      }),
    });

    const response = await worker.fetch(
      new Request("https://api.ctx.rs/functions/v1/provider-matrix/stable/latest.json"),
      testEnv(bucket),
    );

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      providers: {
        codex: { version: "1.0.0" },
      },
    });
    expect(bucket.getCalls).toEqual(["providers/stable/latest.json"]);
  });

  test("serves Supabase storage-shaped public object URLs from R2", async () => {
    const bucket = new FakeReleaseBucket({
      "artifacts/stable/1.2.3/ctx_1.2.3_linux-x64.AppImage": "appimage-bytes",
      "artifacts/stable/1.2.3/nested/download/file.txt": "nested-bytes",
    });

    const response = await worker.fetch(
      new Request("https://api.ctx.rs/storage/v1/object/public/releases/artifacts/stable/1.2.3/ctx_1.2.3_linux-x64.AppImage"),
      testEnv(bucket),
    );

    expect(response.status).toBe(200);
    expect(response.headers.get("content-length")).toBe(String("appimage-bytes".length));
    expect(await response.text()).toBe("appimage-bytes");
    expect(bucket.getCalls).toEqual(["artifacts/stable/1.2.3/ctx_1.2.3_linux-x64.AppImage"]);

    const nested = await worker.fetch(
      new Request("https://api.ctx.rs/storage/v1/object/public/releases/artifacts/stable/1.2.3/nested/download/file.txt"),
      testEnv(bucket),
    );
    expect(nested.status).toBe(200);
    expect(await nested.text()).toBe("nested-bytes");
  });

  test("preserves range requests on storage-shaped public object URLs", async () => {
    const objectKey = "artifacts/stable/1.2.3/ctx_1.2.3_macos-arm64.dmg";
    const bucket = new FakeReleaseBucket({
      [objectKey]: "0123456789",
    });

    const response = await worker.fetch(
      new Request(`https://api.ctx.rs/storage/v1/object/public/releases/${objectKey}`, {
        headers: { range: "bytes=2-5" },
      }),
      testEnv(bucket),
    );

    expect(response.status).toBe(206);
    expect(response.headers.get("accept-ranges")).toBe("bytes");
    expect(response.headers.get("content-range")).toBe("bytes 2-5/10");
    expect(response.headers.get("content-length")).toBe("4");
    expect(await response.text()).toBe("2345");
    expect(bucket.headCalls).toEqual([objectKey]);
    expect(bucket.getCalls).toEqual([objectKey]);
  });

  test("rejects unsatisfiable range requests with Content-Range", async () => {
    const objectKey = "artifacts/stable/1.2.3/ctx_1.2.3_macos-arm64.dmg";
    const bucket = new FakeReleaseBucket({
      [objectKey]: "0123456789",
    });

    const response = await worker.fetch(
      new Request(`https://api.ctx.rs/storage/v1/object/public/releases/${objectKey}`, {
        headers: { range: "bytes=20-25" },
      }),
      testEnv(bucket),
    );

    expect(response.status).toBe(416);
    expect(response.headers.get("content-range")).toBe("bytes */10");
    expect(bucket.headCalls).toEqual([objectKey]);
    expect(bucket.getCalls).toEqual([]);
  });

  test("rejects storage-shaped URLs outside the releases bucket", async () => {
    const response = await worker.fetch(
      new Request("https://api.ctx.rs/storage/v1/object/public/other/artifacts/stable/1.2.3/file"),
      testEnv(new FakeReleaseBucket({})),
    );

    expect(response.status).toBe(404);
  });

  test("rejects unsupported methods with an explicit Allow header", async () => {
    const response = await worker.fetch(
      new Request("https://api.ctx.rs/functions/v1/releases/stable/latest.json", { method: "POST" }),
      testEnv(new FakeReleaseBucket({})),
    );

    expect(response.status).toBe(405);
    expect(response.headers.get("allow")).toBe("GET, HEAD, OPTIONS");
  });
});

function testEnv(bucket: ReleaseBucket): Env {
  return {
    RELEASES_BUCKET: bucket,
    RELEASE_ARTIFACT_REDIRECT_BASE_URL: "https://r2.ctx.test",
  };
}

function fakeJson(value: unknown): string {
  return JSON.stringify(value);
}

class FakeReleaseBucket implements ReleaseBucket {
  readonly getCalls: string[] = [];
  readonly headCalls: string[] = [];
  private readonly objects: Map<string, string>;

  constructor(objects: Record<string, string>) {
    this.objects = new Map(Object.entries(objects));
  }

  async get(key: string, options?: ReleaseGetOptions): Promise<ReleaseObject | null> {
    this.getCalls.push(key);
    const body = this.objects.get(key);
    if (body == null) return null;
    const range = normalizeFakeRange(options?.range);
    return new FakeReleaseObject(applyFakeRange(body, range), range, body.length);
  }

  async head(key: string): Promise<ReleaseObject | null> {
    this.headCalls.push(key);
    const body = this.objects.get(key);
    return body == null ? null : new FakeReleaseObject(body);
  }
}

class FakeReleaseObject implements ReleaseObject {
  readonly httpEtag = '"test-etag"';
  private readonly encodedBody: Uint8Array;

  constructor(
    private readonly bodyText: string,
    readonly range?: ReleaseRange,
    private readonly fullSize?: number,
  ) {
    this.encodedBody = new TextEncoder().encode(bodyText);
  }

  get body(): ReadableStream<Uint8Array> {
    const encoded = this.encodedBody;
    return new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encoded);
        controller.close();
      },
    });
  }

  get size(): number {
    return this.fullSize ?? this.encodedBody.byteLength;
  }

  async text(): Promise<string> {
    return this.bodyText;
  }

  writeHttpMetadata(headers: Headers): void {
    headers.set("content-type", "application/json; charset=utf-8");
    headers.set("cache-control", "public, max-age=31536000");
  }
}

function normalizeFakeRange(range: ReleaseRange | Headers | undefined): ReleaseRange | undefined {
  if (range == null || range instanceof Headers) {
    return undefined;
  }
  return range;
}

function applyFakeRange(body: string, range: ReleaseRange | undefined): string {
  if (range == null) {
    return body;
  }
  if ("suffix" in range) {
    return body.slice(Math.max(0, body.length - range.suffix));
  }
  const start = range.offset ?? 0;
  const end = range.length == null ? undefined : start + range.length;
  return body.slice(start, end);
}
