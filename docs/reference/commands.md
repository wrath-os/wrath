<!-- Made by `wrath dev docs` from the code. Don't edit it by hand. -->

# Commands

## Options every command takes

- `-C, --config <DIR|URL>`: The configuration to act on: a directory, or a URL such as `github:owner/repo//dir`, read as it is [default: the system's, in /etc/wrath]
- `--role <NAME>`: The role to act as, for a configuration with kinds of machines in `roles/`
- `--json`: Print data as JSON instead of text, where a command supports it
- `-v, --verbose`: Show everything each build prints, as it prints it, and how long each step took
- `--source <NAME=DIR>`: A source read from a directory instead of where the lock pins it, for this command only, such as `grapes=../grapes` to work on grapes beside a configuration
- `--locked`: Write no lock: fail where the configuration's lock or a recipe's would change, as CI wants
- `--cache <URL>`: A binary cache to fetch builds from before building them: a directory or an http(s) URL
- `--cache-key <KEY>`: A key whose signature on what a cache holds is trusted, as `wrath cache key` printed it
- `--offline`: Ask no cache for anything: build what isn't here, and plan without saying what a cache has

## `wrath status`

Show what's running and whether it matches the configuration

```text
wrath status [OPTIONS]
```

## `wrath plan`

Show what would change going to the configuration from the running system

```text
wrath plan [OPTIONS]
```

- `--against <AGAINST>`: A configuration, or a `model.json`, to compare against instead of the running system
- `--fresh`: Compare against nothing, so everything is new
- `--host <NAME>`: Compare against what another machine runs, reached with `ssh NAME`

## `wrath apply`

Switch to the configuration, locking its packages first, and say when it takes effect

```text
wrath apply [OPTIONS] [SYSTEM]
```

- `[SYSTEM]`: A system that is already built, such as one `build` made for this machine elsewhere
- `--host <NAME>`: Build here and switch another machine, reached with `ssh NAME`
- `-y, --yes`: Switch without asking first
- `--watch`: Keep watching: try each save, by itself when it only changes settings, until Ctrl-C
- `--boot`: Make it the system booted next, without switching to it now
- `--automatic`: Upgrade from what the system follows, as `updates.switch` says, and keep how it went; what `wrath-upgrade.timer` runs

## `wrath try`

Switch to the configuration for a while, then back unless you `wrath keep` it

```text
wrath try [OPTIONS] [SYSTEM]
```

- `[SYSTEM]`: A system that is already built
- `--host <NAME>`: Build here and try it on another machine, reached with `ssh NAME`
- `--for <DURATION>`: How long to try it, such as 90s, 5m or 1h
- `-y, --yes`: Switch without asking first

## `wrath keep`

Keep the system being tried as the current generation

```text
wrath keep [OPTIONS]
```

## `wrath build`

Build the configuration's system, or the packages you name, and print where each is; `--role` builds another of the configuration's roles

```text
wrath build [OPTIONS] [PACKAGES]...
```

- `[PACKAGES]...`: Packages to build instead of the system, such as `jq`
- `--all`: Every package grapes has a recipe for, instead of the system, as a cache's builder does
- `--image`: A disk image of the system, to write to a USB stick or boot in a VM
- `-o, --link <PATH>`: Link what was built at this path, or in this directory under its own name; an image is moved there instead
- `--check`: Then run each package's smoke commands, and its own tests in a build of it thrown away
- `--kernel-option <KERNEL_OPTIONS>`: A kernel option for an image's boot entry, instead of the screen and serial consoles
- `--with-configuration`: Put the configuration in the image's /etc/wrath, so wrath on it can plan

## `wrath history`

Show the system's generations; with one, what changed in it, and with two, between them

```text
wrath history [OPTIONS] [GENERATION]...
```

- `[GENERATION]...`: A generation, or two to compare

## `wrath clean`

Remove old generations and what only they used, and what builds left behind, such as disk images

```text
wrath clean [OPTIONS]
```

- `--keep <KEEP>`: How many generations before the current one to keep
- `-y, --yes`: Remove them without asking first
- `--raw`: Also remove what packages still in use were built from before their fix-ups; a newer wrath then builds them again where it would have fixed them up again

## `wrath rollback`

Switch back to the previous generation, or to generation N

```text
wrath rollback [OPTIONS] [N]
```

- `[N]`: The generation, as `wrath history` numbers it

## `wrath run`

Run a package's program without installing it

```text
wrath run [OPTIONS] <PACKAGE> [ARGS]...
```

- `<PACKAGE>`: The package, such as `jq`
- `[ARGS]...`: Arguments for the program, after `--`

## `wrath up`

Run a project's services until you stop them

```text
wrath up [OPTIONS]
```

## `wrath shell`

Start a shell with packages, without installing them; in a project, with its packages and environment too

```text
wrath shell [OPTIONS] [PACKAGES]...
```

- `[PACKAGES]...`: The packages, such as `jq` and `ripgrep`

## `wrath add`

Add packages to the configuration and lock them

```text
wrath add [OPTIONS] <PACKAGES>...
```

- `<PACKAGES>...`: The packages, such as `jq` and `ripgrep`
- `--home <USER>`: Add them to this user's packages instead of the system's

## `wrath remove`

Remove packages from the configuration

```text
wrath remove [OPTIONS] <PACKAGES>...
```

- `<PACKAGES>...`: The packages, as the configuration names them
- `--home <USER>`: Remove them from this user's packages instead of the system's

## `wrath install`

Lay out a machine's disks and install the configuration on them, from a live system

```text
wrath install [OPTIONS]
```

- `--disk <DEVICE>`: A disk to install on, as `NAME=/dev/...` for the configuration's disk NAME, or just the device when it has one disk. Repeat it for a disk with several devices. Without it, the machine's disks are matched to what the configuration describes
- `--name <NAME>`: The machine's name, recorded on it
- `--passphrase-file <FILE>`: A file with the disks' passphrase, instead of asking for it
- `--identity <FILE>`: An age identity that decrypts the machine's keys under `secrets/machines/NAME`; without it, install asks for the passphrase `wrath secret machine` encrypted them to
- `--key-passphrase-file <FILE>`: A file with that passphrase, instead of asking for it
- `--target <TARGET>`: Where the new system is mounted while it's installed
- `--wipe`: Erase disks that have partitions already
- `--system <PATH>`: A system already built and in /wrath/store, to install instead of building one [default: the one an image of a configuration with disks carries to install]
- `-y, --yes`: Go ahead without asking

## `wrath update`

Move sources to their newest versions and lock them

```text
wrath update [OPTIONS] [SOURCES]...
```

- `[SOURCES]...`: Sources to update, such as `wrath` [default: all of them]
- `--list`: Only show each source: what it follows, where it's pinned and how old that is

## `wrath check`

Check the configuration for errors, without building it, and pin what's new in it

```text
wrath check [OPTIONS]
```

## `wrath options`

Show the options there are, with their types, defaults and docs

```text
wrath options [OPTIONS] [PREFIX]
```

- `[PREFIX]`: Only the options under this one, such as `services.nginx` or `users.john`
- `--schema`: Print them as a JSON Schema for editors, of the whole configuration or of a file whose options start at the prefix, such as `users.john` for `users/john.toml`
- `--project`: The options of a project, `wrath.toml`, instead of a system's

## `wrath why`

Show where an option's value, a file or a package comes from

```text
wrath why [OPTIONS] [OPTION]
```

- `--all`: Explain every option
- `[OPTION]`: An option or table of options, a file such as `/etc/nginx/main.conf`, or a package

## `wrath secret`

Edit secrets and encrypt them to the machine and the people who manage it

```text
wrath secret <COMMAND>
```

## `wrath secret edit`

Open a secret in $EDITOR and encrypt it again, or create it

```text
wrath secret edit [OPTIONS] <NAME>
```

- `<NAME>`: The secret, as declared under `[secrets.<name>]`
- `--identity <IDENTITY>`: A private key that decrypts the secrets [default: ~/.ssh/id_ed25519]

## `wrath secret rekey`

Encrypt every secret, and every machine's keys, again to the current recipients, such as after adding a machine

```text
wrath secret rekey [OPTIONS]
```

- `--identity <IDENTITY>`: A private key that decrypts the secrets [default: ~/.ssh/id_ed25519]

## `wrath secret machine`

Make the keys a machine doesn't have yet, kept encrypted in `secrets/machines/NAME`: its SSH host key, made before installing it so its secrets decrypt from its first boot, and its Secure Boot keys

```text
wrath secret machine [OPTIONS] <NAME>
```

- `<NAME>`: The machine, as `wrath install --name` or its hostname calls it
- `--passphrase-file <FILE>`: A file with the passphrase install asks for, instead of asking for it here

## `wrath unlock`

How the machine's encrypted disks unlock, such as enrolling its TPM again

```text
wrath unlock <COMMAND>
```

## `wrath unlock enroll`

Let the TPM unlock the disks again, as the machine boots now: it asks for the disks' passphrase and replaces their TPM key, after the boot measured differently and asked

```text
wrath unlock enroll [OPTIONS]
```

## `wrath cache`

Sign and push builds to a binary cache, which machines then fetch from

```text
wrath cache <COMMAND>
```

## `wrath cache key`

Make a key to sign builds with, and print the public key machines trust it by

```text
wrath cache key [OPTIONS] <NAME> <FILE>
```

- `<NAME>`: The key's name, which a cache files what it signs under, such as `home-1`
- `<FILE>`: Where to keep the key, which only whoever pushes should be able to read
- `--url <URL>`: Where machines will fetch from, for the configuration this prints

## `wrath cache check`

Say where a package would come from: the store, a cache, or a build, and why

```text
wrath cache check [OPTIONS] <PACKAGE>
```

- `<PACKAGE>`: The package, as `wrath build` names it

## `wrath cache push`

Push everything built here to a cache, signed, that it doesn't have yet

```text
wrath cache push [OPTIONS] <CACHE>
```

- `<CACHE>`: The cache: a directory
- `--key <KEY>`: The key to sign with, as `wrath cache key` made it

## `wrath cache serve`

Serve a cache in a directory over HTTP, for machines on the network to fetch from

```text
wrath cache serve [OPTIONS] <CACHE>
```

- `<CACHE>`: The cache: a directory `wrath cache push` filled
- `--listen <LISTEN>`: The address and port to listen on

## `wrath dev`

Commands for working on wrath itself

```text
wrath dev <COMMAND>
```

## `wrath dev test`

Run scenario files, or compare bad configs with their golden errors

```text
wrath dev test [OPTIONS] [FILES]...
```

- `[FILES]...`: Scenario files to run, or directories of bad configs to compare with their golden errors
- `--determinism [<DIR>]`: Check that the modules in this directory [default: the set in use's] render the fixture configs byte for byte the same across fresh interpreters and module orders
- `--configs <CONFIGS>`: Configurations for --determinism
- `--bless`: Write the actual errors of bad configs as their expected errors
- `--image <IMAGE>`: Run VM scenarios against this built image instead of building one
- `--run-reproduction`: After a failure, run the printed reproduction and report whether it fails the same way

## `wrath dev acceptance`

Run the acceptance scenarios and show which pass

```text
wrath dev acceptance [OPTIONS]
```

- `--milestone <MILESTONE>`: Only run the scenarios of this milestone, for example `m2`

## `wrath dev docs`

Write the reference pages in docs/reference from the code: commands, options, module API

```text
wrath dev docs [OPTIONS]
```

- `--check`: Fail when a page differs from what the code says, instead of writing it

## `wrath dev lint`

Point out recipes that write a target's name, such as `x86_64`, where a placeholder says it, and record what each recipe's style makes where its lock doesn't yet

```text
wrath dev lint [OPTIONS] [PACKAGES]...
```

- `--redundant`: Also list dependencies another listed one already brings, for information; this builds what isn't built yet
- `[PACKAGES]...`: With `--redundant`, only these recipes [default: every recipe]

## `wrath dev repro`

Build packages a second time, as another user on fewer cores, and say which come out different [default: every recipe, writing a report to build/results]

```text
wrath dev repro [OPTIONS] [PACKAGES]...
```

- `[PACKAGES]...`: Only these recipes

## `wrath dev seed`

Work on the seed, the prebuilt compilers every build starts from

```text
wrath dev seed <COMMAND>
```

## `wrath dev seed check`

Build the toolchain and check it stands on its own: nothing refers to the seed, and a C and a C++ program run without /usr

```text
wrath dev seed check [OPTIONS]
```

## `wrath dev seed pack`

Pack the toolchain and what it needs into a new seed, and print its hash for seed.pin

```text
wrath dev seed pack [OPTIONS]
```

## `wrath dev seed addresses`

Print where every package lands, to compare what two seeds build

```text
wrath dev seed addresses [OPTIONS]
```

## `wrath dev outdated`

Compare each recipe's version with upstream's newest release

```text
wrath dev outdated [OPTIONS]
```

- `--all`: List every recipe, including those that are current or couldn't be checked

## `wrath dev keys`

Print every recipe's key, as `ID KEY` lines, to compare what a change to recipes or the builder rebuilds

```text
wrath dev keys [OPTIONS]
```

## `wrath dev model`

Print the configuration's model as JSON

```text
wrath dev model [OPTIONS]
```

## `wrath dev bench`

Measure how long `plan` takes with many synthetic modules

```text
wrath dev bench [OPTIONS]
```

- `--synthetic <SYNTHETIC>`: How many synthetic service modules to add
- `--runs <RUNS>`: How many times to measure
- `--budget-ms <BUDGET_MS>`: The most the median plan may take, in milliseconds
