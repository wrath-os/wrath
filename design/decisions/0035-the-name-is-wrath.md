# 0035: The name is wrath

Status: accepted, 2026-11-13; the project was renamed the same day.

## Context

"distro" was a working name. It says what the project is, which makes it hard to search for, to
say in a sentence ("the distro distro"), and to tell apart from every other distribution. The
domains wrath.rs and wrath.build are registered.

## Decision

### The name, and the voice

The system, its command and its package manager are wrath: `wrath plan`, `wrath apply`,
`wrath try`, `wrath rollback`.

The name has an edge; everything wrath says doesn't. The contrast is deliberate:

- Errors are calm and clear. They say what's wrong, point at the line of TOML that caused it,
  and say what to do about it. No blame, no shouting, no jokes.
- Output is plain words. A plan says what changes and when it takes effect, and nothing more.
- It's simple to use and easy to understand: the same few verbs everywhere, and docs that read
  like a colleague explaining, not a manual.
- No wrath puns in commands, output or docs. The name is the only place it shows.

### What's renamed

| Now | Becomes |
|---|---|
| the `distro` command | `wrath` |
| crates `distro-cli`, `distro-build`, `distro-eval`, `distro-model`, `distro-starlark`, `distro-check`, `distro-agent` | `wrath-cli`, `wrath-build`, `wrath-eval`, `wrath-model`, `wrath-starlark`, `wrath-check`, `wrath-agent` |
| `/etc/distro`, `/var/lib/distro`, `/run/distro` and the other `distro-*` paths | the same with `wrath` |
| `distro.toml`, `distro.lock`, `distro.local.toml`, `distro.local.lock` | `wrath.toml`, `wrath.lock`, `wrath.local.toml`, `wrath.local.lock` |
| `DISTRO_*` variables | `WRATH_*` |
| `EFI/distro` on the ESP | `EFI/wrath` |
| units such as `distro-agent.service` and `distro-root.service` | `wrath-agent.service`, `wrath-root.service` |
| the built-in platform and source `distro` (0030) | `wrath` |
| `os-release`'s default name and id | `wrath` |
| the boot document's key `org.distro.boot.v1` (0022, 0024) | `org.wrath.boot.v1`, a new key as versioned keys are |

The store stays `/store`, and generations stay in `/store/.system`: neither has the name in it.

crates.io already has `wrath` and `wrath-macros`, unmaintained but taken. No crate of ours is
called either, now or when one is published: the command's crate is `wrath-cli`, with its binary
named `wrath`, and every other crate is `wrath-<part>`. `cargo install wrath` installs the other
project's crate, so the docs install `wrath-cli`. A crate of procedural macros, if one is needed,
is `wrath-derive`.

### Two names stay until everything rebuilds

`lib/distro/runtime-deps`, which every package with run-time dependencies holds, the
`DISTRO_PATCH_n` marker in the scripts of patched packages, and a comment in glibc's patch keep
their names. Changing any of them changes every store path, so they go with the next change that
rebuilds every package anyway.

### No machine moves

Nothing runs installed yet: the Framework has only booted from a USB stick. There's no migration
and no compatibility with the old names. The Framework gets a new image and is installed from it.

## Alternatives

- Keep "distro". No work, but it stays unsearchable and generic.
- A softer name, to match the voice. The contrast is the point: the tool earns calm by what it
  does, not by what it's called.

## Open

- The repository's own name and directory, and where wrath.rs and wrath.build point: the docs
  and a binary cache are the obvious pair.
