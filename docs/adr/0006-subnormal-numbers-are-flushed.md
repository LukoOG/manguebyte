# ADR 0006: Subnormal numbers are flushed to zero on the audio thread

Status: accepted, 2026-10-13

## Context

A sound that fades does not reach zero. Filter memory, reverb tails and
envelope followers keep shrinking until their values leave the normal
floating-point range and become subnormal (denormal). They are still not
zero, and on most processors arithmetic on them is tens of times slower. A
plugin then uses the most processor time in the silence after a note, and a
host drops audio.

Processors have a setting that rounds these numbers to zero. It is per
thread, costs nothing, and removes values more than 750 dB below full scale.

JUCE offers the setting as `ScopedNoDenormals`, a guard a plugin author
places at the top of `processBlock`. The project templates include it; a
plugin written without the template, or a new audio callback, does not have
it until someone remembers.

The defect showed up here in the same way. `clap-validator` times a plugin
on ordinary and on subnormal input, and warned about the example effect. A
direct measurement agreed: 4,000 blocks of subnormal input took the effect
442 ms, against 203 ms for noise.

## Decision

`manguebyte_core::NoDenormals` is a guard that turns the setting on for the
current thread and puts it back when dropped. It is held:

- by the CLAP and VST 3 wrappers around `Plugin::process`;
- by the standalone engine;
- by the device streams around every callback;
- by `Harness`, so that a test computes what a host would.

A plugin author does nothing. With the guard, the same 4,000 blocks take
31 ms.

The setting is restored on the way out, including when a plugin panics, so
a host's own code never runs with a setting it did not choose. The wrappers'
tests check both directions.

## Consequences

**Rust does not define this.** The language assumes the default
floating-point settings, and the documentation of the deprecated
`_mm_setcsr` calls changing them undefined behaviour. What the compiler uses
the assumption for is evaluating arithmetic at compile time. The observable
effect is that an expression folded by the compiler can yield a subnormal
number where the processor would have yielded zero, a difference that cannot
be heard. C and C++ compilers make the same assumption, and audio software
in every language sets these bits. We take the same position knowingly and
keep it in one small module, `denormal.rs`, written with inline assembly so
that nothing else in the framework touches the register.

**It is per processor family.** `x86_64` (MXCSR: flush-to-zero and
denormals-are-zero) and `aarch64` (FPCR: flush-to-zero) are covered.
Elsewhere the guard does nothing and `NoDenormals::SUPPORTED` is `false`.

**Miri cannot run it.** Under Miri the guard is a no-op, so the wrappers'
tests still run there.

**The validator may still warn.** `clap-validator`'s timing includes filling
its own buffers with subnormal numbers, which is slow on its side. For a
plugin that does little work per block the warning can remain although the
plugin itself is faster on subnormal input than on noise.

**Offline code is not covered automatically.** A program that calls a
processor in its own loop, outside the harness and the streams, creates the
guard itself.
