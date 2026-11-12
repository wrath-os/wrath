# 0025: Build styles in typed Starlark

Status: accepted, 2026-11-11.

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

A recipe gets a build style, named by a table of its options or found from its source. A recipe
with a `script` keeps it.

Finding the style:

- The style comes from the files at the top of the source. A lock picks the language: `Cargo.lock`
  is cargo, `go.sum` is go, `package-lock.json` or `pnpm-lock.yaml` is npm, `uv.lock` or
  `poetry.lock` is python, `Gemfile.lock` is ruby. Without a lock, a build file picks it:
  `meson.build`, `CMakeLists.txt`, `configure`.
- More than one match is an error that lists what was found and asks for the style's table. A
  Rust project with a `package-lock.json` for its docs has `[package.cargo]`, empty if it sets
  nothing. A language project without its lock is an error too: styles read locks and never
  resolve versions.
- Every dependency's hash comes from the lock. One without a hash, as in an older
  `Gemfile.lock`, is pinned on its first fetch, as sources are.
- What a source holds is read once per source hash and kept, as locks read from sources are now,
  so finding the style costs a plan nothing after the first.

The shape in TOML:

- There is one kind of package. The style's options go in a table named after it, as a
  module's options do in a configuration:

  ```toml
  [[package]]
  id = "ripgrep"
  source = "ripgrep"

  [package.cargo]
  features = ["pcre2"]
  ```

- The table is the only way to name a style: a recipe whose source makes the choice and that
  sets nothing has no table, and one that has to choose has the table, empty if need be. Tables
  for two styles are an error, and so is a field that is neither the recipe's nor a style.
- A kind of package per style, such as `[[cargo_package]]`, was rejected: the header would
  have to be written, so the style couldn't be found, and every tool reading recipes would
  have to know each kind.

What a style is:

- A style declares what it takes as options, with the `option()` and `types` modules use: a type,
  a default and a doc each. A wrong name or type is an error at the recipe's line, and each
  style's options can be listed with their docs.
- A style produces a package record: the same record a recipe with a plain `script` has, with its
  script, dependencies, environment, sources and outputs. A plain recipe is one that fills the
  record in directly, and `lab show <package>` prints the record whichever way it was made.
- Styles are checked like modules, with every function annotated.
- Styles run at plan time in a hermetic evaluator: no clock, no network, and no reads beyond what
  the recipe declares.
- Rust keeps the store, sandbox, fetching and pinning, ELF handling, output splitting, keys and the
  graph. It parses: TOML, JSON and YAML are decoded in Rust and given to styles as plain values,
  along with reading a file from a pinned source and the action record type. The style says
  what an entry means: its URL, its hash, how it's vendored. Lines of text, as `go.sum` is, are
  split in the style, and get a Rust decoder only if that proves slow. Rust stops knowing about
  languages, and a new one is a style, with at most a new decoder.
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

One kind of option:

- A configuration's `builds` sets a package's options, such as `builds.tiny-nano.options`.
  These are the same options the recipe or its style declares, checked the same way. What a
  recipe sets in `[package.cargo]` is its own default, and `builds` changes it as it changes any
  other. A plain recipe's script reads them as `OPTION_<name>`. A style reads them as its
  arguments.
- Recipes declare their options already, ahead of styles: `[package.options.<name>]` with `type`
  (`bool`, `str`, `int` or `list`), `default` and `doc`. The script starts with every option
  exported, so it carries no defaults of its own. A `builds` entry that names an option the recipe
  doesn't declare, or gives the wrong type, is an error at `builds.<name>.options`. Before, it was
  silently ignored and only changed the key.

## Consequences

- Cargo and Go support become about 100 lines of Starlark, and the 7 copies of the vendoring block
  go.
- Moving a recipe to a style must produce the same record, so nothing rebuilds. That's the test of
  each step.
- Plan pays for evaluating recipes only when one changed. plan-budget gains a recipe case, cold
  and warm.
- Actions added to the graph during a build, such as one per crate, need a decision of their own.
