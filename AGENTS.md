# Notes for automated agents and reviewers

The Manguebyte Audio Framework is a Rust workspace for audio software
(plugins in CLAP, VST 3, AU and LV2, standalone programs, a sampler, a
native interface). [CONTRIBUTING.md](CONTRIBUTING.md) has the rules for
everyone; this file says what matters most when checking a change.

## Review guidelines

Report problems that would make the code wrong for the people who use it.
In order of weight:

- **Real time.** Code the audio thread runs (`process`, `render`, a
  `Processor`, a voice, anything a doc comment says runs on the audio
  thread) must not allocate, free, lock, wait, do I/O or panic on any
  input. Look for `Vec` growth, `Box`/`Arc` creation or drops, `String`
  formatting, `Mutex`, channels that block, `unwrap`/indexing that can go
  out of bounds, and division by values that can be zero. ADR 0002 and
  ADR 0003 say how state crosses threads: it is polled, never pushed
  through listeners.
- **Numerical correctness.** Filters and oscillators stay stable for
  every parameter they accept, including sample rates from 8 kHz to
  384 kHz; NaN or infinity never reaches the output; a parameter change
  does not click unless documented. Units (hertz, seconds, samples,
  decibels, normalised 0..1) must match what the docs say.
- **Determinism.** Output promised "bit for bit" the same whatever the
  block size, thread count or processor must stay so: watch for
  reordered floating-point sums, platform maths (`f32::sin` where
  `manguebyte_gfx::sin_cos` or the crate's own is used for goldens) and
  state that depends on how a block is split.
- **Unsafe code and FFI.** Every `unsafe` block needs a `// SAFETY:`
  comment that is true; plugin and host wrappers must not unwind across
  the C ABI, and must follow the format's threading rules (CLAP, VST 3,
  AU, LV2).
- **Layering.** Audio crates never depend on interface crates
  (`manguebyte-gfx`, `-ui`, `-window`, `-gui`), not even behind a feature
  (ADR 0001, `scripts/check-layering.sh`).
- **Claims.** Documentation, the changelog and docs/JUCE_PARITY.md claim
  only what a test proves. A claim with no test behind it is a finding.
- **Licences.** Nothing may be copied or translated from JUCE or from code
  under the GPL, LGPL, AGPL or MPL.

Style, naming and wording are not findings unless they make the code
wrong or the documentation untrue.

## Audit pull requests

A pull request into a base branch named `audit/...` is never merged. Its
base is `main` with one crate removed, so its diff is the whole crate, and
it exists only to have that crate reviewed. Review it as code already in
`main`: report bugs and untrue claims in it, by the guidelines above.
