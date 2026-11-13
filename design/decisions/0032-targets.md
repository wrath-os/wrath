# 0032: Packages build for a target, natively

Status: proposed, 2026-11-13.

## Context

Everything builds for x86_64 Linux today, and that's written into the tool in a few places: the
toolset's `TARGET` and the seed's gcc path in the chain, and seven recipe lines such as zig-bin's
download URL and the loader's name `ld-linux-x86-64.so.2`.

Projects (0010) are meant to replace devenv, and devenv is used on macOS as much as on Linux. A
project and the packages it names have to work on aarch64 macOS, as nixpkgs does, and later on
aarch64 Linux. Systems stay Linux.

nixpkgs writes each package once and evaluates it once per platform. Most packages say nothing
about the platform, because stdenv carries the compiler, libc, triple and linker flags. A few use
`lib.optionals stdenv.isDarwin`. Two things there are hard to work with:

- Cross builds. Every dependency has to be sorted into build, host and target platforms
  (`nativeBuildInputs`, `buildInputs`, `depsBuildBuild`, ...), and getting it wrong shows up as a
  build that works natively and fails crossed. Few people understand the splicing behind it.
- What a package does on one platform is only visible by evaluating it there and reading the
  derivation, so a Linux user can't easily see why the macOS build differs.

"Platform" already names a set of modules (0030), so this uses "target".

## Decision

### A target is an input to evaluating a recipe

- A target is an architecture and an OS, written as nixpkgs does: `x86_64-linux`,
  `aarch64-linux`, `aarch64-darwin`.
- A recipe is written once. Evaluating it for a target gives that target's action, and the
  target is part of every store address, so the same recipe for two targets gives two paths.
- A package set doesn't grow per target: there's one `build/recipes`, and a target only decides
  which entries apply and how they're merged.

### No cross builds

A package for a target is built on a machine of that target, locally, on a builder (0018), or
fetched from a cache. A build never knows another target exists, so a recipe has one dependency
list and no build/host split.

When there's no machine of the target at hand, a build can run under emulation (binfmt and
qemu-user on Linux, Rosetta on macOS) and still believe it's native. That's slow, but needs
nothing in recipes. Building the first toolset for a new target, which needs something to start
from, uses a seed pinned for that target, as x86_64 does now.

### Where the differences go

In order of preference, so most recipes never mention a target:

1. The toolset. Each target has its own seed and chain, which decide the compiler, libc, linker,
   loader path and triple. Recipes reach these through variables, as they reach `$LAB_LIB_glibc`
   today, so `ld-linux-x86-64.so.2` becomes `$LAB_LOADER`.
2. Styles. A style is Starlark and gets the target, so the meson, cargo, zig and autotools styles
   pass the right triple or `-Dtarget` themselves.
3. Placeholders, like `{version}`: `{arch}`, `{os}` and `{target}` in sources and scripts. Binary
   downloads such as zig-bin and Rust's need one line each:

   ```toml
   sources = [{ url = "https://ziglang.org/download/{version}/zig-{arch}-{os}-{version}.tar.xz" }]
   ```

4. Target tables, for what's left. `targets` says where a recipe builds (everywhere the toolset
   supports when left out), and `[package.target.<match>]` tables are merged over the entry on
   matching targets, borrowing Cargo's `[target.'cfg(...)'.dependencies]`:

   ```toml
   [[package]]
   id = "foo"
   targets = ["linux", "aarch64-darwin"]
   extra_deps = ["zlib"]

   [package.target.linux]
   extra_deps = ["libcap"]

   [package.target.aarch64-darwin.meson]
   args = ["-D x11=disabled"]
   ```

   A match is a full target, an OS or an architecture, nothing more. Tables merge, lists extend
   and other values replace, from the least specific match to the most. Logic beyond that belongs
   in a style, where there's a real language, so TOML doesn't grow conditions of its own.

### Seeing it

- `distro show foo --target aarch64-darwin` prints the recipe as merged for that target, with
  each value's table, on any machine.
- `distro dev lint` evaluates every recipe for every target and fails on a target name written
  outside a target table or a `targets` list, which is how `x86_64` crept into recipes.
- `distro why` and plans name the target when a configuration has more than one.

### Locks

Sources are pinned once. A source that differs per target, such as a binary download, is pinned
per target, and locking fetches it for every target the configuration names, from any machine:
fetching isn't building. A project lists its targets, so a Linux user's `distro lock` keeps the
macOS teammate's pins:

```toml
targets = ["x86_64-linux", "aarch64-darwin"]
```

A system has one target, its machine's.

### macOS

What runs on macOS is projects, and later homes. Systems and their modules stay Linux. macOS
needs work of its own, each piece its own decision when it's started:

- A toolset: clang with Apple's linker or lld, against Apple's SDK and libSystem, which are
  Apple's and can't be built. How the SDK is fetched and pinned, and what its license allows, is
  open.
- A sandbox. bubblewrap is Linux's; macOS has `sandbox-exec`, deprecated but what Nix uses. The
  builder already asks the sandbox for what a build may see (0018).
- Where the store lives. `/store` at the root needs `synthetic.conf` and an APFS volume, as Nix
  does with `/nix`.
- Project services lower to launchd agents, or to distro's own supervisor (ideas.md).
- Which packages a project can use on macOS starts small: what the lint finds builds there.

## Order

1. Make the target explicit on x86_64 Linux: placeholders, `$LAB_LOADER`, the target in store
   addresses, and the lint. Small, and stops more hard-coding.
2. aarch64 Linux, built natively: a seed and chain. This checks the model on a second target
   without a second OS.
3. aarch64 macOS for projects: toolset, sandbox, store, and the packages projects need first.

## Alternatives

- Cross builds, as nixpkgs. They make one fast machine build for every target, but every recipe
  then has to sort its dependencies by role, and the failures appear only when crossed. Native
  builds, emulation and a cache cover the same need with nothing for recipe authors to learn.
  Cross building only the first toolset for a new target is still possible if a seed can't be
  found.
- A recipe file per target. Simple to read, but every fix is made several times and they drift.
- Conditions in TOML (`if`, expressions). That's a language inside TOML; Starlark styles already
  are one.

## Open

- Removing from a list on one target. Extending covers every case found so far.
- Whether a target's toolset variables (`$LAB_LOADER` and the rest) are a fixed set or something
  each toolset declares.
- Whether 1 changes every store address, and so goes with the next full rebuild.
