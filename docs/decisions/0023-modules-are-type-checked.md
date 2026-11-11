# 0023: Modules are type-checked

Status: accepted, 2026-11-11.

## Context

Modules are Starlark (0004). A misspelt `ctx` method or an argument of the wrong type showed up
only when a render reached that line, which for a module that's off, or a branch a test doesn't
take, is when a user enables it. starlark-rust has optional type annotations, checked as functions
run, and a static checker that reads a whole file before it runs; Buck2 uses both.

## Decision

Module and library files are parsed with types on, and checked statically before they're
evaluated. The `ctx` a render gets is the type `Context`, whose methods carry their parameter
types, so `def render(ctx: Context) -> None:` makes every `ctx` call in it checked when the module
loads. The built-in modules annotate `ctx` everywhere. Annotations stay optional: unannotated code
runs as before and the checker only reports what it can prove wrong.

## Consequences

- A typo in a module that's off is an error when it loads, not when someone turns it on.
- Values read from options are untyped to the checker (`ctx.read` returns any value), so checks stop
  at the option boundary; option types themselves are checked by distro's own schema.
- Annotating helpers and libraries further is ordinary work, file by file.
