// Finds the WebAssembly module the tests drive.
//
// Build it first with
//   cargo build -p manguebyte-web --target wasm32-unknown-unknown
// or point MANGUEBYTE_WASM at a module built elsewhere (a release build,
// say). The tests are skipped, with this reason, when there is none.

import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const candidates = [
  process.env.MANGUEBYTE_WASM,
  fileURLToPath(new URL('../../../../target/wasm32-unknown-unknown/debug/manguebyte_web.wasm', import.meta.url)),
  fileURLToPath(new URL('../../../../target/wasm32-unknown-unknown/release/manguebyte_web.wasm', import.meta.url)),
].filter(Boolean);

/** The path of the module, or null. */
export const wasmPath = candidates.find((path) => existsSync(path)) ?? null;

/** Why the tests that need the module are skipped, or false. */
export const skipWithoutWasm = wasmPath
  ? false
  : 'no manguebyte_web.wasm: run cargo build -p manguebyte-web --target wasm32-unknown-unknown';

/** The module's bytes. */
export function wasmBytes() {
  return readFileSync(wasmPath);
}
