# 0004: The module language

Status: proposed, 2026-11-05 (spike S4). The choice is a human decision.

## Context

Users configure systems in TOML. Modules are written by distro maintainers and power users in an
embedded language. A module declares typed options, reads resolved config through the host API,
and renders files, units, packages and settings for other modules' options. Modules must be
deterministic, sandboxed and fast. Errors must point at the module's source.

Three candidates were built to the same host API, each running the same eight modules: system,
users, firewall, syncthing, nginx, PAM, the base PAM stacks and fprintd. They live in
`modules/luau`, `modules/starlark` and `modules/typescript`. A separate survey of other
options pruned Rhai, Nickel, CUE, Pkl, KCL, Jsonnet, Dhall, Koto, Rune and others.

## What was measured

All three:

- render the fixture configs to byte-identical output, with the same model hash
  (`check --determinism modules`, parity tests in each crate);
- render the same output across 5 fresh interpreters per config with modules loaded in shuffled
  order;
- stop a module that loops forever, and hand modules read-only values.

Plan time after a one-line change with 159 to 161 modules, median (decision 0003):

| Build | Luau | Starlark | TypeScript |
|---|---|---|---|
| release | 48 ms | 20 ms | 45 ms |
| dev, dependencies optimized | 87 ms | 55 ms | 71 ms |

## Comparison

| | Luau (mlua) | Starlark (starlark-rust) | TypeScript (QuickJS + swc strip) |
|---|---|---|---|
| Module code | `cfg.port`, `pairs(...)`, `..` for strings | `cfg["port"]`, `cfg.get("root")`, Python syntax | `cfg.port`, template strings, `Object.entries` |
| Static types | None in our setup; luau-lsp could add them | None today; the typing extension is experimental | `tsc` infers config types from the option declarations |
| Editor support | luau-lsp with a definition file, not set up | starlark LSP library, would need our own binary | Any editor with TypeScript support |
| Lists vs tables | `{}` is ambiguous; the host fixes it from the schema | distinct | distinct |
| Numbers | doubles | integers and floats | doubles |
| Determinism | needs a prelude and a lint against `for k, v in t` | by design | by language spec (key order); `Date`, `Math.random`, `WeakRef` removed |
| Error locations | line | exact span | line |
| Rust embedding | clean, scoped callbacks, no unsafe | derive macros need `unsafe`; files re-evaluated per render | host functions must be `'static`, so renders replay against a snapshot of reads |
| Dependencies | Luau VM (C++) | pure Rust, large | QuickJS (C) and swc (large) |

The TypeScript typing catches mistakes before anything runs. With `cfg.prot` instead of
`cfg.port`, `tsc` says `Property 'prot' does not exist on type '{ user: string; port: number;
... }'`. A wrong default type is caught the same way. In Luau and Starlark the same typo only
fails when the module runs, and only if that path executes.

## Options

1. **TypeScript.** The best authoring experience: the most familiar syntax for developers, static
   types inferred from the schema, and editor support everywhere. Costs: two large native
   dependencies, the replay workaround, doubles for numbers, and a lock-down list to maintain
   (any new global in QuickJS has to be checked).
2. **Starlark.** Deterministic by design, the fastest, the best error spans, and familiar to
   Bazel and Buck2 users. Costs: no static types today (static checking is the untested part of
   the survey's recommendation), dict-style field access, `unsafe` in the bindings, and API churn
   between minor versions.
3. **Luau.** Small and fast with clean bindings. Costs: the `{}` ambiguity and doubles are
   permanent, determinism depends on our prelude and lint, and its types would need luau-lsp
   wired up.

## Recommendation

TypeScript, because module authors get type checking and completion from the schema they
already write, with tools they already have. The runtime costs are contained in one crate and
measured: it is as fast as Luau, and the replay workaround is tested by the same parity and
determinism checks as the others.

If the dependency weight or the lock-down surface is a concern, Starlark is the fallback, but
only after a short spike shows its static checker catching the `cfg.prot` typo with a useful
message.

## What follows from the choice

- Keep the chosen runtime and delete the other two crates, keeping the parity test as a
  determinism test.
- Module documentation is generated from option declarations in either case.
- The determinism suite stays permanent.
