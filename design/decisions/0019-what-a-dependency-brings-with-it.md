# 0019: What a dependency brings with it

Status: accepted, 2026-11-11.

## Context

A build sees only what its recipe lists (0014). That keeps builds honest, but a library's
`.pc` file often names other libraries: GTK's names Pango, Cairo and GLib, and `pkg-config
--cflags gtk+-3.0` fails unless all of them are in the view. So every recipe that uses GTK lists
a dozen packages it never asked for by name, and forgets one now and then. The desktop recipes
show it: most of their failures were a missing transitive requirement.

Nix asks packagers to list these by hand as propagated inputs. They get forgotten, which breaks
builds, and they get overlisted, which spreads to every dependent. Fedora reads the `.pc` files
and writes the dependencies itself. Distributions with one shared `/usr` mostly don't notice,
because everything is installed.

We want the tool to carry this, without giving up what makes a build correct: the same inputs
see the same view, and the key says what the view is.

## Decision

A dependency brings what its `.pc` files require, by default, and recipes can narrow or extend
that on either side.

**What a package requires.** For each `.pc` file a package installs, its `Requires` and
`Requires.private` name modules. Each is resolved against what that package was itself built
with: the dependency of its own whose outputs have the module's `.pc` file. A module the package
installs itself needs nothing. A module none of its dependencies has is an error, which names the
package and its `.pc` file, so a broken `.pc` file shows up where it was made. `Requires.private`
counts as well, because `pkg-config --cflags` reads the headers it names.

**What a dependent sees.** A recipe's view is what it lists, plus what each listed package
requires, followed transitively. A requirement reached through any `Requires.private` is private.
What's added is the dev and library outputs of those packages, the same as a listed dependency.
Run-time closures don't change: they still come from what the built files refer to.

**Choosing.** A dependent decides per dependency:

```toml
extra_deps = [
  "pkgconf",
  { id = "gtk3", propagate = "public" },     # only what Requires names
  { id = "libfoo", propagate = "none" },     # nothing, the recipe lists it
  { id = "glib", exclude = ["util-linux"] }, # all but this
]
```

`propagate` is `"all"` (the default), `"public"` or `"none"`. A package adds what its `.pc` files
don't say, or leaves out its private requirements:

```toml
propagates = ["libbar"]   # its headers include libbar's, and no .pc file names it
propagate_private = false # its private requirements only matter for static linking
```

**Determinism.** What a package requires follows from its outputs and its dependencies, both
fixed by its key. Its own choices above are part of its key, and a dependent's are part of the
dependent's, so a dependent's unresolved key already decides its view. The resolved key lists
every package in the view, added or not.

**Conflicts.** Two packages in one view with the same file are refused, as before, so a variant
listed beside the package it varies stops the build and the recipe chooses.

## Consequences

- Recipes list what they use. The transitive requirements already written by hand can go.
- A wider view lets a configure script find optional libraries it wasn't asked to use. What's
  added is bounded by what `.pc` files declare, and a recipe that wants control says `"none"`.
- The first build of a dependent reads its dependencies' `.pc` files. They're small.
- CMake's `find_dependency` and other languages' equivalents can feed the same mechanism later.

## Alternatives

- Listing by hand, as now. Correct but tedious, and the failures are late and confusing.
- Recording what a package requires as a file beside its outputs. It would have to travel with
  cached outputs; reading the `.pc` files that already travel gives the same answer.
- The whole closure of every dependency in the view. Simple, and too wide: every library a
  dependency links against at run time would be found by configure scripts.
