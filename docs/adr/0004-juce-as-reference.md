# ADR 0004: JUCE is the reference for scope and lessons, never for code

**Status:** Accepted
**Date:** 2026-10-13
**Deciders:** Maintainers

## Context

The project's stated goal is to be for Rust what JUCE is for C++ and then to
go further, including fixing the problems JUCE users run into most. That
makes JUCE's repository the natural yardstick, and we do use it as one.

It also creates a legal constraint. JUCE is licensed under the AGPLv3 or a
commercial licence. Manguebyte is `MIT OR Apache-2.0`. Code derived from
JUCE's source could not be distributed under our licence, and a single
derived function would put the licence of the whole project in doubt for
every downstream user.

## Decision

Use JUCE as a reference for **what to build and what to avoid**, and never as
a source of **how it is written**.

Allowed, and encouraged:

- The list of modules and the features each one offers, as a checklist of
  scope ([JUCE_PARITY.md](../JUCE_PARITY.md)).
- Public documentation, the changelog and the roadmap, to understand
  behaviour and direction.
- The issue tracker and the forum, to learn which designs caused trouble.
- The public behaviour of a class as a requirement: "a normalisable range
  with a skew that puts a chosen value at the centre".

Not allowed:

- Copying, translating or paraphrasing JUCE source code, line by line or
  function by function.
- Writing an implementation with the corresponding JUCE implementation open.
- Reproducing JUCE's internal structure where it is an expressive choice
  rather than a consequence of the problem.

Implementations come from specifications, standards, papers and textbooks,
and name their source in a comment: the RBJ cookbook for biquads, Bluestein
for arbitrary-length transforms, Harris and Nuttall for windows, the RIFF
specification for WAV.

The same rule applies to any other project whose licence is incompatible
with ours. When a dependency is considered, its licence is checked first. For
VST3 in particular, the `vst3-sys` bindings are GPLv3 and must not be used;
the VST3 SDK itself is now MIT, and bindings generated from it are fine.

## Options considered

### Option A: ignore JUCE entirely

**Pros:** no licence question at all.
**Cons:** throws away twenty years of evidence about what an audio framework
needs and where one goes wrong.

### Option B: port JUCE to Rust

**Pros:** fastest route to feature parity.
**Cons:** the result would be AGPL; it would also inherit the designs this
project exists to improve on.

### Option C: reference for scope and lessons, clean implementation (chosen)

**Pros:** keeps the permissive licence; lets the design start from Rust's
strengths rather than from C++ idioms.
**Cons:** slower than porting; needs care from every contributor.

## Consequences

- Reviewers ask where an algorithm came from when it is not obvious.
- API names follow Rust conventions and our own vocabulary, not JUCE's class
  names.
- Parity is measured by capability, not by having the same classes.

## Action items

1. [x] State the rule in `CONTRIBUTING.md`
2. [x] Keep a parity matrix by module
3. [ ] Add a pull request template with a provenance checkbox
