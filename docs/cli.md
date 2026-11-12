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
3. The system's configuration in `/etc/distro`.

```console
$ distro plan
system /etc/distro (neptune)
packages
  ~ ripgrep (tier: live)
      14.1.0 -> 15.2.0
1 change, tier: live
```

Another machine is managed from where you have a checkout: `plan --host NAME`, `apply --host NAME`
and `try --host NAME` build here, copy what the machine lacks over SSH, and switch it with the new
system's own distro. `NAME` is what `ssh NAME` reaches, so `~/.ssh/config` applies, and
`DISTRO_SSH` replaces the `ssh` command, such as `DISTRO_SSH="ssh -p 2222"`. The login needs
root, or sudo, which asks for its password once.

```console
$ distro apply --host root@framework
```

## Naming a package

Everywhere a package is named, on the command line and in TOML, it's written the same way:

```text
[source#]name[@version]
```

| You write | You get |
|---|---|
| `jq` | jq from the recipes of the distro your lock pins |
| `jq@1.7` | jq 1.7 from the newest distro revision that had it (planned) |
| `acme#jq` | jq from the recipes in the source you named `acme` (planned) |

### Which jq is `jq`?

The one your configuration would give you. Recipes live in distro's repository, so the distro
revision your lock pins decides every package's version and source. `distro run jq` builds that
recipe, or finds it in the store already, and nothing else changes. It never quietly means
"whatever is newest today".

Every command that picks a package says where it came from, on stderr:

```console
$ distro run age -- --version
age from distro 1d06008, 2026-10-30
v1.2.1
```

### Versions (planned)

`jq@1.7` means the recipe from the newest distro revision that had jq 1.7, built from that
revision. The store is content-addressed, so whatever comes out the same as what you have is
shared, and an older version costs only what differs.

### Trying, then keeping

```console
$ distro run jq -- --version      # runs it, changes nothing
$ distro shell jq ripgrep         # a shell with both, changes nothing
$ distro add jq                   # writes it into your TOML
```

`add` writes what you typed, `packages = ["jq"]`.

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
| `distro up` | Runs a project's services, in dependency order, until you stop them |
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

The lock pins distro too, as the source `distro`: its modules, its evaluator and its recipes.
`distro update` pins the distro you're running, and `[sources.distro]` says where distro is
fetched from. A distro other than the one the lock pins says so.

A newer distro can rebuild packages the configuration never mentions, such as a kernel with
other options. Each system records which packages it was built from, and `plan` compares that
with today's recipes, so a rebuilt package shows with the tier it takes:

```console
$ distro plan --host framework
packages
  ~ linux (tier: reboot)
      rebuilt
  + bubblewrap (tier: live)
2 changes, tier: reboot
```

Understanding the configuration:

| Command | Does |
|---|---|
| `distro check` | Evaluates the configuration and reports errors, without building |
| `distro why THING` | Where an option's value, a file such as `/etc/nginx/main.conf`, or a package comes from |
| `distro options [PREFIX]` | The options there are, with their types, defaults and docs |

Secrets: `distro secret edit NAME`, `distro secret list`, `distro secret rekey`.

Setting up a machine: `distro install -C DIR [--disk NAME=/dev/...] [--role NAME] [--name NAME]`,
from distro's image or a checkout with its builder, lays out the disks the configuration describes with
systemd-repart, encrypts them with a passphrase it asks for, builds the system onto them and
records the machine's role and name. It refuses disks that have partitions unless given `--wipe`.
The machine's SSH host key and initrd key, kept encrypted in `secrets/machines/NAME/`, are
decrypted into it with `--identity FILE`, which stays where install runs. Without `--disk`, it finds
the disks the configuration describes by kind and size, refusing to guess when more than enough
fit, and records them on the machine.
With `[unlock]`, install enrolls the TPM, and the initrd can be unlocked over SSH.

## Output and the API

- Human output by default. `--json` gives the same thing as data, with a schema per command.
- Builds show a line each as they start. On a terminal that line stays up to date with the
  build's time and latest output, and ends as `built NAME 42s`. `-v` shows everything each build
  prints. Each build's full output is kept in `build/work/logs/f/NAME.log` either way.
- Exit codes: 0 for success, 1 for errors. `plan --exit-code` returns 2 when there are changes.
- `apply` asks before switching unless `-y`. Without a terminal, it refuses rather than
  guessing.
- `distro options --schema` prints the options as a JSON Schema, so an editor completes, checks
  and explains the TOML as you write it. `--schema users.john` is the one for files in `users/`,
  and `--project` the one for `distro.toml`. With taplo, in the configuration's `.taplo.toml`:

  ```toml
  [[rule]]
  include = ["system.toml", "services/*.toml", "roles/*.toml"]
  schema.path = "./distro.schema.json"

  [[rule]]
  include = ["users/*.toml"]
  schema.path = "./users.schema.json"
  ```

- Each command is a call on a library, and later on a local daemon. The CLI, a GUI installer,
  editor plugins and a Talos-style remote API all use the same calls.

## For contributors

The commands that only make sense in this repository move under `distro dev`: `dev test`
(running scenarios, today's `check`), `dev status`, `dev bench`. `fill-templates` stays hidden;
activation runs it, and people don't.

`dev test DIR` checks each configuration in `DIR`. One with an `expected/` directory renders:
each file there must be what the configuration writes at that path, with units under
`etc/systemd/system/` and a user's files under `home/NAME/`. Any other case is a bad
configuration whose errors must match its `expected-error.txt`. `--bless` writes what they
actually give. The modules' own cases are in `modules/tests/`; start one with empty files at the
paths to check, then bless and review them.

## What's built

Built: `-C` and the system default, `status`, `plan`, `apply` (asking first), `try`, `keep`,
`history`, `diff`, `rollback`, `clean`, `build`, `apply PATH`, `run`, `shell`, `add`, `remove`,
`lock`, `update`, `sources`, `check`, `why` for options, files and packages, `options`,
`secret`, the `dev` commands, `--json` for `plan`, `status`, `history` and `sources`, and
`--role` for configurations with `roles/`, which defaults to the machine's role from install or
`machines.toml`. `apply` and `try` check `[machine.requires]` first. Projects: found from the
current directory, with `shell`, `up`, `run`, `add`, `remove`, `check`, `lock`, `update` and
`why`, and `distro.local.lock` for what only `distro.local.toml` adds.

A home of its own, `home.toml` in `~/.config/distro`, isn't supported for now: a user's
packages and files go under `[users.NAME]` in the system's configuration.

Not yet: versions (`jq@1.7`), `search`, `info`, `--json` elsewhere, and `--verbose`.
`clean` lists the generations it removes rather than estimating the space first, and says what it
freed afterwards.

## Open questions

- How a machine builds what it needs, or fetches it built, without a checkout.
- Whether `update PKG` moving one package without moving distro is worth what it costs: that
  package then comes from a different distro revision than the rest.
- How long `try` waits by default, and whether it rolls back when the machine loses the network.
