# 0020: Files made from what packages share

Status: accepted, 2026-11-11.

## Context

A generation's `share` merges every package's data (0016). Some programs don't read that data
directly; they read one file made from all of it. GSettings reads `gschemas.compiled`, made from
every package's schemas, and a GTK program aborts without it. The MIME database, icon caches,
GIO's module cache, gdk-pixbuf's loader cache and fontconfig's cache are the same kind of file.

Each package can't make it alone, because the file covers all of them, and two packages shipping
their own collide in the merge. One way is to compile schemas per package and wrap each program
to find its package's own, making the MIME database and icon caches when the system profile is
built. Another is to run the tools from package manager hooks as packages come and go, which
leaves the result in mutable state.

## Decision

These files are made when the generation is built, by the tool of the package that reads them.

A recipe declares what it makes from the merged tree:

```toml
[[package]]
id = "glib"
indexes = [
  { from = "share/glib-2.0/schemas", run = "bin/glib-compile-schemas share/glib-2.0/schemas" },
]
```

When any of a generation's packages has files under `from`, the generation gets one more action:
the package's tool runs in the sandbox, over a copy of the merged `from` tree, with the tree's
root as its working directory, and the files it adds become part of the generation. The tool's
package doesn't need to be part of the system; it's the one the recipes build.

The action is keyed like the others: by the tool's package and the files it reads, so a change to
any schema makes a new file and nothing else reruns. Homes get the same once they merge `share`.

## Consequences

- A generation is complete as built. Rollback is a rename, as before, with nothing to recompile.
- A broken schema or a conflicting override fails the build, before anything is applied.
- Building a generation runs programs, where it only made links and files before. Each is small and
  only reruns when what it reads changes.
- Packages stop shipping these files themselves; a recipe that installs one removes it.
- The MIME database, icon caches and the rest follow as their packages' recipes declare them.
- Only the packages a system names share their data, so a library whose data a program reads
  is named beside it: GTK's schemas, and the desktop schemas the GTK portal reads without
  referring to them. Following named packages' run-time closures would cover the first; that
  needs closures recorded when packages are built, since scanning for them on every plan is too
  slow.

## Alternatives

- At activation, as package manager hooks do. Generation builds stay link-only, but the file lives
  outside the generation, rollback has to remake it, errors show up at switch or boot, and
  activation gains state.
- Per package, with wrappers. Nothing to merge, but every program needs wrapping and an unwrapped
  one fails at run time.
- In a module instead of a recipe. Modules decide what a system has; how a package's data is
  indexed belongs to the package, and holds wherever it's used.
