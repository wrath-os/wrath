# 0044: What a module reads and writes comes from its code

Status: accepted, 2026-11-23. Done 2026-11-24.

## Context

A module lists the options it sets in `writes` (0004). That's how wrath knows to run the ssh
module before the firewall module reads `allowed_tcp_ports`: reading an option renders its writers
first. A `ctx.set` outside the list fails, but only when the line runs, so a branch no test takes
fails the day someone turns it on. And the list is one more thing to keep in step with the code by
hand. Of 50 modules in grapes, 37 write nothing, and the rest repeat in `writes` what their
`ctx.set` calls already say.

Other things an author keeps in step by hand, or that the checker can't see:

- `ctx.read` returns an untyped value (0023), so `cfg["acceleraton"]` fails when it runs, though
  the module's own `options` say what the record holds.
- Annotations are optional, so a module that leaves out `ctx: Context` loses every check.

How others order this: a lazy fixpoint, where configuration refers to itself and evaluation
follows what's asked for, needs nothing declared, at the cost of infinite-recursion errors and
special forms to avoid them. Build systems with dynamic dependencies record what each step read and
run it again when that changes, which is sound for pure code, but errors then depend on which
branches ran and cycles show up as steps that never settle.

Starlark is simple to analyse: no `eval`, no imports beyond `load`, and the syntax tree is
available from the parser the host already uses.

## Decision

### Writes are derived

The host walks each module's syntax tree, and the libraries it loads, when they load, and collects
every call to `set`, `default` or `remove` on a value of type `Context`. What each call writes is
its path argument:

- a literal, `"networking.firewall.allowed_tcp_ports"`, writes that path;
- a literal start and something computed, `"security.pam.services." + service` or
  `"security.pam.services.%s.auth" % service`, writes the literal part up to its last whole
  segment, `security.pam.services`;
- anything else, a path that starts computed, is an error when the module loads, pointing at the
  call: "write the option's path where it's set".

A call in a library is charged to every module that loads it, which only ever orders more than
needed. `getattr(ctx, "set")` and taking a method as a value, `f = ctx.set`, are errors when the
module loads, as the analysis couldn't follow them.

`writes` goes away. Nothing outside grapes used it, so it went at once: every module in grapes
was loaded with its list checked against what was derived, they all agreed, and then the lists
went and `module()` stopped taking one. The check when a `set` runs stays, as an assertion that
the analysis is right.

### Cycles are found when modules load

With every module's writes known, and its reads collected the same way (`ctx.read` with a literal
start), a cycle, where one module reads what another writes and the other reads what the first
writes, is an error when the modules load, naming both calls. Today it's found when evaluation
reaches it, if a test happens to.

### `ctx` is always annotated

A function that takes the context must say so, `ctx: Context`, or the module doesn't load. The
analysis relies on it, and so do the checks of 0023.

### What `ctx.read` returns is typed

The host gives the checker a type for each option a module declares, made from its schema: a record
for `types.record`, a list or a dict for `types.list` and `types.map`, a string for `types.one_of`.
`ctx.read("services.ollama")` with a literal path then has that type, and `cfg["acceleraton"]` is an
error when the module loads. A path the module doesn't declare, or a computed one, stays untyped.

## Consequences

- An author writes `ctx.set(...)` and nothing else; ordering follows from the code, whichever
  branches run.
- Mistakes that showed up when a branch ran, a write not listed, a misspelt option field, a cycle,
  show up when the module loads, even for a module that's off.
- Paths built entirely at run time, such as from a value the user gave, can't be written; nothing
  in grapes does that, and an option holding a map is the way to say it.
- The analysis is a few hundred lines in `crates/starlark`, with tests for each form it accepts
  and refuses.

## Later

- `restart_on` from a unit's text naming files the module renders, as the model already does for
  secrets.
- Packages for the programs a unit's `Exec` lines name, with 0043's closed sets.

## Work

1. The analysis: calls on `Context` values, literal folding for `+` and `%`, library calls, the
   refused forms. Derived writes beside the declared ones, with the warning.
2. Cycles from derived reads and writes, as load errors.
3. `ctx: Context` required.
4. Option types for `ctx.read`.
5. `writes` removed from grapes and the docs, then refused.

## Done

2026-11-23: the analysis, cycles and `ctx: Context` (steps 1 to 3), and `writes` gone from
grapes, the docs and `module()` (step 5).

- A call's path is cut back to its last whole segment, so `"security.pam.services.%s.auth" %
  service` writes `security.pam.services`. Derived writes are sometimes more exact than the lists
  were: syncthing listed `users` and writes `users.syncthing`.
- A parameter named `ctx` without a type is refused. One under another name without a type
  isn't seen, so it's left to review.
- Cycles are reported once per set of modules, with each module's read and the other's write.
  Grapes has none.

2026-11-24: fields of what `ctx.read` returns (step 4).

- It's part of the same walk over the syntax, rather than types given to the checker: a read with
  a literal path is followed through literal keys and `.get("key")`, directly or through a name
  the function binds once. Each field is checked against the whole schema when the configuration
  is evaluated, so reads of other modules' options are checked too.
- A map entry's name and anything inside a freeform option aren't checked, nor is anything once a
  computed key or a loop variable takes over.
