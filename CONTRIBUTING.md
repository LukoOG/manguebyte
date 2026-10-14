# Contributing to Manguebyte

Thank you for wanting to help. Manguebyte is a large project with a long road
ahead, and it only gets there if many people build it together.

Everything in this repository is in English: code, comments, commit messages,
issues and pull requests. Imperfect English is completely fine.

## Ways to contribute

- **Report a bug.** Open an issue with what you did, what you expected and what
  happened. A small program that reproduces the problem is worth a lot.
- **Propose a feature.** Open an issue before writing a large change, so the
  design can be discussed first. [ROADMAP.md](ROADMAP.md) lists what is planned.
- **Improve documentation.** Clearer explanations and better examples are
  always welcome.
- **Write code.** Pick an item from the roadmap or an open issue, comment that
  you are taking it, and send a pull request.

## JUCE is a reference, not a source

Manguebyte aims to offer what JUCE offers and to fix what JUCE users
struggle with, so we study JUCE closely. It is an independent implementation
under a different licence, so we never take code from it.

- **Do** use JUCE's module list, public documentation, changelog, issue
  tracker and forum to decide what to build and which designs to avoid.
  [docs/JUCE_PARITY.md](docs/JUCE_PARITY.md) tracks this.
- **Never copy, translate or paraphrase JUCE source code**, and do not write
  an implementation with the corresponding JUCE implementation open. JUCE is
  licensed under the AGPL or a commercial licence, both incompatible with
  ours.
- The same goes for any other project whose licence is not compatible with
  `MIT OR Apache-2.0`.
- Work from specifications, standards, papers, textbooks and your own
  knowledge. When an algorithm comes from a published source, name it in a
  comment.

The reasoning is in
[ADR 0004](docs/adr/0004-juce-as-reference.md). If you are not sure whether
something is acceptable, ask in an issue first.

## Getting set up

You need a recent stable Rust toolchain (1.85 or newer) with `rustfmt` and
`clippy`:

```bash
rustup component add rustfmt clippy
```

On Linux, the device backend needs the ALSA development headers:

```bash
sudo apt install libasound2-dev
```

Then fork the repository, clone your fork and create a branch:

```bash
git clone https://github.com/<your-user>/manguebyte
cd manguebyte
git checkout -b my-change
```

## Before you open a pull request

Run the same checks the CI runs. One script does all of them and stops at the
first failure:

```bash
./scripts/check.sh
```

It runs `cargo fmt --all --check`, `cargo clippy` and `cargo doc` with
warnings as errors, and `cargo test` with and without default features.

If you changed `manguebyte-devices` and have a sound card, also run the
hardware tests, which play nothing audible:

```bash
cargo test -p manguebyte-devices --test hardware -- --ignored --test-threads 1
```

A pull request should:

- do one thing, and say what and why in its description;
- include tests for new behaviour and for fixed bugs;
- document every public item;
- keep the build free of warnings.

## Writing real-time code

Code that runs on the audio thread has a deadline of a few milliseconds, every
time. Anything that can take an unbounded amount of time will eventually cause
an audible glitch. In `Processor::process` and anything it calls:

- **Do not allocate or free memory.** No `Vec::push` past capacity, no `Box`,
  no `String`, no `format!`. Allocate in `prepare`.
- **Do not lock.** No `Mutex`, no `RwLock`. Use atomics or the `spsc` queue.
- **Do not block or do I/O.** No files, no network, no printing.
- **Do not panic.** Validate in `prepare`, and clamp instead of asserting.

Say in the documentation whether a function is safe to call from the audio
thread whenever it is not obvious.

These rules are enforced. Each crate with audio-thread code has a
`tests/realtime.rs` that replaces the allocator with one that counts and fails
if anything touches the heap while processing (`manguebyte-dsp` for
processors, `manguebyte-analysis` for meters and analysers, and so on).
**When you add a processor, add it to the test of its crate.**

Before changing a core trait or adding a crate, read
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Style

- `cargo fmt` decides formatting.
- Prefer safe code. Every `unsafe` block needs a `// SAFETY:` comment that
  explains why it is sound.
- Keep dependencies few. `manguebyte-core` has none and should stay that way.
  Adding a dependency to another crate needs a reason in the pull request.
- Be generic over `Sample` where it is natural, so code works for both `f32`
  and `f64`.
- Test behaviour, not implementation: check a filter by its frequency
  response, an oscillator by its pitch.

## Commit messages

Write the subject line in the imperative, under about 70 characters, and use
the body to explain why the change is needed:

```text
Add state-variable filter

The biquad becomes unstable when its cutoff is modulated at audio rate.
The SVF topology stays stable under modulation, which synth filters need.
```

## Licensing of contributions

Manguebyte is dual licensed under MIT and Apache-2.0. Unless you explicitly
state otherwise, any contribution you intentionally submit for inclusion in
the project, as defined in the Apache-2.0 license, is dual licensed in the
same way, without any additional terms or conditions.

By contributing you confirm that the work is your own, or that you have the
right to submit it under these licenses.

## Conduct

Be kind, be patient and assume good faith. Criticise code, not people. This
project wants contributors of every background and experience level, and
harassment or disrespect of any kind is not accepted.
