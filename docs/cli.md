# The command line (draft)

A draft of the command set, so new commands fit one scheme. Nothing here is final; "What's
built" near the end says how far along it is.

## Principles

- One tool, one verb per job. The same verb works for the system, a user and a project; which
  one it acts on comes from where you are, and the first line of output says which.
- Plain words: package, source, lock, plan, generation. Backend words like derivation, flake,
  attribute path and closure only show up in `--verbose` output and `why`.
- Anything that changes a machine shows the plan first and says when the change takes effect:
  now, at the next login, after a soft reboot, or after a reboot.
- Trying something never changes anything. Keeping it writes it into your TOML, so nothing a
  command does lives outside the configuration.
- Every command has `--json`, and the CLI is a thin layer over the same calls a GUI or a remote
  API would make.

## Where a command acts

A command acts on one configuration, its context, found in this order:

1. `-C DIR`, a configuration directory.
2. The nearest directory upwards with a `distro.toml`, a project.
3. `--user`, your own configuration in `~/.config/distro`.
4. The system's configuration in `/etc/distro`.

```console
$ distro plan
system /etc/distro (neptune)
packages
  ~ ripgrep (tier: live)
      14.1.0 -> 15.2.0
1 change, tier: live
```

Later, `--host NAME` builds locally and applies on another machine over SSH or its agent.

## Naming a package

Everywhere a package is named, on the command line and in TOML, it's written the same way:

```text
[source#]name[@version]
```

| You write | You get |
|---|---|
| `jq` | jq as your configuration's lock pins it |
| `jq@1.7` | the newest jq 1.7.x that any nixpkgs had |
| `stable#jq` | jq from the source you named `stable` |
| `github:NixOS/nixpkgs/nixos-25.05#jq` | jq from that nixpkgs branch, without declaring it |
| `github:jqlang/jq#jq` | jq from the project's own flake |
| `./tools#mytool` | a package defined in a local directory |

### Which jq is `jq`?

The one your configuration would give you. If the context's lock has jq, `distro run jq` runs
that build. If it doesn't, jq comes from the nixpkgs the context is already pinned to, so it
shares the libraries already on the machine and needs nothing new evaluated or downloaded beyond
jq itself. It never quietly means "whatever unstable is today", which is what
`nix run nixpkgs#jq` does through the registry.

Every command that picks a package says what it picked, on stderr:

```console
$ distro run jq -- --version
jq 1.8.1 from nixpkgs, locked 2026-10-30 (1d06008)
jq-1.8.1
```

### Versions

nixpkgs mostly carries one version of each package, so `jq@1.7` needs to know where 1.7 lived.
The factory publishes a version index: every version each package had across nixpkgs history,
with the newest nixpkgs revision that has it. `@1.7` matches 1.7, 1.7.1 and so on, and takes the
newest. Packages that nixpkgs carries in several versions at once (`python312`, `nodejs_20`)
are in the index too, so `python@3.12` works without knowing the attribute name.

```console
$ distro info jq
jq  command-line JSON processor
  1.8.1  nixpkgs (locked)                     installed for john
  1.8.1  stable  nixos-25.11
  1.7.1  nixpkgs 2025-04-12 (a1b2c3d)         ~42 MB more to download
  1.6    nixpkgs 2023-11-02 (9f8e7d6)         ~61 MB more to download
```

An older version comes from an older nixpkgs, with its own copies of the libraries, and
`info` shows that cost up front.

### Trying, then keeping

```console
$ distro run jq@1.7 -- --version      # runs it, changes nothing
$ distro shell jq@1.7 ripgrep         # a shell with both, changes nothing
$ distro add jq@1.7                   # writes it into your TOML and the lock
```

`add` writes what you typed, `packages = ["jq@1.7"]`, and the lock records the nixpkgs revision
it came from. `add` with a URL declares the source for you, named after the last part of the URL:

```console
$ distro add --home john github:jqlang/jq#jq
declared source jq as github:jqlang/jq
added jq#jq to users.john.packages in users/john.toml
```

## Commands

The machine:

| Command | Does |
|---|---|
| `distro status` | What's running, whether it matches the configuration, and anything waiting for a login or reboot |
| `distro plan` | What would change |
| `distro apply` | Shows the plan, asks, switches, and says when the change takes full effect. `-y` skips asking. Also takes a system built elsewhere: `distro apply ./result` |
| `distro try` | Like `apply`, but rolls back after five minutes unless you run `distro keep` |
| `distro history` | The generations, each with what changed in it |
| `distro rollback [N]` | Back one generation, or to generation N |
| `distro diff [A] [B]` | The plan between two generations, or a generation and the configuration |
| `distro build` | Builds the system without switching, for another machine or a VM |
| `distro clean` | Removes old generations and what only they used, after showing how much it frees |

Packages:

| Command | Does |
|---|---|
| `distro run PKG [-- ARGS]` | Runs a package without installing it |
| `distro shell [PKG...]` | A shell with those packages; with none in a project, the project's environment |
| `distro add PKG...` | Adds packages to the configuration and locks them; `--home USER` for a user's own |
| `distro remove PKG...` | Removes them |
| `distro search TEXT` | Finds packages by name and description |
| `distro info PKG` | Versions, where each comes from, what's installed and what a change would download |

The lock:

| Command | Does |
|---|---|
| `distro lock` | Locks what isn't locked yet, moving nothing |
| `distro update [SOURCE or PKG...]` | Moves sources, or single packages, to their newest versions |
| `distro sources` | Each source with what it follows, where it's pinned and how old that is |

Understanding the configuration:

| Command | Does |
|---|---|
| `distro check` | Evaluates the configuration and reports errors, without building |
| `distro why THING` | Where an option's value, a file such as `/etc/nginx/main.conf`, or a package comes from |
| `distro options [PREFIX]` | The options there are, with their types, defaults and docs |

Secrets: `distro secret edit NAME`, `distro secret list`, `distro secret rekey`.

Setting up a machine: `distro install`, covering disks, file systems, encryption and the
machine's key, gets its own design.

## Output and the API

- Human output by default. `--json` gives the same thing as data, with a schema per command.
- Exit codes: 0 for success, 1 for errors. `plan --exit-code` returns 2 when there are changes.
- `apply` asks before switching unless `-y`. Without a terminal, it refuses rather than
  guessing.
- Each command is a call on a library, and later on a local daemon. The CLI, a GUI installer,
  editor plugins and a Talos-style remote API all use the same calls.

## For contributors

The commands that only make sense in this repository move under `distro dev`: `dev test`
(running scenarios, today's `check`), `dev status`, `dev bench`. `fill-templates` stays hidden;
activation runs it, and people don't.

## What's built

Built: `-C` and the system default, `status`, `plan`, `apply` (asking first), `try`, `keep`,
`history`, `diff`, `rollback`, `clean`, `build`, `apply PATH`, `run`, `shell`, `add`, `remove`,
`lock`, `update`, `sources`, `check`, `why` for options, files and packages, `options`,
`secret`, the `dev` commands, `--json` for `plan`, `status`, `history` and `sources`, and
`--role` for configurations with `roles/`, which defaults to the machine's role from install or
`machines.toml`.

Not yet: finding a project's `distro.toml`, `--user`, versions (`jq@1.7`), `search`, `info`,
`--json` elsewhere, `--host`, and `--verbose`. `clean` lists the generations it removes rather than
estimating the space first, and says what it freed afterwards.

## For people coming from Nix

| Nix | distro |
|---|---|
| `nix-shell -p jq`, `nix shell nixpkgs#jq` | `distro shell jq` |
| `nix run nixpkgs#jq` | `distro run jq` |
| `nix profile install`, `nix-env -i` | `distro add jq` |
| `nix flake update`, `nix flake lock --update-input x` | `distro update`, `distro update x` |
| `nixos-rebuild switch`, `home-manager switch` | `distro apply` |
| `nixos-rebuild dry-activate` | `distro plan` |
| `nix search nixpkgs jq` | `distro search jq` |
| `nix-collect-garbage -d` | `distro clean` |
| `nix why-depends` | `distro why` |

## Open questions

- Where the version index comes from: built by our factory from nixpkgs history and Hydra, or
  reusing an existing index.
- Whether `distro.toml` is the project file, and what a project can set beyond packages and
  environment variables.
- Whether `update PKG` moving one package without its source is worth what it costs: that
  package then comes from a different nixpkgs than the rest.
- How long `try` waits by default, and whether it rolls back when the machine loses the network.
