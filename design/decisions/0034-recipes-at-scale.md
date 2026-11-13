# 0034: A file per package, a lock beside it, and an index

Status: proposed, 2026-11-13.

## Context

There are 370 recipes in 34 files under `build/recipes`, grouped by theme: `desktop.toml` has 78,
`full.toml` 48. Patches and other files sit loose beside them. One `sources.lock`, 824 KB and
5,833 lines, pins every source of every recipe. Reading and linting everything takes 0.22 s.

The aim is nixpkgs' range, tens of thousands of packages. At that size:

- Reading isn't the cost. 30,000 recipes are about 15 MB of TOML, parsed in well under a second.
  Evaluating every recipe's style in Starlark to build one package would be: nixpkgs evaluates
  lazily and still takes minutes and gigabytes to go through all of it.
- Changes are. nixpkgs moved packages from `all-packages.nix` to
  `pkgs/by-name/<xx>/<name>/package.nix` (RFC 140) because that file conflicted on nearly every
  change, and an update bot's work merges best as one small diff per package. Our theme files
  would conflict the same way once anything updates recipes in bulk, and every update already
  touches `sources.lock`.
- Search needs more than reading files: which package has a program, what a package is, which
  version. There's no `search` yet.

Theme files have one thing worth keeping: related packages read together, such as gtk4 and its
typelib part, with one comment about the family.

## Decision

### A file per package, or per family

```text
build/recipes/
  gh/ghostty.toml
  gh/ghostty.lock
  gt/gtk4.toml           gtk4 and gtk4-gir
  gt/gtk4.lock
  gt/gtk4/fix-x.patch    a recipe's own files, beside it
```

- A file holds one package, or a family that changes together: a library and its typelib part,
  a compiler and its runtime. It's named after its main package, under the first two letters of
  that name, so no directory grows past a few thousand entries.
- A recipe's own files (patches, small sources, its Cargo.lock) go in a directory with the
  file's name.
- `distro dev lint` checks every id is declared once, and every file's name is one of its ids.
  Which family a package belongs to is the author's call.

### A lock beside each file

A recipe file's sources are pinned in a lock with its name next to it. Updating ghostty changes
`gh/ghostty.toml` and `gh/ghostty.lock` and nothing else, so updates in bulk merge without
conflicts, and removing a package removes its pins. The format stays the one `sources.lock` has.

### The tool files recipes

`distro recipe new NAME` makes the file where it goes, and `distro recipe edit ID` opens the file
an id is in, family or not. Nobody has to know the layout to change a package.

### An index for finding, never all of it for building

- Building evaluates the recipes a build needs, the asked-for package and what it depends on,
  found through the index. Nothing evaluates every recipe except the lint and the index itself.
- A style's result is cached by the hash of the recipe's text, its style and the target (0032),
  so an unchanged recipe isn't evaluated twice.
- The index is a SQLite file in `build/work`, updated per recipe file by hash: each package's id,
  version, description, file, dependencies, and the programs and libraries it provides once it
  has been built or fetched. `distro search` reads only that, and so can a command-not-found
  hook ("`rg` is in ripgrep").

### When

Not urgent at 370 recipes, and the split is mechanical: a tool moves each package, or family, to
its file with its pins. It goes before anything updates recipes in bulk, or at about 1,000
recipes, whichever comes first. The single lock is what starts hurting first.

## Consequences

- A source that brings recipes (0030) can replace one package by shipping its file; ids staying
  one namespace means that's an error for now, as it is today.
- Moving a package between families is a rename in version control.
- Reviews and ownership can be per directory, as nixpkgs does.

## Alternatives

- Keep theme files. Fine to read at this size, but they and the single lock conflict under bulk
  updates, and finding a package's file needs a search.
- Strictly one package per file, as nixpkgs. The lookup is simpler, with no index needed, but
  families that change together get split and lose their shared comment.
- A database instead of files. Fast to query, but changes stop being reviewable diffs, and the
  index gives the speed without it.

## Open

- Whether the index is committed, so a fresh checkout searches without reading recipes first, or
  only built locally. Local is simpler, and building it takes about as long as the lint.
- The shard: two letters, as nixpkgs, or by the hash of the name for an even spread.
