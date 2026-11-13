# Packages

Every package wrath installs is built from a recipe in wrath's own repository, from source.
The wrath revision your lock pins decides which recipes you get, so it decides every package's
version too. Around 350 build today, which covers a desktop, development tools and the usual
command line things.

## Installing one

Add it to `packages` and apply:

```toml
packages = ["ripgrep", "jq", "helix"]
```

Or let wrath write it for you:

```console
$ wrath add jq
$ wrath apply
```

`add` writes the name into the configuration's starting file, exactly as you typed it, and pins
anything new. A name no recipe has is refused before anything's written, with the closest one
there is, and `check` and `plan` catch one written by hand the same way.

`wrath add --home john lazygit` adds it to john's packages instead, which only end up on their
`PATH`. `wrath remove jq` takes it out again.

## Trying one without installing it

```console
$ wrath run jq -- --version
jq from wrath 1d06008, 2026-10-30
jq-1.8.1
$ wrath shell jq ripgrep
```

`run` runs a package's program and `shell` starts a shell with the packages on its `PATH`.
Neither changes anything: nothing gets written to the configuration and nothing gets installed.
They build the package if it isn't built yet, so the first time can take a while.

Each one says on stderr where the package came from, so you always know which jq you got. It's
the one your configuration would give you, never whatever happens to be newest today.

## Building one

```sh
wrath build jq ripgrep        # prints where each one is
wrath build jq -o ./jq        # and links it there
```

That's mostly useful while working on recipes. There's no `./result` link by default. If you
want a link, `-o` says where.

## Updating

Packages move when the wrath revision does:

```console
$ wrath update --list
wrath  github:johnae/wrath  1d06008, 2026-10-30, 13 days old
$ wrath update
$ wrath plan
packages
  ~ ripgrep (tier: live)
      rebuilt
```

The plan says a package was rebuilt, not yet which versions it went between. That's coming.

`update` moves every source to its newest version, `wrath update wrath` just that one. Nothing
changes on the machine until you apply. A newer wrath can also rebuild packages your
configuration never mentions (a kernel with other options, say), and the plan shows those as
"rebuilt", with the tier they need.

## Your own build of a package

Sometimes you want a package built a bit differently: a patch, or an option the recipe offers.
That's a build, and it gets its own name:

```toml
packages = ["tiny-nano", "patched-hello"]

[builds.tiny-nano]
package = "nano"
options.enableTiny = true

[builds.patched-hello]
package = "gnu-hello"
patches = ["patches/hello.patch"]
```

Only that package changes. Everything else keeps using the regular nano, and the patch file
lives next to your configuration.

## Programs from elsewhere

Programs downloaded for other distributions expect their libraries in `/lib64` and the like,
which wrath doesn't have. Turn this on and they find a set of common libraries there:

```toml
[programs.foreign]
enable = true
```

## What's not there yet

- Versions: `jq@1.7` for an older jq from an older wrath revision is planned, but not built.
- `wrath search` and `wrath info`, for finding packages and seeing what's in one.
- Recipes from sources other than wrath. `acme#jq` is how it'll be written.
- A binary cache, so everything builds on your machine.
