# 0037: One change to the recipe format, and one rebuild

Status: proposed, 2026-11-14.

## Context

Five changes are due, and each one alone would rebuild every package, which takes about two
hours. Together they take one rebuild:

- Recipes are 32 theme files and one `sources.lock` of 5,800 lines (0034 plans a file and a lock
  per package).
- The builder hard-codes what dependencies give a build: `site` on `PYTHONPATH`, `ACLOCAL_PATH`.
  Every new language means Rust code.
- Computing a package's key needs its source tarball, because styles read the source (a
  `Cargo.lock`, `meson.options`) to write the script. Planning a system downloads every source in
  it, even for packages a cache would supply.
- What the builder does after a build (run paths, splitting, rewriting, addressing) is keyed by
  a constant, `BUILD_RULES`, bumped by hand. A missed bump shipped a broken Firefox; a bump
  rebuilds everything.
- The format has debt: `probes` (150 recipes) and `fixup` do nothing; `deps` and `extra_deps`
  mean the same thing; `core` and `toolset` are two flags for three cases.

The research behind each choice is in `design/next.md`.

## Decision

### A file and a lock per package (0034, adopted)

```text
recipes/
  gh/ghostty.toml
  gh/ghostty.lock
  gt/gtk4.toml          gtk4 and gtk4-gir, a family
  gt/gtk4.lock
  gt/gtk4/fix-x.patch   a recipe's own files
```

As 0034 describes. A one-off tool moves each package or family to its file with its pins; nobody
writes the moves by hand. `sources.lock` goes away.

### The lock holds what the style read

A recipe's lock pins its sources, as now, and also what its style produced from them:

```toml
# gh/ripgrep.lock
[ripgrep]
record = "blake3-..."           # the style's result: script, sources it added, dependencies
deps = ["rust-bin"]             # dependencies the style added

[[ripgrep.sources]]
url = "https://github.com/BurntSushi/ripgrep/archive/14.1.1/ripgrep-14.1.1.tar.gz"
hash = "sha256-..."
# ... and every crate the Cargo.lock names, as now
```

- Keys are computed from the recipe and its lock: planning reads files in grapes and never a
  tarball.
- A build fetches the source, runs the style again, and refuses to go on if the record differs
  from the lock's ("`ripgrep`'s style or source changed; `wrath update ripgrep` takes it").
- `wrath update` and the first pin write the record, as they write source hashes now. A user's
  machine never writes it: it comes with the pinned grapes.

Bazel does the same with module extensions and `MODULE.bazel.lock`.

### Search paths, declared by the recipe that reads them

```toml
# py/python3.toml
[[package.search_paths]]
variable = "PYTHONPATH"
files = ["site"]

# au/automake.toml
[[package.search_paths]]
variable = "ACLOCAL_PATH"
files = ["share/aclocal"]
```

- A build with a package among its dependencies gets that package's search paths, expanded over
  every dependency that has the directories, in dependency order, joined with `:` (or the
  `separator` given).
- The expanded variables go into the build's key: a package's search paths don't change its own
  output, so its address alone wouldn't tell its dependents that what they see changed.
- The builder's own `PYTHONPATH` and `ACLOCAL_PATH` go, and so do the recipes' workarounds
  for `GI_TYPELIB_PATH` and the like.
- Generations can use the same declarations at run time (0020's indexes), later.

This is Guix's search paths. Nix's setup hooks, shell code a dependency runs in its dependents'
builds, are what this replaces without copying.

### What the builder does after a build is a step of its own

- A build makes a raw output, keyed by its recipe and dependencies, without the post-processing.
- A fix-up step, keyed by the raw output's address and the hash of the fix-up code, makes the
  final outputs: run paths, splitting, rewriting to content addresses.
- Changing a fix-up rule reruns fix-ups, seconds per package, and content addresses stop
  everything whose output didn't change. A comment in the builder reruns fix-ups, never
  compiles. `BUILD_RULES` goes.
- Raw outputs are kept until the next `wrath clean`, so a fix-up change never needs a compile.
- The golden test of placing (`split.rs`) stays, as a check that a change in output is meant.

### The format, cleaned up

| Now | After |
|---|---|
| `deps` and `extra_deps`, concatenated | `deps` |
| `core = true` (350 recipes): the toolchain and the core tools | the default |
| `toolset = true` (24): the toolchain alone | `toolchain = "compilers"` |
| neither (7 boot recipes): the seed alone | `toolchain = "seed"` |
| `probes` (150 recipes), unused | gone |
| `fixup`, unused but kept in keys | gone |
| `enableTiny` and other camelCase options | snake_case |

`scope` stays: it says where a package lives (its own prefix, or `/usr` in builds), which is
another question than what it builds with.

## Order of the work

1. The builder reads both formats, so nothing breaks midway.
2. The tool moves recipes to files and locks, writing records for every recipe.
3. Search paths and the fix-up step, with tests.
4. The format cleanup in the moved files.
5. One world rebuild, the VM scenarios, a new image.
6. The old format and `BUILD_RULES` go.

## Consequences

- Planning, `check` and keys need no network once grapes is pinned.
- An update touches one recipe's two files, so updates in bulk merge without conflicts.
- A builder change costs seconds per package instead of a world rebuild.
- A new language's environment is a few lines in its recipe.
- Grapes grows a little: records are digests, and crate pins already live in `sources.lock`.

## Alternatives

- Five changes, five rebuilds. Ten hours of building for nothing.
- Keep `BUILD_RULES` and only add the golden test (0036's batch). Safe against missed bumps,
  but every bump still rebuilds everything.
- Hash the whole builder into every key. No bumps, but every commit to wrath rebuilds the world.
- Search paths as code (Spack, Nix setup hooks). More power, and the opacity that made setup
  hooks the thing to avoid.

## Open

- Where raw outputs live and when `clean` removes them: one generation's worth, or all until
  asked.
- Whether a record mismatch at build time should offer to update the lock in a checkout of
  grapes, where updating is what the author wants anyway.
