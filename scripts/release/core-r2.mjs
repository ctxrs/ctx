import crypto from "node:crypto";
import { readBoundedResponse } from "./managed-pair-release-io.mjs";

const SESSION_TOKEN_MAX_BYTES = 4096;

function fail(message) { throw new Error(message); }
function sha256(body) { return crypto.createHash("sha256").update(body).digest("hex"); }

function hmac(key, value, encoding) {
  return crypto.createHmac("sha256", key).update(value).digest(encoding);
}

function encodedPath(bucket, objectKey) {
  const encode = (value) => encodeURIComponent(value).replace(
    /[!'()*]/gu,
    (character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`,
  );
  return `/${encode(bucket)}/${objectKey.split("/").map(encode).join("/")}`;
}

function validateAuthority(authority, environment) {
  const required = ["accessKeyEnv", "bucket", "endpointEnv", "label", "secretKeyEnv"];
  if (authority == null || typeof authority !== "object"
      || required.some((key) => typeof authority[key] !== "string" || authority[key] === "")) {
    fail("R2 authority contract is invalid");
  }
  const accessKeyId = environment[authority.accessKeyEnv];
  const secretAccessKey = environment[authority.secretKeyEnv];
  if (typeof accessKeyId !== "string" || accessKeyId === ""
      || typeof secretAccessKey !== "string" || secretAccessKey === "") {
    fail(`${authority.label} R2 credentials are unavailable`);
  }
  const sessionToken = authority.sessionTokenEnv == null
    ? undefined
    : environment[authority.sessionTokenEnv];
  if (sessionToken !== undefined
      && (typeof sessionToken !== "string" || sessionToken === ""
        || Buffer.byteLength(sessionToken, "utf8") > SESSION_TOKEN_MAX_BYTES
        || !/^[\x21-\x7e]+$/u.test(sessionToken))) {
    fail(authority.sessionTokenError
      ?? `${authority.label} R2 session token is malformed`);
  }
  const endpointValue = environment[authority.endpointEnv];
  let endpoint;
  try {
    endpoint = new URL(endpointValue);
  } catch {
    fail(`${authority.label} R2 endpoint is invalid`);
  }
  if (endpoint.protocol !== "https:" || endpoint.pathname !== "/"
      || endpoint.search || endpoint.hash || endpoint.username || endpoint.password) {
    fail(`${authority.label} R2 endpoint must be a pathless HTTPS origin`);
  }
  return { accessKeyId, endpoint, secretAccessKey, sessionToken };
}

function signedHeaders(method, url, body, extraHeaders, credentials, sessionTokenEnv) {
  const now = new Date();
  const amzDate = now.toISOString().replace(/[:-]|\.\d{3}/gu, "");
  const date = amzDate.slice(0, 8);
  const payloadHash = sha256(body);
  const headers = new Map([
    ["host", url.host],
    ["x-amz-content-sha256", payloadHash],
    ["x-amz-date", amzDate],
  ]);
  for (const [name, value] of Object.entries(extraHeaders)) {
    const canonicalName = name.toLowerCase();
    if (canonicalName === "x-amz-security-token") {
      fail(`${sessionTokenEnv ?? "R2 authority"} owns the x-amz-security-token header`);
    }
    headers.set(canonicalName, value.trim());
  }
  if (credentials.sessionToken !== undefined) {
    headers.set("x-amz-security-token", credentials.sessionToken);
  }
  const names = [...headers.keys()].sort();
  const canonicalHeaders = names.map(
    (name) => `${name}:${headers.get(name).replace(/\s+/gu, " ")}\n`,
  ).join("");
  const canonicalRequest = [
    method,
    url.pathname,
    url.searchParams.toString(),
    canonicalHeaders,
    names.join(";"),
    payloadHash,
  ].join("\n");
  const scope = `${date}/auto/s3/aws4_request`;
  const stringToSign = [
    "AWS4-HMAC-SHA256",
    amzDate,
    scope,
    sha256(Buffer.from(canonicalRequest, "utf8")),
  ].join("\n");
  const dateKey = hmac(`AWS4${credentials.secretAccessKey}`, date);
  const regionKey = hmac(dateKey, "auto");
  const serviceKey = hmac(regionKey, "s3");
  const signingKey = hmac(serviceKey, "aws4_request");
  const signature = hmac(signingKey, stringToSign, "hex");
  headers.set(
    "authorization",
    "AWS4-HMAC-SHA256 "
      + `Credential=${credentials.accessKeyId}/${scope}, `
      + `SignedHeaders=${names.join(";")}, Signature=${signature}`,
  );
  return Object.fromEntries(headers);
}

export function createR2Request(authority, environment, fetchImplementation = globalThis.fetch) {
  const credentials = validateAuthority(authority, environment);
  if (typeof fetchImplementation !== "function") fail("fetch is unavailable");
  return async (method, bucket, objectKey, body = Buffer.alloc(0), extraHeaders = {}) => {
    if (bucket !== authority.bucket) {
      fail(authority.bucketMismatchError
        ?? `refusing unexpected R2 bucket: ${bucket}`);
    }
    if (typeof objectKey !== "string" || !/^[A-Za-z0-9._/-]{1,1024}$/u.test(objectKey)
        || objectKey.startsWith("/") || objectKey.includes("//")
        || objectKey.split("/").some((part) => part === "." || part === "..")) {
      fail("R2 object key is invalid");
    }
    const url = new URL(credentials.endpoint);
    url.pathname = encodedPath(bucket, objectKey);
    const headers = signedHeaders(
      method,
      url,
      body,
      extraHeaders,
      credentials,
      authority.sessionTokenEnv,
    );
    return fetchImplementation(url, {
      body: method === "PUT" ? body : undefined,
      headers,
      method,
      redirect: "error",
    });
  };
}

export async function getR2Object(request, bucket, key, maximumBytes) {
  const response = await request("GET", bucket, key);
  if (response.status === 404) return null;
  if (!response.ok) fail(`R2 GET failed with status ${response.status}: ${key}`);
  const { body } = await readBoundedResponse(response, maximumBytes, true, `R2 object ${key}`);
  return { body, etag: response.headers.get("etag") };
}

export async function putImmutableR2Object(request, bucket, object) {
  const current = await getR2Object(request, bucket, object.key, object.body.length + 1);
  if (current != null) {
    if (!current.body.equals(object.body)) fail(`immutable R2 object already differs: ${object.key}`);
    return "existing-identical";
  }
  const response = await request("PUT", bucket, object.key, object.body, {
    "content-length": String(object.body.length),
    "content-type": object.contentType,
    "if-none-match": "*",
    "x-amz-meta-sha256": sha256(object.body),
  });
  await response.arrayBuffer();
  if (![200, 201, 204, 412].includes(response.status)) {
    fail(`R2 immutable PUT failed with status ${response.status}: ${object.key}`);
  }
  const stored = await getR2Object(request, bucket, object.key, object.body.length + 1);
  if (stored == null || !stored.body.equals(object.body)) {
    fail(`R2 immutable object verification failed: ${object.key}`);
  }
  return response.status === 412 ? "raced-identical" : "created";
}
