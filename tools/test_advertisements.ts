/** 广告位配置与真实 HTTP 客户端回归：异常内容不替换有效缓存，也不阻止工作台启动。 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { once } from "node:events";
import { ADVERTISEMENTS_CACHE_KEY, loadAdvertisementConfig, parseAdvertisementConfig, readAdvertisementCache, type AdvertisementConfig } from "../frontend/src/lib/advertisements.ts";

const fallback = parseAdvertisementConfig(JSON.parse(await readFile(new URL("../config/advertisements.json", import.meta.url), "utf8")))!;
const remote: AdvertisementConfig = { ...fallback, interval_seconds: 9, items: [{ ...fallback.items[0], id: "remote", title: "远程推荐" }] };
function memoryStorage() {
  const values = new Map<string, string>();
  return { getItem: (key: string) => values.get(key) || null, setItem: (key: string, value: string) => { values.set(key, value); } };
}

test("内置配置符合契约；支持关闭整个广告位、单条停投和空广告列表", () => {
  assert.equal(fallback.items.length, 2);
  assert.deepEqual(parseAdvertisementConfig({ ...fallback, enabled: false })?.enabled, false);
  assert.equal(parseAdvertisementConfig({ ...fallback, items: [] })?.items.length, 0);
  assert.equal(parseAdvertisementConfig({ ...fallback, items: [{ ...fallback.items[0], enabled: false }] })?.items[0].enabled, false);
});

test("拒绝危险协议、带凭据链接、重复 ID 和越界配置", () => {
  for (const url of ["javascript:alert(1)", "file:///tmp/private", "http://example.com", "//example.com", "https://user:pass@example.com"]) {
    assert.equal(parseAdvertisementConfig({ ...fallback, items: [{ ...fallback.items[0], url }] }), null);
  }
  for (const interval_seconds of [0, 4, 61, 6.5, NaN]) assert.equal(parseAdvertisementConfig({ ...fallback, interval_seconds }), null);
  assert.equal(parseAdvertisementConfig({ ...fallback, items: [fallback.items[0], fallback.items[0]] }), null);
  assert.equal(parseAdvertisementConfig({ ...fallback, items: Array.from({ length: 13 }, (_, id) => ({ ...fallback.items[0], id: `${id}` })) }), null);
  assert.equal(parseAdvertisementConfig({ ...fallback, items: [{ ...fallback.items[0], title: " " }] }), null);
  assert.equal(parseAdvertisementConfig({ ...fallback, version: 2 }), null);
});

test("缓存损坏、超过七天或来自未来时回退；缓存禁用不影响默认内容", () => {
  const storage = memoryStorage();
  const now = Date.now();
  for (const saved_at of [now - 8 * 86400000, now + 1000, "invalid"]) {
    storage.setItem(ADVERTISEMENTS_CACHE_KEY, JSON.stringify({ saved_at, config: remote }));
    assert.equal(readAdvertisementCache(storage, now), null);
  }
  storage.setItem(ADVERTISEMENTS_CACHE_KEY, "{");
  assert.equal(readAdvertisementCache(storage), null);
  assert.equal(readAdvertisementCache({ getItem: () => { throw new Error("存储不可用"); }, setItem: () => {} }), null);
});

test("通过实际 HTTP 验证远程配置、缓存与错误回退", async t => {
  const requests: string[] = [];
  const server = createServer((req, res) => {
    const route = req.url || "/";
    requests.push(route);
    assert.equal(req.headers.cookie, undefined);
    assert.equal(req.headers.referer, undefined);
    if (route === "/hanging") return;
    if (route === "/404") { res.writeHead(404); res.end("Not found"); return; }
    if (route === "/500") { res.writeHead(500, { "content-type": "text/html" }); res.end("<h1>Unavailable</h1>"); return; }
    res.setHeader("content-type", "application/json");
    if (route === "/disabled") res.end(JSON.stringify({ ...remote, enabled: false }));
    else if (route === "/empty") res.end(JSON.stringify({ ...remote, items: [] }));
    else if (route === "/malformed") res.end("{");
    else if (route === "/invalid") res.end(JSON.stringify({ ...remote, interval_seconds: 0 }));
    else if (route === "/oversize") res.end(" ".repeat(32769));
    else res.end(JSON.stringify(remote));
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  const url = `http://127.0.0.1:${address.port}`;
  try {
    await t.test("远程成功后立即采用并缓存；不能写缓存时仍采用远程内容", async () => {
      const storage = memoryStorage();
      assert.deepEqual(await loadAdvertisementConfig({ fallback, storage, url: `${url}/ok` }), remote);
      assert.deepEqual(readAdvertisementCache(storage), remote);
      assert.deepEqual(await loadAdvertisementConfig({ fallback, url: `${url}/ok`, storage: { getItem: () => null, setItem: () => { throw new Error("配额已满"); } } }), remote);
    });
    for (const route of ["disabled", "empty"]) {
      await t.test(`${route} 是有效的停投配置，不回退内置广告`, async () => {
        const storage = memoryStorage();
        const expected = route === "disabled" ? { ...remote, enabled: false } : { ...remote, items: [] };
        assert.deepEqual(await loadAdvertisementConfig({ fallback, storage, url: `${url}/${route}` }), expected);
        assert.deepEqual(readAdvertisementCache(storage), expected);
      });
    }
    for (const route of ["404", "500", "malformed", "invalid", "oversize", "hanging"]) {
      await t.test(`${route} 保留有效缓存；没有缓存时显示内置内容`, async () => {
        const storage = memoryStorage();
        storage.setItem(ADVERTISEMENTS_CACHE_KEY, JSON.stringify({ saved_at: Date.now(), config: remote }));
        const options = { fallback, url: `${url}/${route}`, timeoutMs: route === "hanging" ? 40 : 2000 };
        assert.deepEqual(await loadAdvertisementConfig({ ...options, storage }), remote);
        assert.deepEqual(await loadAdvertisementConfig(options), fallback);
        assert.deepEqual(readAdvertisementCache(storage), remote);
      });
    }
    assert.ok(requests.includes("/ok"));
    assert.ok(requests.includes("/hanging"));
  } finally { server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve())); }
  await t.test("服务不可达时仍返回内置内容", async () => {
    assert.deepEqual(await loadAdvertisementConfig({ fallback, url: `${url}/ok`, timeoutMs: 500 }), fallback);
  });
});
