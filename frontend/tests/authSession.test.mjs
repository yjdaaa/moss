import assert from "node:assert/strict";
import test from "node:test";
import {
  authenticatedHeaders,
  clearAuthToken,
  fetchWithAuth,
  readAuthToken,
  storeAuthToken,
} from "../src/authSession.js";

function storage() {
  const values = new Map();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, String(value)),
    removeItem: (key) => values.delete(key),
  };
}

test("auth token is session-scoped and added without replacing headers", async () => {
  const session = storage();
  storeAuthToken("abc", session);
  assert.equal(readAuthToken(session), "abc");
  const headers = authenticatedHeaders({ "Content-Type": "application/json" }, session);
  assert.equal(headers.get("Content-Type"), "application/json");
  assert.equal(headers.get("Authorization"), "Bearer abc");
  let seen;
  await fetchWithAuth(async (_url, init) => {
    seen = init.headers;
    return { ok: true };
  }, "/api/health", {}, session);
  assert.equal(seen.get("Authorization"), "Bearer abc");
  clearAuthToken(session);
  assert.equal(readAuthToken(session), "");
});

