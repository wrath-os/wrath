# Packages

Every package wrath installs is built from source, from a recipe in
[grapes](https://github.com/wrath-os/grapes), the set of recipes and modules wrath systems are
made of. Your lock pins grapes as a source of its own, named `grapes`, so the version it pins
decides every package's version. Around 380 recipes build today, which covers a desktop,
development tools and the usual command line things.

A configuration doesn't need to declare grapes. Its lock pins it the first time a command reads
it, and `wrath update grapes` moves it forward. To build from a checkout of grapes for one
command, give it with `--source grapes=DIR`.

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
jq from grapes 1d06008, 2026-10-30
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

Packages move when grapes does:

```console
$ wrath update grapes
$ wrath plan
packages
  ~ ripgrep (tier: live)
      rebuilt
```

The plan says a package was rebuilt, not yet which versions it went between. That's coming.

`update` moves every source to its newest version, `wrath update grapes` just that one, and
`wrath update --list` shows each source with where it's pinned and how old that is. Nothing
changes on the machine until you apply. A newer grapes can also rebuild packages your
configuration never mentions (a kernel with other options, say), and the plan shows those as
"rebuilt", with the tier they need.

Every system carries the grapes it was built from, at `/run/system/grapes`. When the lock pins
that same version, commands use that copy instead of fetching grapes, which works offline. Any
other version is fetched, never replaced by the copy.

## Your own build of a package

Sometimes you want a package built a bit differently: a patch, or an option the recipe offers.
That's a build, and it gets its own name:

```toml
packages = ["tiny-nano", "patched-hello"]

[builds.tiny-nano]
package = "nano"
options.enable_tiny = true

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

- Versions: `jq@1.7` for an older jq from an older version of grapes is planned, but not built.
- `wrath search` and `wrath info`, for finding packages and seeing what's in one.
- Recipes from sources other than grapes. `acme#jq` is how it'll be written.
- A binary cache, so everything builds on your machine.
