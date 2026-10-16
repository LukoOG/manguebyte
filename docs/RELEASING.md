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

Each crate carries its own `README.md` and both licence files, because a
package holds only the files under its folder; CI checks the licence copies
against the root ones. A crate that depends on itself for its own tests does
so through a path-only dev-dependency, which cargo drops on publish.

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
