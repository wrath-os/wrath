# Recipes and styles

Every package comes from a recipe in `recipes/` in [grapes](https://github.com/wrath-os/grapes),
the set wrath builds from. A recipe says where the source is, what the build needs and how to
build it. Most of the time a build style writes the build script, so the recipe is mostly a list
of what's different about this project.

## A recipe

```toml
[[package]]
id = "jq"
version = "1.8.1"
scope = "runtime"
core = true
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
  recipe works for any target. `wrath dev lint` points out a target written out.
- `scope` is `runtime` for anything that ends up on a machine, `tool` for things only builds use
  and `boot` for the toolchain itself.
- `core = true` builds with the usual tools around (coreutils, sed, grep, make, bash and so on).
  Almost every recipe wants it.
- `extra_deps` lists what the build needs, by id. That's exactly what the build sees in its
  `/usr`, nothing else.
- `sources` are downloads, or `git+URL#COMMIT` for a git commit. Each is pinned in
  grapes' `sources.lock` the first time it's fetched, so recipes never carry hashes. A second source
  goes somewhere else in the tree with `into = "subdir"`.
- `smoke` lists quick commands that should work once it's built, run from the package's
  directory. `check`, when there is one, runs the project's own tests in its build tree.

A table named after a style, `[package.autotools]` here, picks the style and gives it options.
Without one, the files in the source pick it (a `meson.build` means meson, a `Cargo.toml` cargo,
and so on). A recipe can also skip styles and write `script` itself, as plain shell.

## Styles

There are five so far:

| Style | For | Notable options |
|---|---|---|
| `autotools` | `./configure && make` | `args` |
| `meson` | Meson projects | `args`, checked against the project's own options |
| `cmake` | CMake projects | `args` |
| `cargo` | Rust, from `Cargo.lock` | `packages`, `programs`, `features` |
| `go` | Go, from `go.sum` | `packages`, `ldflags` |

Every style takes `env` and `install` (shell run after installing, for what the project's install
leaves out), and the autotools, meson and cmake styles take `prepare` (shell run before
configuring) too. The cargo and go styles read the project's own lock file, fetch every crate
or module separately and build offline. You never write a vendor hash.

Styles are Starlark too, in grapes' `styles/`. A style is a `build(recipe, options)` function that
returns the script and the dependencies it adds, plus a `style(...)` declaration of its options.
`meson.star` is a good one to read: it's short, and it shows how a style can check a recipe
against the project before anything builds.

## What the script sees

A build runs in a sandbox with no network. Its script gets:

| Variable | Is |
|---|---|
| `$PREFIX` | where the package will live, `/store/<hash>-<name>` |
| `$DEST` | where to install to, so `make install DESTDIR=$DEST` |
| `$NPROC` | how many jobs to run |
| `$LAB_DEP_<id>` | where a dependency is, such as `$LAB_DEP_gettext` |
| `$LAB_ARCH`, `$LAB_OS`, `$LAB_TRIPLE` | the target, as in URLs above |
| `$LAB_LOADER` | the loader the package's programs run with, for a program built elsewhere |

The current directory is the unpacked source.

## The packaging loop

Add the recipe, then build just that package:

```sh
wrath build grim
```

On a terminal you get a line per package that keeps updating with its time and latest output.
`-v` shows everything instead. The full log is always in `build/work/logs/f/<id>.log`.

When a build fails, the error usually comes with a hint. A missing pkg-config module, header or
program gets matched to the recipe that has it, and so does a fallback subproject Meson tried to
fetch or an install that wrote outside the prefix:

```text
error: `grim` Failed after 4 s, see build/work/logs/f/grim.log
  hint: `wayland` has the program `wayland-scanner`: add it to `extra_deps`
```

Once it builds:

```sh
wrath build grim --check  # its smoke commands, and its own tests in a build thrown away
wrath dev lint grim       # dependencies it lists that another one already brings
wrath dev repro grim      # build it again as someone else and compare
wrath dev outdated        # every recipe against upstream's newest release
```

When packaging goes wrong in a way the tool could have caught, it goes in grapes'
[packaging findings](https://github.com/wrath-os/grapes/blob/main/docs/packaging-findings.md):
what happened, what we did, what would prevent it. Most of the hints above started out there.

## What's not there yet

- Python, Node, Ruby and the other language ecosystems don't have styles yet.
- `wrath info <id>` for a project's build options, so you don't have to read `meson.options` by
  hand.
