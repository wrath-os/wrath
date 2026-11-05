# 0004: The module language

Status: proposed, 2026-11-05 (spike S4). The choice is a human decision.

## Context

Users configure systems in TOML. Modules are written by distro maintainers and power users in an
embedded language. A module declares typed options, reads resolved config through the host API,
and renders files, units, packages and settings for other modules' options. Modules must be
deterministic, sandboxed and fast. Errors must point at the module's source.

Four candidates were built to the same host API, each running the same eight modules: system,
users, firewall, syncthing, nginx, PAM, the base PAM stacks and fprintd. They live in
`modules/luau`, `modules/starlark`, `modules/typescript` and `modules/python`. A separate survey
of other options pruned Rhai, Nickel, CUE, Pkl, KCL, Jsonnet, Dhall, Koto, Rune and others. Among
widely known languages, real Python, Go, Ruby, HCL and YAML templating were ruled out: they can't
be sandboxed in process, can't be embedded in Rust, or can't express render logic well.

"Typed Python" means modules written in the common subset of Python and Starlark, with type
annotations. pyright checks them against stubs that `distro stubs` generates from the option
declarations. starlark-rust runs them, parsing annotations without checking them at run time.

## What was measured

All four:

- render the fixture configs to byte-identical output, with the same model hash
  (`check --determinism modules`, parity tests in each crate);
- render the same output across 5 fresh interpreters per config with modules loaded in shuffled
  order;
- stop a module that loops forever, and hand modules values they can't change.

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

What the type checkers catch before anything runs:

| Mistake | TypeScript | Typed Python |
|---|---|---|
| misspelled config field, `cfg.prot` / `cfg["prot"]` | yes | yes |
| reading an optional field without checking it | no | yes |
| misspelled option setting (`defualt =`) | yes | yes |
| a default of the wrong type | yes | no (`default` is `Any`) |

In Luau and plain Starlark all of these fail only at run time, and only when that path runs.

## Options

1. **Typed Python.** The largest audience, real type checking with the editors people already
   use, and Starlark's runtime: deterministic by design, the fastest, exact error spans, pure
   Rust. Costs:
   - It is a subset of Python with no classes, `while`, `try` or `import`. Starlark rejects
     those at load time with a clear error, but pyright doesn't warn first.
   - Stubs have to be regenerated when options change. `distro stubs --check` in CI catches
     stale ones.
   - Sharing code between module files is unsolved, because Starlark's `load()` isn't valid
     Python.
   - The `unsafe` in starlark-rust's derive macros, and API churn between its minor versions.
2. **TypeScript.** Types inferred directly from the schema, and a familiar language for web
   developers. Costs: two large native dependencies, the replay workaround, doubles for numbers,
   and a list of globals to keep locked down.
3. **Starlark without types**, or **Luau.** Both work, but they give up static checking, which
   is the main difference between the options.

## Recommendation

Typed Python. It gives module authors the language they most likely know and type checking
comparable to TypeScript's, and it keeps the runtime that is deterministic by construction,
fastest and pure Rust.

Before committing, settle how module files share code (a library module, `load()` with
generated Python import shims, or no sharing). If that turns out badly, TypeScript is the
fallback.

## What follows from the choice

- Keep the chosen runtime and delete the others. The parity tests become determinism tests.
- Module documentation is generated from option declarations whichever language is chosen.
- The determinism suite stays permanent.
