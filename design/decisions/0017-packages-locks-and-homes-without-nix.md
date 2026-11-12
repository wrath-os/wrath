# 0017: Packages, locks and homes without Nix

Status: accepted, 2026-11-10.

## Context

0014 replaced Nix with distro's own builder and store. Systems, images, installs and most VM tests
now use only the store. Four things still go through Nix, because each leaned on something Nix
provides that has no direct counterpart yet:

- The lock pins nixpkgs revisions, and resolves package names against them.
- Sources, which configurations include files from, are fetched with Nix's `fetchTree`, pinned by
  Nix's tree hash. A pinned distro is built from its flake.
- `run` and `shell` build an environment of any nixpkgs package and run it.
- A user's own home, `~/.config/distro`, is built and switched by the user without root, which
  works because Nix's daemon lets users add to the root-owned store.

These need deciding before the Nix code can go.

## Decision

**Packages and the lock.** Recipes live in distro's repository, so distro's revision pins every
package's version and source. The lock pins distro, as it does today with the source `distro`, and
that pin is what makes a configuration build the same packages years later. It stops pinning
packages one by one: `packages` in the lock goes. `distro update` moves the pin to a newer distro,
and `plan` shows which packages that changes, from the two revisions' recipes.

A package from an older distro, `jq@1.7`, means the recipe from the newest distro revision that had
that version, built from that revision. Content addressing shares whatever comes out the same, so
it costs only what differs. That comes after this decision; for now a configuration gets the
packages of the distro it pins.

**Sources.** A source is a URL that distro fetches itself: a git repository at a commit, or an
archive. It's pinned by the SHA-256 of its files in a fixed order, so the same tree has the same
hash however it was fetched. The lock keeps the URL, the commit when there is one, and the hash.
`distro update SOURCE` fetches the newest commit and pins it. A repository with a recipe file at
its top can be a source of packages, `source#name`, built against the pinned distro's packages.

**Running packages.** `distro run NAME` and `distro shell NAME...` build the packages' recipes with
the pinned distro and run them with their programs on `PATH`. On a machine with a store, that's
`/store`. In a checkout, where the store is `build/work/store`, distro runs the program in a mount
namespace with that directory at `/store`, the same way the build sandbox does.

**Homes.** A user's packages and files are part of the system's configuration, as `[users.NAME]`
already allows, and switch with the system. Standalone homes, which a user builds and switches
without root from `home.toml`, are dropped for now. They come back with a service that builds for
users, the subject of its own decision, since it decides who may put what into a store everyone
shares.

**Then.** With these done, the Nix code goes: `factory/nix`, the backend crate, the Nix paths of
the harness, and the scenarios and acceptance checks that still name Nix images, the last after
review.

## Consequences

- Configurations can no longer name arbitrary nixpkgs packages. A package without a recipe is an
  error that says so, and writing recipes is the work that follows.
- `distro info` and version lookups read recipes, and are fast because nothing evaluates.
- Locks lose their per-package entries. Old locks still read; their package entries are ignored.
- Standalone homes are gone until the build service exists; `home.toml` is an error that says so.

## Alternatives

- Keep a lock entry per package, pinned to a recipe's hash. More precise, but every recipe change
  would rewrite every lock, and the distro pin already determines the recipe.
- Let users write to the store directly, with group permissions. Simple, but then any user can
  plant what another user's programs run.
- Keep standalone homes on Nix until the build service exists. Keeps the feature, but keeps Nix
  and its daemon on every machine that uses one.
