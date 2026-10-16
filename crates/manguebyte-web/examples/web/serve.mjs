// A small static server for the demo, with the headers that make the page
// cross-origin isolated, so that it gets SharedArrayBuffer:
//
//   Cross-Origin-Opener-Policy: same-origin
//   Cross-Origin-Embedder-Policy: require-corp
//
// It serves crates/manguebyte-web (so the page can load ../../js/*.js) and
// answers /manguebyte_web.wasm with the module from the workspace's target
// directory: the release build if there is one, else the debug build.
//
//   node crates/manguebyte-web/examples/web/serve.mjs [port]
//
// then open http://127.0.0.1:8000/ (it redirects to the demo page).
// Pass --no-isolation to leave the two headers out and see the message
// fallback at work.

import { createServer } from 'node:http';
import { existsSync } from 'node:fs';
import { readFile, stat } from 'node:fs/promises';
import { extname, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

/** The directory served: crates/manguebyte-web. */
export const ROOT = fileURLToPath(new URL('../../', import.meta.url));

const TARGET = fileURLToPath(new URL('../../../../target/wasm32-unknown-unknown/', import.meta.url));

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.wasm': 'application/wasm',
  '.md': 'text/plain; charset=utf-8',
  '.hex': 'text/plain; charset=utf-8',
};

/** The module the server hands out, or null if none has been built. */
export function wasmFile() {
  for (const profile of ['release', 'debug']) {
    const path = `${TARGET}${profile}${sep}manguebyte_web.wasm`;
    if (existsSync(path)) return path;
  }
  return null;
}

/**
 * Starts the server. Resolves to the node:http server once it listens;
 * `server.address().port` gives the port when `port` is 0.
 */
export function startServer({ port = 8000, host = '127.0.0.1', isolation = true } = {}) {
  const server = createServer(async (request, response) => {
    const headers = { 'Cache-Control': 'no-cache', 'X-Content-Type-Options': 'nosniff' };
    if (isolation) {
      headers['Cross-Origin-Opener-Policy'] = 'same-origin';
      headers['Cross-Origin-Embedder-Policy'] = 'require-corp';
      headers['Cross-Origin-Resource-Policy'] = 'same-origin';
    }
    const fail = (code, text) => {
      response.writeHead(code, { ...headers, 'Content-Type': 'text/plain; charset=utf-8' });
      response.end(text);
    };
    if (request.method !== 'GET' && request.method !== 'HEAD') return fail(405, 'GET only\n');
    let path;
    try {
      path = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    } catch {
      return fail(400, 'bad path\n');
    }
    if (path === '/') {
      response.writeHead(302, { ...headers, Location: '/examples/web/' });
      return response.end();
    }
    let file;
    if (path === '/manguebyte_web.wasm') {
      file = wasmFile();
      if (!file) return fail(404, 'build it: cargo build -p manguebyte-web --target wasm32-unknown-unknown --release\n');
    } else {
      file = resolve(ROOT, `.${path}`);
      if (file !== ROOT.replace(/[\\/]$/, '') && !file.startsWith(ROOT)) return fail(403, 'outside the crate\n');
      try {
        if ((await stat(file)).isDirectory()) file = resolve(file, 'index.html');
      } catch {
        return fail(404, 'not found\n');
      }
    }
    let body;
    try {
      body = await readFile(file);
    } catch {
      return fail(404, 'not found\n');
    }
    response.writeHead(200, {
      ...headers,
      'Content-Type': TYPES[extname(file)] ?? 'application/octet-stream',
      'Content-Length': body.length,
    });
    response.end(request.method === 'HEAD' ? undefined : body);
  });
  return new Promise((done, failed) => {
    server.once('error', failed);
    server.listen(port, host, () => done(server));
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2);
  const isolation = !args.includes('--no-isolation');
  const port = Number(args.find((arg) => /^\d+$/.test(arg)) ?? process.env.PORT ?? 8000);
  const server = await startServer({ port, isolation });
  const { address, port: actual } = server.address();
  console.log(`serving ${ROOT} on http://${address}:${actual}/`);
  console.log(isolation ? 'cross-origin isolated: the ring uses SharedArrayBuffer' : 'not isolated: records go by message');
  console.log(wasmFile() ? `module: ${wasmFile()}` : 'no module built yet: see the README');
}
