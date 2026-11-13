# 0018: Building on machines, and applying from elsewhere

Status: accepted, 2026-11-10.

## Context

Since 0017, only a checkout of distro can build: the recipes, the pins and the builder's work
directory live in `build/`, and the store a checkout builds into is `build/work/store`. A machine
installed from an image has `/store` and the `distro` command, but nothing to build with. So on a
machine `apply` after a configuration change, `run` and `shell`, and running the distro a lock
pins all fail or fall short. That's the gap between the Framework booting and the Framework being
managed.

Three ways a machine can get what a configuration needs:

1. Another machine builds it and copies it over.
2. The machine builds it itself.
3. The machine fetches it built from a cache, by the same key it would build it under.

They don't exclude each other. The question is the order, and what each needs from the model.

## Decision

All three, in that order, with one store format and one key throughout.

**Applying from elsewhere: `--host NAME`.** `distro apply --host framework` builds the system
where it runs, copies the store paths the machine lacks over SSH, the way the VM harness copies
generations in, and runs `distro apply /store/<hash>` there. `plan --host` compares against the
model the machine runs. The machine needs SSH and root, nothing else. This is what we need first:
it makes the Framework manageable from a checkout today.

**Building on the machine.** The builder is already a library inside `distro`. The distro package
carries its recipes and pins in `share/distro/build`, and builds into the machine's own `/store`,
with its records and downloads in `/var/lib/distro/build`. The seed comes by hash like any other
download. A machine that runs a distro other than the one its lock pins builds that distro's
`distro` recipe from the pinned source, and runs it, which brings back what pinning promised.

**A cache.** A store path is named by its content and found by its unresolved key, so a cache is a
map from key to path plus the paths themselves, signed by the builder that made them. A machine
asks the cache before building and builds what it doesn't find. Who may write to a cache, and
which keys a machine trusts, is the cache's own decision, together with the build service that
standalone homes wait on, since both decide who may put what into a store.

**How a cache sends a path** (added 2026-11-13). Addresses stay one flat hash over a path's
entries. Nix fetches a whole path, so one changed file in LLVM downloads all of LLVM again; Bazel
and Buck2 store single files by digest and fetch only the ones missing. We get the second without
changing addresses, by keeping a list of files next to each path:

- For each path the cache keeps a manifest: every entry's name, kind, mode and file digest. The
  files are stored once each, by digest.
- A machine fetches the manifest, looks up which digests its store already has, fetches the rest
  and checks each against its digest as it arrives. It then assembles the path and recomputes
  its address, which also checks the manifest, so the manifest needs no trust of its own.
- A file is stored with references to its own path zeroed, as the address already hashes it, and
  the manifest lists the offsets. The same file in two versions of a package is then stored
  once, and the machine writes the path back in as it assembles. Nix can't do that, because a
  file that names its own path differs in every version.
- The store keeps an index of file digests for this, which also lets identical files share one
  inode.
- Later, if needed: content-defined chunks for large files that change a little between versions
  (as Attic does for Nix), zstd, and not fetching what isn't needed: a machine applying wants the
  runtime closure only, and build-time dependencies of paths the cache has are never fetched.

Merkle addresses, hashing each directory from its children, would add skipping a whole unchanged
directory by one digest, and checking every byte as it arrives, manifest included. Packages are
small enough that per-file skipping covers nearly all of the first, and either could come later
in the transfer format alone, so addresses stay flat.

## Consequences

- `--host` is the first step and needs nothing new in the model: the plan, the copy and the apply
  are what exist already, run across SSH.
- A machine that builds needs disk and time for the toolchain the first time, until the cache
  exists. A laptop that applies through `--host` never does.
- `Lab` learns to keep its records apart from its store, so `/store` holds only store paths.
- Builds run in bubblewrap, which every system has. It's Linux's; building on macOS later needs
  a sandbox of its own there, so the builder asks the sandbox for what a build may see and do, and
  doesn't assume bwrap's options.
- The pinned distro is built, not just noted, once machines build.

## Alternatives

- Only a cache, no building on machines. Simpler machines, but a machine is then stuck whenever
  the cache lacks something, and someone has to run the cache before anything works.
- Only `--host`. Enough for one person with a checkout, but a machine can't change itself, and
  `run` and `shell` on it stay broken.
- Ship every package anyone might want in the image. Large, never complete, and it doesn't help
  with a changed configuration.
