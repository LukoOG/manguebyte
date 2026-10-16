# Web demo

A page that plays a one-bar drum pattern and a bass line through the
manguebyte-web engine, compiled to WebAssembly and run in an
`AudioWorklet`.

## Build the module

From the workspace root:

```sh
cargo build -p manguebyte-web --target wasm32-unknown-unknown --release
```

This writes `target/wasm32-unknown-unknown/release/manguebyte_web.wasm`.
A debug build (without `--release`) works too, but its DSP is much slower.
No binding generator is involved: the module imports nothing and the
JavaScript in `crates/manguebyte-web/js/` calls its exports directly.

## Serve it with the right headers

The engine's event ring lives in a `SharedArrayBuffer`, and a browser only
gives a page `SharedArrayBuffer` when the page is cross-origin isolated.
That takes two response headers:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

`serve.mjs` is a small Node.js server that sends them:

```sh
node crates/manguebyte-web/examples/web/serve.mjs
```

Open <http://127.0.0.1:8000/> and press Play. The server serves the crate
directory and answers `/manguebyte_web.wasm` with the release build, or the
debug build if there is no release build. Browsers treat `127.0.0.1` and
`localhost` as secure contexts, which `AudioWorklet` also needs; from any
other host the page must be served over HTTPS.

Any other server works if it sends the two headers, serves `.wasm` as
`application/wasm` and `.js` as JavaScript, and makes the module available
at `/manguebyte_web.wasm`.

To see the fallback, start the server with `--no-isolation`: the page then
has no `SharedArrayBuffer`, the status line says so, and the records are
copied into messages on the node's port instead.

## Sample rate

The engine runs at the `AudioContext`'s sample rate, which must be from
3 000 to 768 000 Hz, the range browsers allow. Outside it the page reports
that the sample rate is out of range, and names it.

## What the page does

- Makes part 0 a drum kit and part 1 a synth, both with the full chain
  (low-pass, reverb, gain and pan), and sets a few parameters by their
  text identifiers.
- Schedules each sixteenth note 150 ms ahead, with times taken from
  `AudioContext.currentTime`; the helper turns them into engine frames.
- The drum pattern is written in Aratu's symbols: `b` kick, `s` snare,
  `c` closed hat, `o` open hat, `r` ride, `x` crash. The open hat on the
  last step is choked by the closed hat on the first.
- The sliders read their ranges from the part's parameter JSON.
