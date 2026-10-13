# ADR 0001: One crate per area, layered, with a facade

**Status:** Accepted
**Date:** 2026-10-13
**Deciders:** Maintainers

## Context

Manguebyte aims to cover everything JUCE covers. JUCE ships as 24 modules
that depend on each other fairly freely; until JUCE 8.0.11 its plugin
processing and hosting module depended on GUI modules, so a command-line tool
that only hosts a plugin had to build the user-interface stack.

We want the opposite property: a user who needs a filter compiles a filter.

## Decision

Split the framework into one crate per area, arranged in layers where
dependencies only point downwards, and publish a `manguebyte` facade crate
that re-exports them all with a prelude. `manguebyte-core` has no
dependencies. Presentation crates are never a dependency of processing crates.

## Options considered

### Option A: a single crate with feature flags

| Dimension | Assessment |
|---|---|
| Complexity | Low at first, high later: features interact |
| Compile time | Everything rebuilds together |
| Enforcing layering | Not possible; any module can import any other |

**Pros:** one version number, simplest to publish.
**Cons:** nothing stops processing code from importing GUI code; feature
combinations multiply and are rarely all tested.

### Option B: layered crates plus a facade (chosen)

| Dimension | Assessment |
|---|---|
| Complexity | Medium: several manifests, one workspace |
| Compile time | Crates build in parallel and cache separately |
| Enforcing layering | The compiler refuses a dependency cycle |

**Pros:** the layering is checked by Cargo; users can depend on one crate;
the core can become `no_std` without dragging the rest along.
**Cons:** more crates to version and publish; a type shared by two crates
must live in a lower one.

## Trade-off analysis

The cost of Option B is paid by maintainers at release time. The cost of
Option A is paid by every user in compile time and binary size, and by the
design itself as shortcuts across layers accumulate. A framework that intends
to be large must have its structure enforced by a tool rather than by
discipline.

## Consequences

- A headless build is the default state of every processing crate.
- New areas arrive as new crates; the facade gains a feature flag for any
  that pull in system libraries, as `devices` does.
- Shared vocabulary types gravitate to `core`, which must be kept small on
  purpose.

## Action items

1. [x] Workspace with `core`, `midi`, `params`, `dsp`, `formats`, `devices`
2. [x] CI job that builds and tests the facade with no default features
3. [ ] `manguebyte-plugin` above `dsp`, `params` and `midi`
4. [ ] `no_std` for `core` and `dsp`
