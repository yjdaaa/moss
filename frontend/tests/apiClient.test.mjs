import test from "node:test";
import assert from "node:assert/strict";
import { fetchPreviewText, getJson, postJson, previewResourcePath } from "../src/api/client.js";

function withFetch(impl, run) {
  const original = globalThis.fetch;
  globalThis.fetch = impl;
  return Promise.resolve().then(run).finally(() => { globalThis.fetch = original; });
}

function hangingFetch(seen) {
  return (url, init = {}) => new Promise((_, reject) => {
    seen.push({ url, init });
    init.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
  });
}

test("getJson/postJson：传 timeoutMs 时挂起的请求到点中止并如实报告超时", async () => {
  const seen = [];
  await withFetch(hangingFetch(seen), async () => {
    const started = Date.now();
    const got = await getJson("/api/jobs/stop-targets", { timeoutMs: 30 });
    assert.equal(got.ok, false);
    assert.equal(got.timedOut, true);
    assert.match(got.data.result, /超时/);
    const posted = await postJson("/api/safe-action", { action: "emergency_stop", confirmed: true }, { timeoutMs: 30 });
    assert.equal(posted.ok, false);
    assert.equal(posted.timedOut, true);
    assert.ok(Date.now() - started < 2000, "超时必须有界");
  });
  assert.equal(seen[1].init.method, "POST");
  assert.equal(seen[1].init.body, JSON.stringify({ action: "emergency_stop", confirmed: true }));
});

test("getJson/postJson：不传 timeoutMs 时行为不变（无超时信号），并保留调用方 signal 的中止", async () => {
  const seen = [];
  await withFetch(async (url, init = {}) => {
    seen.push(init);
    return new Response(JSON.stringify({ success: true, result: "ok" }), { status: 200 });
  }, async () => {
    const res = await getJson("/api/system/info");
    assert.deepEqual(res, { status: 200, ok: true, data: { success: true, result: "ok" } });
  });
  assert.equal(seen[0].signal, undefined);

  const controller = new AbortController();
  await withFetch(hangingFetch([]), async () => {
    const pending = postJson("/api/workflow", {}, { signal: controller.signal, timeoutMs: 5000 });
    controller.abort();
    const res = await pending;
    assert.equal(res.ok, false);
    assert.equal(res.timedOut, undefined, "调用方主动中止不算超时");
    assert.match(res.data.result, /网络连接失败/);
  });
});

test("fetchPreviewText：非 2xx 不把错误页当正文；成功时返回文本；统一拼 /preview?path=", async () => {
  assert.equal(previewResourcePath("a b.gcode"), "/preview?path=a%20b.gcode");
  assert.equal(previewResourcePath("/preview?path=x"), "/preview?path=x");
  const urls = [];
  await withFetch(async (url) => {
    urls.push(url);
    return url.includes("missing")
      ? new Response("<html>404 Not Found</html>", { status: 404 })
      : new Response("G0 X0 Y0\nG1 X1 S10\n", { status: 200 });
  }, async () => {
    const missing = await fetchPreviewText("missing.gcode");
    assert.equal(missing.ok, false);
    assert.equal(missing.text, "");
    assert.match(missing.error, /404/);
    const ok = await fetchPreviewText("job.gcode");
    assert.equal(ok.ok, true);
    assert.equal(ok.text, "G0 X0 Y0\nG1 X1 S10\n");
  });
  assert.deepEqual(urls, ["/preview?path=missing.gcode", "/preview?path=job.gcode"]);
  assert.equal((await fetchPreviewText("")).ok, false);
});
