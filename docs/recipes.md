# Recipes and styles

Every package comes from a recipe in `recipes/` in [grapes](https://github.com/wrath-os/grapes),
the set wrath builds from. A recipe says where the source is, what the build needs and how to
build it. Most of the time a build style writes the build script, so the recipe is mostly a list
of what's different about this project.

Each package has a file of its own under the first two letters of its name, such as
`recipes/ri/ripgrep.toml`. A family that changes together, such as gobject-introspection and its
typelibs, shares the main package's file, `recipes/go/gobject-introspection.toml`. Beside each file
is its lock, `ripgrep.lock`, and a directory with its name, `recipes/ri/ripgrep/`, holds its own
files, such as patches, which `files` names from there. `wrath dev lint` says when a file isn't
where it goes.

## A recipe

```toml
[[package]]
id = "jq"
version = "1.8.1"
scope = "runtime"
sources = [{ url = "https://github.com/jqlang/jq/releases/download/jq-{version}/jq-{version}.tar.gz" }]
smoke = ["bin/jq --version", "echo '{\"a\":1}' | bin/jq -e .a"]

# The release carries oniguruma, the regular expression library, and builds it in.
[package.autotools]
args = ["--with-oniguruma=builtin", "--disable-docs", "--disable-static"]
```

What the fields mean:

- `id` is the package's name, what people write in `packages`. `version` goes into its store path
  and into URLs as `{version}`, and as `{series}` (`2.84` of `2.84.4`, as GNOME names its
  directories) and `{underscored}` (`3_133`, as some projects tag releases).
- A URL names the machine it's built for as `{arch}` (`x86_64`), `{os}` (`linux`), `{target}`
  (`x86_64-linux`) or `{triple}` (`x86_64-unknown-linux-gnu`), never by writing it out, so the
  recipe works for any target. `wrath dev lint` points out a target written out. A project that
  spells targets its own way, as Go's `amd64`, says how in `arch_names`, and the URL uses
  `{arch_name}`: `arch_names = { x86_64 = "amd64", aarch64 = "arm64" }`.
- `scope` is `runtime` for anything that ends up on a machine, `tool` for things only builds use
  and `boot` for the toolchain itself.
- A build has the compilers, the C library and the usual tools (coreutils, sed, grep, make, bash
  and so on). `toolchain = "compilers"` leaves the usual tools out, for the packages they're
  built from, and `toolchain = "seed"` leaves out everything but the prebuilt compilers the
  toolchain itself starts from.
- `deps` lists what else the build needs, by id. That and the toolchain are exactly what the build
  sees in its `/usr`, nothing else.
- `sources` are downloads, or `git+URL#COMMIT` for a git commit. Each is pinned in the
  recipe's lock the first time it's fetched, so recipes never carry hashes. A second source
  goes somewhere else in the tree with `into = "subdir"`.
- `smoke` lists quick commands that should work once it's built, run from the package's
  directory. `check`, when there is one, is shell that runs the project's own tests in its build
  tree, such as `check = "make -j$NPROC check"`.

The rest are for when a package needs them:

| Field | What it's for |
|---|---|
| `script` | The build, as plain shell, when no style fits; it installs into `$DEST$PREFIX` |
| `files` | The recipe's own files, such as `["fix-crash.patch"]`, copied into the source tree |
| `sources` `into`, `file` | A second source unpacked into a subdirectory, or put there as it is |
| `runtime_deps` | Packages it needs when it runs but doesn't refer to by path, such as data it reads |
| `whole` | One output instead of a split, for a program that finds files beside its libraries |
| `options` | What a build of it can set, read by its script as `$OPTION_<name>` (see [packages](packages.md)) |
| `track`, `hold` | The release series `wrath dev outdated` keeps to, and a version it stays below and why |
| `silent_for` | How long the build may print nothing before it's stopped, such as `"3h"` |

A table named after a style, `[package.autotools]` here, picks the style and gives it options.
Without one, the files in the source pick it (a `meson.build` means meson, a `Cargo.toml` cargo,
and so on). A recipe can also skip styles and write `script` itself, as plain shell.

A program that finds other packages' files through a variable says so in its recipe, and every
build that has it among its dependencies gets the variable, listing those directories of all its
dependencies:

```toml
# python3's recipe
[[package.search_paths]]
variable = "PYTHONPATH"
files = ["site"]
```

So a build with python3 and jinja2 among its dependencies finds jinja2's modules without saying
where they are. `separator` sets what goes between the directories, `:` when it's left out.

## What the lock holds

Beside each recipe file is a lock, which wrath writes and nobody edits. For each package it pins
every source by its hash and records what the package's style made:

```toml
[jq]
style = "autotools"
record = "84d739dc..."   # the script, and the sources and dependencies the style added
from = "bddf18a9..."     # the style's code and the recipe it read
deps = []                # what the style added to the recipe's own `deps`, such as `rust-bin`

[[jq.sources]]
url = "https://github.com/jqlang/jq/releases/download/jq-1.8.1/jq-1.8.1.tar.gz"
hash = "2be64e7129cecb11d5906290eba10af694fb9e3e7f9fc208a311dc33ca837eb0"
```

With it, a plan knows what every package is without downloading a source: a style reads the
project's own files, such as its `Cargo.lock`, only when the package is built, and the build
stops if the style makes something else from the same input. In a checkout of grapes, wrath
records a new source or a changed style the first time it builds, and says so. A plan only
reads locks, downloads nothing, and says when one is behind. `wrath dev lint` brings every
lock up to date. `--locked`, on any command, writes none and fails instead, which is what a CI
wants: `wrath dev lint --locked` fails on a lock someone forgot to update. With a pinned grapes,
as a machine has, a lock that's behind is an error: nothing is written into a version someone
pinned. That version of
grapes was committed without running `wrath dev lint`; a newer one, or a checkout given with
`--source grapes=DIR`, gets past it.

A style that reads a lock of the project's own, such as cargo's `Cargo.lock`, takes each crate's
hash from there, so the recipe's lock pins only the project itself. A style that reads a list
without hashes, such as Go's module list, gets every module pinned in the recipe's lock instead.

## Styles

There are seven so far:

| Style | For | Notable options |
|---|---|---|
| `autotools` | `./configure && make` | `args`, `cflags` |
| `meson` | Meson projects | `args`, checked against the project's own options |
| `cmake` | CMake projects | `args`, `cflags` |
| `cargo` | Rust, from `Cargo.lock` | `packages`, `programs`, `features` |
| `go` | Go, from `go.sum` | `packages`, `ldflags` |
| `zig` | Zig, from `build.zig.zon.json` | `args`, `deps` |
| `prebuilt` | a project's own build for other distributions, such as a browser | `programs`, `remove` |

Every style takes `env` and `install` (shell run after installing, for what the project's install
leaves out), and the autotools, meson, cmake and zig styles take `prepare` (shell run before
configuring) too. The cargo, go and zig styles read the project's own list of dependencies,
fetch each crate, module or package separately and build offline. You never write a vendor hash.
`prebuilt` copies a build made elsewhere into the package and points its programs at wrath's
loader and libraries; it's never picked from the files, so a recipe names it.

Styles are Starlark too, in grapes' `styles/`. A style is a `build(recipe, options)` function that
returns the script and the dependencies it adds, plus a `style(...)` declaration of its options.
`meson.star` is a good one to read: it's short, and it shows how a style can check a recipe
against the project before anything builds.

## What the script sees

A build runs in a sandbox with no network. Its script gets:

| Variable | Is |
|---|---|
| `$PREFIX` | where the package will live, `/wrath/store/<hash>-<name>` |
| `$DEST` | where to install to, so `make install DESTDIR=$DEST` |
| `$OUT` | the package's own directory as the build sees it |
| `$NPROC` | how many jobs to run |
| `$WRATH_DEP_<id>` | where a dependency is, such as `$WRATH_DEP_gettext` (`-` becomes `_`) |
| `$WRATH_LIB_<id>` | where a dependency's libraries are: its own directory, or the output they're split into |
| `$WRATH_SEED` | where the prebuilt compilers everything starts from are, for the toolchain's recipes |
| `$WRATH_ARCH`, `$WRATH_OS`, `$WRATH_TRIPLE` | the target, as in URLs above |
| `$WRATH_ARCH_NAME` | the target as the project names it, from `arch_names` |
| `$WRATH_LOADER` | the loader the package's programs run with, for a program built elsewhere |
| `$WRATH_LOADER_NAME` | the loader's file name on this target, `ld-linux-x86-64.so.2` |

The current directory is the unpacked source.

## The packaging loop

Add the recipe, then build just that package:

```sh
wrath build grim
```

On a terminal you get a line per package that keeps updating with its time and latest output.
`-v` shows everything instead. The full log is always in `build/work/logs/f/<id>.log` in the
wrath checkout building it.

When a build fails, the error usually comes with a hint. A missing pkg-config module, header or
program gets matched to the recipe that has it, and so does a fallback subproject Meson tried to
fetch or an install that wrote outside the prefix:

```text
error: `grim` Failed after 4 s, see build/work/logs/f/grim.log
  hint: `wayland` has the program `wayland-scanner`: add it to `deps`
```

Once it builds:

```sh
wrath build grim --check        # its smoke commands, and its own tests in a build thrown away
wrath dev lint                  # locks up to date, files where they go, no target written out
wrath dev lint --redundant grim # dependencies it lists that another one also brings
wrath dev repro grim            # build it again as someone else and compare
wrath dev outdated              # every recipe against upstream's newest release
```

To use it on your machine before it's in grapes, point your configuration at your checkout of
grapes for the apply, and add it as any package:

```sh
wrath add grim
wrath apply --source grapes=~/src/grapes
```

When packaging goes wrong in a way the tool could have caught, it goes in grapes'
[packaging findings](https://github.com/wrath-os/grapes/blob/main/docs/packaging-findings.md):
what happened, what we did, what would prevent it. Most of the hints above started out there.

## What's not there yet

- Python, Node, Ruby and the other language ecosystems don't have styles yet.
- `wrath info <id>` for a project's build options, so you don't have to read `meson.options` by
  hand.
