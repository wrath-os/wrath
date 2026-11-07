# 0004: The module language

Status: accepted, 2026-11-05 (spike S4), and confirmed 2026-11-07. Modules are written in
Starlark. Static typing is deferred.

## Context

Users configure systems in TOML. Modules are written by distro maintainers and power users in an
embedded language. A module declares typed options, reads resolved config through the host API,
and renders files, units, packages and settings for other modules' options. Modules must be
deterministic, sandboxed and fast. Errors must point at the module's source.

Four candidates were built to the same host API, each running the same eight modules: system,
users, firewall, syncthing, nginx, PAM, the base PAM stacks and fprintd. A separate survey of
other options pruned Rhai, Nickel, CUE, Pkl, KCL, Jsonnet, Dhall, Koto, Rune and others. Among
widely known languages, real Python, Go, Ruby, HCL and YAML templating were ruled out: they can't
be sandboxed in process, can't be embedded in Rust, or can't express render logic well.

"Typed Python" means modules written in the common subset of Python and Starlark, with type
annotations, checked by pyright against stubs generated from the option declarations and run
by starlark-rust with annotations parsed but not checked.

## What was measured

All four:

- rendered the fixture configs to byte-identical output, with the same model hash;
- rendered the same output across 5 fresh interpreters per config with modules loaded in
  shuffled order;
- stopped a module that loops forever, and handed modules values they can't change.

Plan time after a one-line change with 159 to 161 modules, median (decision 0003):

| Build | Luau | Starlark and typed Python | TypeScript |
|---|---|---|---|
| release | 48 ms | 20 ms | 45 ms |
| dev, dependencies optimized | 87 ms | 55 ms | 71 ms |

## Comparison

| | Luau | Starlark | TypeScript | Typed Python |
|---|---|---|---|---|
| Audience | Neovim, games | Bazel, Buck2 | web and JS developers | the largest, and common among sysadmins |
| Module code | `cfg.port`, `..` | `cfg["port"]` | `cfg.port`, template strings | `cfg["port"]`, f-strings |
| Static types | none | none | `tsc`; types inferred from option declarations, `ctx.read<T>` needs `T` | pyright; generated stubs type every `ctx.read` path |
| Editor support | needs luau-lsp setup | needs our own LSP binary | any editor | any editor with Pylance, pyright or basedpyright |
| Lists vs tables | `{}` ambiguous | distinct | distinct | distinct |
| Numbers | doubles | integers and floats | doubles | integers and floats |
| Determinism | prelude and lint | by design | key order by spec; globals removed | by design |
| Error locations | line | exact span | line | exact span |
| Rust embedding | clean | `unsafe` in derives; re-evaluated per render | `'static` callbacks force replaying renders | as Starlark |
| Dependencies | Luau VM (C++) | pure Rust | QuickJS (C) and swc | pure Rust |

What the type checkers caught before anything ran:

| Mistake | TypeScript | Typed Python |
|---|---|---|
| misspelled config field, `cfg.prot` / `cfg["prot"]` | yes | yes |
| reading an optional field without checking it | no | yes |
| misspelled option setting (`defualt =`) | yes | yes |
| a default of the wrong type | yes | no (`default` is `Any`) |

## Decision

Starlark. It is deterministic by design rather than by a prelude or lock-down list, the fastest
of the four, gives exact error spans, and is pure Rust. Its purity and frozen values match what
the evaluator needs; laziness and dependency tracking live in the host.

Static typing is deferred. Two routes remain open, and neither changes the runtime:

- typed Python checked by pyright, as in the spike: mature, and editors already support it;
- starlark-rust's own type checker, as used by Buck2: written for Starlark, but younger.

Sharing code between module files is also open: a host-provided library, `load()`, or none.
The modules so far haven't needed it.

The Luau, TypeScript and typed Python spikes were removed after this decision and remain in
version control history.

## Consequences

- The cross-language parity tests are now snapshot and determinism tests of the Starlark
  modules.
- Module documentation is generated from option declarations.
- The determinism suite stays permanent.
- `unsafe` in starlark-rust's derive macros, and API churn between its minor versions, are
  accepted costs.
