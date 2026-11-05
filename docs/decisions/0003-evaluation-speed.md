# 0003: Evaluate from scratch on every plan, without an incremental engine

Status: accepted, 2026-11-05 (spike S1)

## Context

`plan` should feel instant after a one-line change, including on a machine with many modules.
Nix's evaluation speed is one of the problems this project exists to fix. The plan called for a
salsa-style incremental engine with a persistent memo table, but only if evaluating from scratch
turns out too slow.

## Measurement

`distro bench` generates synthetic service modules in each module language. Each is shaped like
a real service module: a map of instances, a unit and a JSON file per instance, a system user
set through `users`, and a firewall port contributed to `networking.firewall`. One more module
iterates over every service. A generated configuration enables half of them and sets freeform
settings with a machine-fact reference. The real modules (users, PAM, syncthing, nginx and so
on) are loaded too.

It then times what `plan` does after a one-line change: load every module, evaluate the changed
configuration, build its model and diff it against the model from before. Nothing is cached
between runs.

On an AMD Ryzen AI Max+ 395 (32 threads; the evaluator is single-threaded), median of 5 to 9 runs:

| Build | Modules | Luau | Starlark |
|---|---|---|---|
| release | 159 | 48 ms | 20 ms |
| release | 609 | 206 ms | 91 ms |
| dev (dependencies optimized) | 159 | 89 ms | 56 ms |
| dev (nothing optimized) | 159 | 220 ms | 399 ms |

Time grows linearly with the number of modules. Luau is slower in release because every module
gets its own VM with the sandbox prelude.

## Decision

- Evaluate everything from scratch on each plan. Don't build the incremental engine yet.
- The budget is 500 ms for about 150 modules, checked by `scenarios/plan-budget.toml`. It is
  provisional.
- Optimize dependencies in dev builds, so tests and the acceptance run see realistic speeds.

## Consequences

- Much less code: no query graph, no persisted memo table and no invalidation bugs.
- What this doesn't cover: a real package index lookup per package, much larger configs, and
  modules that do heavier work. Revisit if `plan` on the author's laptop config passes 200 ms, or
  when the native backend (Phase B) adds work per artifact. The reads each module records are
  already the inputs an incremental engine would need.
- The acceptance scenario says "warm plan". With nothing cached, there's no difference between
  a warm plan and a cold one inside the process. Process startup adds a few milliseconds.
