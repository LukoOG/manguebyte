// The demo server sends the headers that make the page cross-origin
// isolated, and the demo's scripts parse.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import { startServer, wasmFile } from '../../examples/web/serve.mjs';

async function withServer(options, body) {
  const server = await startServer({ port: 0, ...options });
  const base = `http://127.0.0.1:${server.address().port}`;
  try {
    await body(base);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

test('the page, the scripts and the module come with isolation headers and their types', async () => {
  await withServer({}, async (base) => {
    const root = await fetch(`${base}/`, { redirect: 'manual' });
    assert.equal(root.status, 302);
    assert.equal(root.headers.get('location'), '/examples/web/');
    for (const [path, type] of [
      ['/examples/web/', 'text/html; charset=utf-8'],
      ['/examples/web/main.js', 'text/javascript; charset=utf-8'],
      ['/js/engine.js', 'text/javascript; charset=utf-8'],
      ['/js/worklet.js', 'text/javascript; charset=utf-8'],
      ['/js/ring.js', 'text/javascript; charset=utf-8'],
    ]) {
      const response = await fetch(base + path);
      assert.equal(response.status, 200, path);
      assert.equal(response.headers.get('content-type'), type, path);
      assert.equal(response.headers.get('cross-origin-opener-policy'), 'same-origin', path);
      assert.equal(response.headers.get('cross-origin-embedder-policy'), 'require-corp', path);
      await response.arrayBuffer();
    }
    const wasm = await fetch(`${base}/manguebyte_web.wasm`);
    if (wasmFile()) {
      assert.equal(wasm.status, 200);
      assert.equal(wasm.headers.get('content-type'), 'application/wasm');
      assert.ok(WebAssembly.validate(new Uint8Array(await wasm.arrayBuffer())));
    } else {
      assert.equal(wasm.status, 404);
      await wasm.arrayBuffer();
    }
    for (const path of ['/..%2f..%2f..%2fCargo.toml', '/js/..%2f..%2f..%2f..%2fREADME.md', '/nothing-here.js']) {
      const response = await fetch(base + path);
      assert.ok(response.status === 403 || response.status === 404, `${path}: ${response.status}`);
      await response.arrayBuffer();
    }
    const post = await fetch(`${base}/js/ring.js`, { method: 'POST' });
    assert.equal(post.status, 405);
    await post.arrayBuffer();
  });
});

test('without isolation the headers are left out', async () => {
  await withServer({ isolation: false }, async (base) => {
    const response = await fetch(`${base}/examples/web/index.html`);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get('cross-origin-opener-policy'), null);
    assert.equal(response.headers.get('cross-origin-embedder-policy'), null);
    await response.arrayBuffer();
  });
});

test('the demo script and the worklet parse', () => {
  for (const file of ['../../examples/web/main.js', '../worklet.js', '../engine.js']) {
    execFileSync(process.execPath, ['--check', fileURLToPath(new URL(file, import.meta.url))]);
  }
});
