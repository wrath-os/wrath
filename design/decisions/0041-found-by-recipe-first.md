# 0041: Builds are found by their recipes first

Status: accepted, 2026-11-16.

## Context

A package has two keys (store.md). Its unresolved key hashes its recipe and its dependencies'
unresolved keys, so it's known from recipes alone. Its resolved key, which builds and records
use, hashes its recipe, its dependencies' store paths, and a build environment that depends on
what's in those paths: the directories that become search paths, whether `lib` and the loader
are there, and the `.pc` files a dependency's requirements come from (0019). That's how a rebuild
with identical output stops there: a dependent of zlib keys on zlib's path, not on its recipe.

The cost is that working out one resolved key needs the contents, or a cache's listing, of
every dependency, all the way down to the seed. A machine booted from an image has the system's
packages but not gcc, glibc's build or rust. It can't work out wrath's resolved key, so it
doesn't recognise the wrath it's running, and rebuilds it unless a cache has listings of the
whole toolchain. The same goes for a machine installed from that image, one switched with
`apply --host`, and a plan on any machine without a cache: each answers "what do I have" by
resolving the whole build graph.

A key that is a digest of its inputs' digests doesn't look inside a dependency to name a build.
Content-addressed builds do, and they all have this need: resolving one requires what its inputs
resolved to.

## Decision

- Every build, and every fetch, keeps a hint beside the store: `hints/<unresolved key>` names the
  resolved key it built or fetched as, the local form of the hints 0038 lays out for caches.
- Before resolving a package, a build or plan works out its unresolved key and reads the hint. A
  hint whose resolved key has a record is a hit: the package's outputs come from the record, and
  nothing about its dependencies' contents is read. Only a miss resolves, as today.
- A hit says which build the package is, not that its paths are in the store. A dependent that
  misses and builds needs what it builds with, so a hit that's absent is fetched or, failing
  that, built, before anything that reads its contents.
- Images, `install` and `apply --host` bring the hints of a system's build graph along with its
  records, as they bring `builds/`, `fixed/` and `refs/` now.
- Hints are trusted as records are: written by root beside the store, or brought by whoever
  installs the machine. A cache's hints stay out of this: following an unsigned hint from a
  cache would let it substitute another signed build, so that waits until hints are signed or
  checked against the trace.

## Consequences

- A machine recognises everything whose recipes haven't changed without any listing, cache or
  toolchain, and a plan reads one file per package instead of resolving the graph. Recipes are
  the same on every machine with the same grapes, so a key that differs between machines, as 0038
  worries about, matters only for what was changed.
- Early cutoff stays where it pays: when a recipe changes, its dependents miss by unresolved key,
  resolve, and stop at the first identical output, as today. Offline, that needs the changed
  dependency in the store, which building it puts there.
- The same unresolved key built twice, non-reproducibly, gives two resolved keys. The hint names
  the last one, and both are consistent closures; `wrath dev repro` is still where that shows.
- Listings of tools a machine doesn't carry aren't needed in images.

## Work

1. Write hints where records are written: a build's record, a fetched trace. Small.
2. The early lookup in the chain: work out the unresolved key first, and on a hit fill in what
   dependents read (paths and outputs, the view and requirements, search paths, indexes,
   closures) from the record, without the contents. The largest part, in `chain.rs`, with tests
   for a hit, a miss over hits, and a hit that's absent.
3. A hit that's absent and needed: fetch or build it before a dependent reads it.
4. Images, `install` and `apply --host` bring hints, on top of the records they bring now.
5. The VM check: an image with no cache plans nothing and switches.

## Built so far

The first cut, 2026-11-16, found a whole system or nothing:

- Every package worked out keeps its hint. A system build first works out the unresolved keys of
  what it uses and its builds; when each of them has a hint whose record is there, the system is
  put together from those, and nothing is resolved, built or fetched, toolchain included. Else it
  builds as before. A plan does the same, and says only what a cache would bring.
- A package can be found with its paths missing, as a traced one can: btrfs-progs and cryptsetup
  go into the initrd, so the running system doesn't refer to them, and a machine doesn't have
  them. What the system runs is fetched after it's put together, where it isn't there.
- Putting a system together runs its actions, and a machine installed from an image has no action
  database, so it ran them all again, the initrd's with packages it doesn't have. What each action
  made is now kept beside the store too, in `actions/` with its files in `e2e/objects/`, and read
  when the database lacks it.
- Each system lists its builds, hints and actions in `builds/<system>`, and images, `install` and
  `apply --host` bring that list and what it names, with the `refs/` of what they copy.
- Checked: three configurations, the desktop one among them, put together the same system either
  way, in 0.15 to 0.18 seconds where resolving took up to 2.9. A VM booted from an image with no
  cache to reach planned nothing, then upgraded from its repository and switched. Another, switched
  over SSH with `apply --host` to a configuration adding jq, which its image never had, got jq's
  hint and record along with it, and planned that configuration with nothing to build.
- Then, 2026-11-17, package by package: when something the system uses isn't found, it and what
  it's built with, all the way down, are worked out as before, and every other package found is
  kept. A changed recipe works out the toolchain and the changed package's dependencies, and
  leaves the rest of the system alone. With the hints of jq and gcc removed, a system came out the
  same in 0.7 seconds, where everything found took 0.3.
- `scenarios/native-host.toml` and `native-image-install.toml` check, with no cache, that an image,
  `install` and `apply --host` bring what the machine needs to plan with nothing to build.
- A machine with no cache and no seed can't build what it misses. Its plan says the builds need a
  seed it doesn't have, and to use a cache or build where the seed is and apply with
  `apply --host`; an apply fails with that before changing anything. `native-host` checks both.

## Open

- Cache hints. A trace that names the unresolved key it was built for, under its signature, would
  let a machine follow a cache's hint without trusting it about which build that is.
- Build descriptions: writing down what a key hashes, so keys can be
  compared between machines, checked, and sent to a builder without the recipes. That's its own
  decision.

## Alternatives

- Images bring listings of every tool in the build graph, which a machine reads as it reads a
  cache's. It fixes images, `install` and `apply --host`, but every plan still resolves the whole
  graph, and a machine without an image's listings, or a cache, is back where it started.
- Images bring the tools themselves. Rust alone is 389 MB.
- Naming builds by unresolved key only. A change anywhere below would then rebuild
  everything above it, which store.md measured: a zlib comment rebuilt 23 packages instead of 1.
