<!-- Made by `distro dev docs` from the code. Don't edit it by hand. -->

# Commands

## Options every command takes

- `-C, --config <DIR>`: The configuration to act on [default: the system's, in /etc/distro]
- `--role <NAME>`: The role to act as, for a configuration with kinds of machines in `roles/`
- `--json`: Print data as JSON instead of text, where a command supports it
- `-v, --verbose`: Show everything each build prints, as it prints it

## `distro status`

Show what's running and whether it matches the configuration

```text
distro status [OPTIONS]
```

## `distro plan`

Show what would change going to the configuration from the running system

```text
distro plan [OPTIONS]
```

- `--against <AGAINST>`: A configuration, or a `model.json`, to compare against instead of the running system
- `--fresh`: Compare against nothing, so everything is new
- `--host <NAME>`: Compare against what another machine runs, reached with `ssh NAME`

## `distro apply`

Switch to the configuration, locking its packages first, and say when it takes effect

```text
distro apply [OPTIONS] [SYSTEM]
```

- `[SYSTEM]`: A system that is already built, such as one `build` made for this machine elsewhere
- `--host <NAME>`: Build here and switch another machine, reached with `ssh NAME`
- `-y, --yes`: Switch without asking first

## `distro try`

Switch to the configuration for a while, then back unless you `distro keep` it

```text
distro try [OPTIONS] [SYSTEM]
```

- `[SYSTEM]`: A system that is already built
- `--host <NAME>`: Build here and try it on another machine, reached with `ssh NAME`
- `--for <DURATION>`: How long to try it, such as 90s, 5m or 1h
- `-y, --yes`: Switch without asking first

## `distro keep`

Keep the system being tried as the current generation

```text
distro keep [OPTIONS]
```

## `distro build`

Build the configuration's system, or the packages you name, and print where each is; `--role` builds another of the configuration's roles

```text
distro build [OPTIONS] [PACKAGES]...
```

- `[PACKAGES]...`: Packages to build instead of the system, such as `jq`
- `--image`: A disk image of the system, to write to a USB stick or boot in a VM
- `-o, --link <PATH>`: Link what was built at this path, or in this directory under its own name
- `--check`: Then run each package's smoke commands, and its own tests in a build of it thrown away
- `--kernel-option <KERNEL_OPTIONS>`: A kernel option for an image's boot entry, instead of the screen and serial consoles
- `--with-configuration`: Put the configuration in the image's /etc/distro, so distro on it can plan

## `distro history`

Show the system's generations; with one, what changed in it, and with two, between them

```text
distro history [OPTIONS] [GENERATION]...
```

- `[GENERATION]...`: A generation, or two to compare

## `distro clean`

Remove old generations and what only they used

```text
distro clean [OPTIONS]
```

- `--keep <KEEP>`: How many generations before the current one to keep
- `-y, --yes`: Remove them without asking first

## `distro rollback`

Switch back to the previous generation, or to generation N

```text
distro rollback [OPTIONS] [N]
```

- `[N]`: The generation, as `distro history` numbers it

## `distro run`

Run a package's program without installing it

```text
distro run [OPTIONS] <PACKAGE> [ARGS]...
```

- `<PACKAGE>`: The package, such as `jq`, `stable#jq` or `github:jqlang/jq#jq`
- `[ARGS]...`: Arguments for the program, after `--`

## `distro up`

Run a project's services until you stop them

```text
distro up [OPTIONS]
```

## `distro shell`

Start a shell with packages, without installing them; in a project, with its packages and environment too

```text
distro shell [OPTIONS] [PACKAGES]...
```

- `[PACKAGES]...`: The packages, such as `jq`, `stable#jq` or `github:jqlang/jq#jq`

## `distro add`

Add packages to the configuration and lock them

```text
distro add [OPTIONS] <PACKAGES>...
```

- `<PACKAGES>...`: The packages, such as `jq`, `stable#jq` or `github:jqlang/jq#jq`
- `--home <USER>`: Add them to this user's packages instead of the system's

## `distro remove`

Remove packages from the configuration

```text
distro remove [OPTIONS] <PACKAGES>...
```

- `<PACKAGES>...`: The packages, as the configuration names them
- `--home <USER>`: Remove them from this user's packages instead of the system's

## `distro install`

Lay out a machine's disks and install the configuration on them, from a live system

```text
distro install [OPTIONS]
```

- `--disk <DEVICE>`: A disk to install on, as `NAME=/dev/...` for the configuration's disk NAME, or just the device when it has one disk. Repeat it for a disk with several devices. Without it, the machine's disks are matched to what the configuration describes
- `--name <NAME>`: The machine's name, recorded on it
- `--passphrase-file <FILE>`: A file with the disks' passphrase, instead of asking for it
- `--identity <FILE>`: An age identity that decrypts the machine's keys under `secrets/machines/NAME`
- `--target <TARGET>`: Where the new system is mounted while it's installed
- `--wipe`: Erase disks that have partitions already
- `--system <PATH>`: A system already built and in /store, to install instead of building one
- `-y, --yes`: Go ahead without asking

## `distro update`

Move sources to their newest versions and lock them

```text
distro update [OPTIONS] [SOURCES]...
```

- `[SOURCES]...`: Sources to update, such as `distro` [default: all of them]
- `--list`: Only show each source: what it follows, where it's pinned and how old that is

## `distro check`

Check the configuration for errors, without building it, and pin what's new in it

```text
distro check [OPTIONS]
```

- `--locked`: Fail when something isn't pinned yet instead of pinning it, as CI wants

## `distro options`

Show the options there are, with their types, defaults and docs

```text
distro options [OPTIONS] [PREFIX]
```

- `[PREFIX]`: Only the options under this one, such as `services.nginx` or `users.john`
- `--schema`: Print them as a JSON Schema for editors, of the whole configuration or of a file whose options start at the prefix, such as `users.john` for `users/john.toml`
- `--project`: The schema of a project, `distro.toml`, instead of a system

## `distro why`

Show where an option's value, a file or a package comes from

```text
distro why [OPTIONS] [OPTION]
```

- `--all`: Explain every option
- `[OPTION]`: An option or table of options, a file such as `/etc/nginx/main.conf`, or a package

## `distro secret`

Edit secrets and encrypt them to the machine and the people who manage it

```text
distro secret <COMMAND>
```

## `distro secret edit`

Open a secret in $EDITOR and encrypt it again, or create it

```text
distro secret edit [OPTIONS] <NAME>
```

- `<NAME>`: The secret, as declared under `[secrets.<name>]`
- `--identity <IDENTITY>`: A private key that decrypts the secrets [default: ~/.ssh/id_ed25519]

## `distro secret rekey`

Encrypt every secret again to the current recipients, such as after adding a machine

```text
distro secret rekey [OPTIONS]
```

- `--identity <IDENTITY>`: A private key that decrypts the secrets [default: ~/.ssh/id_ed25519]

## `distro dev`

Commands for working on distro itself

```text
distro dev <COMMAND>
```

## `distro dev test`

Run scenario files, or compare bad configs with their golden errors

```text
distro dev test [OPTIONS] [FILES]...
```

- `[FILES]...`: Scenario files to run, or directories of bad configs to compare with their golden errors
- `--determinism <DETERMINISM>`: Check that the modules in this directory render the fixture configs byte for byte the same across fresh interpreters and module orders
- `--configs <CONFIGS>`: Configurations for --determinism
- `--bless`: Write the actual errors of bad configs as their expected errors
- `--image <IMAGE>`: Run VM scenarios against this built image instead of building one
- `--run-reproduction`: After a failure, run the printed reproduction and report whether it fails the same way

## `distro dev acceptance`

Run the acceptance scenarios and show which pass

```text
distro dev acceptance [OPTIONS]
```

- `--milestone <MILESTONE>`: Only run the scenarios of this milestone, for example `m2`

## `distro dev docs`

Write the reference pages in docs/reference from the code: commands, options, module API

```text
distro dev docs [OPTIONS]
```

- `--check`: Fail when a page differs from what the code says, instead of writing it

## `distro dev lint`

Point out what recipes list that they don't need to, such as a dependency another one brings

```text
distro dev lint [OPTIONS] [PACKAGES]...
```

- `[PACKAGES]...`: Only these recipes [default: every recipe, which builds what isn't built yet]

## `distro dev repro`

Build packages a second time, as another user on fewer cores, and say which come out different [default: every recipe, writing a report to build/results]

```text
distro dev repro [OPTIONS] [PACKAGES]...
```

- `[PACKAGES]...`: Only these recipes

## `distro dev seed`

Work on the seed, the prebuilt compilers every build starts from

```text
distro dev seed <COMMAND>
```

## `distro dev seed check`

Build the toolchain and check it stands on its own: nothing refers to the seed, and a C and a C++ program run without /usr

```text
distro dev seed check [OPTIONS]
```

## `distro dev seed pack`

Pack the toolchain and what it needs into a new seed, and print its hash for seed.pin

```text
distro dev seed pack [OPTIONS]
```

## `distro dev seed addresses`

Print where every package lands, to compare what two seeds build

```text
distro dev seed addresses [OPTIONS]
```

## `distro dev outdated`

Compare each recipe's version with upstream's newest release

```text
distro dev outdated [OPTIONS]
```

- `--all`: List every recipe, including those that are current or couldn't be checked

## `distro dev model`

Print the configuration's model as JSON

```text
distro dev model [OPTIONS]
```

## `distro dev bench`

Measure how long `plan` takes with many synthetic modules

```text
distro dev bench [OPTIONS]
```

- `--synthetic <SYNTHETIC>`: How many synthetic service modules to add
- `--runs <RUNS>`: How many times to measure
- `--budget-ms <BUDGET_MS>`: The most the median plan may take, in milliseconds
