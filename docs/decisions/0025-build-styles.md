# 0025: Build styles in typed Starlark

Status: proposed, 2026-11-11.

## Context

Recipes are TOML with a shell `script`. No build system has Rust of its own except Cargo and Go,
whose locks are read in Rust (about 200 lines). The weight is repetition in the recipes:

- The meson setup, build and install lines appear about 56 times.
- An identical 20-line Cargo vendoring block appears in 7 recipes.
- The Go proxy setup appears in 3.

Adding a language today means writing Rust. Nix and Bazel show the other extreme: a large shared
library of build functions, with overrides, overlays and rule sets released apart from the core,
that every user depends on and nobody can read whole.

## Decision

A recipe may name a build style, such as `build = "meson"` with its arguments, or a `.star` file
beside it for a build of its own. Recipes without either keep their `script`.

- A style is a typed Starlark function from the recipe's fields to an action record: script,
  dependencies, environment, sources and outputs. It's checked like a module, with every function
  annotated.
- Styles run at plan time in a hermetic evaluator: no clock, no network, and no reads beyond what
  the recipe declares.
- Rust keeps the store, sandbox, fetching and pinning, ELF handling, output splitting, keys and the
  graph, and gives styles a few primitives: decoding TOML and JSON, reading a file from a pinned
  source, and the action record type. Rust stops knowing about languages.
- Keys come from the evaluated record. A change to a style rebuilds only recipes whose record
  changed.
- Evaluated records are kept, keyed by the recipe file, the Starlark files it loaded, the hashes
  of what it read and the host API's version. A plan with nothing changed reads records only.

Discipline:

- The styles live in this repository and change with it. There are no rule sets from elsewhere.
- A style is added only when at least three recipes repeat the block it replaces. Each has
  golden tests of the records it produces.
- There is no override or overlay layer, no toolchain resolution and no helpers of helpers. A
  recipe that doesn't fit writes its own `.star` file or a plain script.

## Consequences

- Cargo and Go support become about 100 lines of Starlark, and the 7 copies of the vendoring block
  go.
- Moving a recipe to a style must produce the same record, so nothing rebuilds. That's the test of
  each step.
- Plan pays for evaluating recipes only when one changed. plan-budget gains a recipe case, cold
  and warm.
- Actions added to the graph during a build, such as one per crate, need a decision of their own.
