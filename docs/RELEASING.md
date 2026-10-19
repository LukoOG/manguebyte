# Releasing

How a version of the Manguebyte Audio Framework reaches crates.io. Every
crate of the workspace shares one version number and is released together.

## Before

1. `main` is green in CI, and `./scripts/check.sh` passes locally.
2. The version is raised in two places, together: `version` under
   `[workspace.package]` in the root `Cargo.toml`, and the `version` of every
   `manguebyte-*` entry under `[workspace.dependencies]`.
3. In `CHANGELOG.md`, the `Unreleased` heading becomes `[X.Y.Z] - YYYY-MM-DD`,
   with a new empty `Unreleased` above it.
4. The packages are checked as crates.io will see them:

   ```sh
   cargo publish --workspace --dry-run
   ```

   This packages every crate, then builds each one from its package against
   the others, in dependency order. It needs a few gigabytes of free disk.
   `--no-verify` skips the builds, which is enough to check manifests and
   package contents.

Each crate carries its own `README.md` and its licence files, because a
package holds only the files under its folder: both licence texts, which
CI checks against the root ones, except `manguebyte-pro-io`, which carries
the GPLv3 alone in `LICENSE`, as CI also checks. A crate that depends on
itself for its own tests does so through a path-only dev-dependency, which
cargo drops on publish.

What `cargo publish --workspace` takes is set by each manifest's `publish`
key. The example plugins (`plugins/pluck`, `plugins/duck` and
`plugins/aratu-bridge`) have `publish = false` and are never published.
No crate under `crates/` has a `publish` key, so all of them are
published, `manguebyte-pro-io` among them: it is under the GPLv3, not MIT
or Apache-2.0, and whether it goes to crates.io with the rest is a
decision to take before the first release.

## Publishing

1. The person publishing logs in with a crates.io token that can publish new
   crates and updates: `cargo login`. Tokens never go into the repository.
2. Publish, in dependency order:

   ```sh
   cargo publish --workspace
   ```

3. crates.io limits how fast one account can create **new** crates: a small
   burst, then roughly one every ten minutes (check the current numbers in
   crates.io's policies, or ask help@crates.io to raise the limit for a
   release). The first release creates every crate, so it takes a few hours.
   When the limit stops the command, publish the remaining crates with
   `cargo publish -p <name>`, in the order the dry run printed. Later
   releases only update existing crates and are not held back this way.

## After

1. Tag the commit that was published, and push the tag:
   `git tag -a vX.Y.Z -m "vX.Y.Z" && git push origin vX.Y.Z`.
2. Make a GitHub release from the tag with the version's CHANGELOG section.
3. Check that docs.rs built every crate.
4. Add the project's other maintainers as owners of every crate
   (`cargo owner --add <login> <crate>`), so publishing never depends on one
   person.

A published version cannot be deleted, only yanked
(`cargo yank --version X.Y.Z <crate>`), which stops new projects from
choosing it but keeps it for those that already depend on it. Yank only a
version that is broken or unsafe, and publish the fix first.

## Before a test in a DAW

A test by hand in a DAW ([HOST_TESTS.md](HOST_TESTS.md)) starts by
installing the example plugins again from the commit under test, in every
format the test uses, with the DAW closed:

```sh
cargo mangue install -p mangue-pluck --release --format clap,vst3
cargo mangue install -p mangue-duck --release --format clap,vst3
cargo mangue install -p aratu-bridge --release --format clap,vst3
```

(`--format lv2` for LV2, and `--format au` on a Mac; `cargo run -p
cargo-mangue -- mangue install ...` without `cargo-mangue` installed.) A
DAW loads whatever is installed, however old: on 19 October 2026 an
overload reported in REAPER came from an installed Mangue Pluck built
before its output ceiling existed.
