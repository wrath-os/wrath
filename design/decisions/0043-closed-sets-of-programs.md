# 0043: What runs on a machine calls only what it declares

Status: proposed, 2026-11-19.

## Context

Everything `wrath` does is Rust in this repository, including commands that decide nothing hard.
`wrath unlock enroll` reads `/etc/crypttab` and the model, then runs `systemd-cryptenroll` twice.
Changing how a machine unlocks, or adding a similar command for something else, means changing
wrath itself, in Rust, for what is a short script. The store, builder, plan, switch, boot entries
and secrets do need Rust: they have to be correct, survive interruption and be fast. Commands
that act on a running machine at someone's request mostly don't.

Whatever runs them, nothing today makes sure the programs they call are there:

- Units find programs on the system's PATH. `ExecStart = "ollama serve"` works because the module
  called `ctx.package("ollama")`. Syncthing's module keeps a shell script in a string and adds
  `curl` and `jq` to the system for it. Drop `jq` from the module and the script still finds it
  while another module adds it, then breaks when that module is turned off. Everything a script
  needs also lands on every user's PATH.
- wrath's own Rust runs programs by name: `cryptsetup`, `systemd-repart`, `mkfs.btrfs`, `age`,
  `ssh-keygen`, `tar`, `curl` and more. Its recipe declares none of them. They're there because
  the base system and the installer carry them, and on a development machine devenv does. Only
  the VM scenarios show they are. Switching is the exception: it runs `bootctl`, `systemd-creds`
  and `switch-to-configuration` from inside the generation it switches to, so they're in its
  closure.
- A script in a string gets no linting, highlighting or tests of its own.

How others make dependencies certain: substituting store paths into text, `${pkgs.jq}/bin/jq`,
makes each reference exact, but nothing checks that every reference was written that way, a bare
`jq` still works from PATH where it happens to be, and the text fills with paths. Wrapper scripts
that set PATH to a declared list keep scripts readable and give a closed set, and a mistake shows
up only when the script runs. Tools that parse shell to resolve its commands catch mistakes at
build time, for that one language. git and cargo find subcommands as programs named `git-foo` and
`cargo-foo`, so anyone can add one in any language.

Most glue scripts exist because a primitive is missing: making directories, fixing ownership,
generating a key on first start, pushing settings into a program that keeps its own.

## Decision

### Everything that runs gets a closed set of programs

Units, module commands, steps during a switch and wrath itself each declare the packages whose
programs they call, by name, as `ctx.package` names them and pinned by the lock the same way. What
runs gets a PATH of exactly those packages' `bin` directories, and the packages' own dependencies
come along through their store references, as they do for any package. Nothing falls back to the
system's PATH, so what a program calls is what it lists, on every machine.

Modules never write store paths. They name packages, and wrath turns the names into paths where
the launcher, the unit or wrath's binary needs them. Nothing in a closed set is added to users'
PATH; `ctx.package` stays the way to give users a program.

### Missing programs explain themselves

A closed PATH ends with one more directory, holding a stub for every program a package in the
source provides that isn't in the set. A stub runs nothing; it fails with what to add:

```text
error: unlock/enroll.sh ran `jq`, which isn't in its `uses`
  add "jq" to uses in modules/unlock.star:8 (the jq package provides it)
```

This needs no understanding of the language, so a shell script, a Python program and a Rust
binary from a recipe get the same message, in scenarios and on a machine. A name no package
provides fails as an unknown command does.

### Checked before it runs where that's cheap

- Shell scripts are parsed when the command or unit is built, POSIX sh and the bash features
  scripts use, likely with tree-sitter-bash. Every command a script calls is resolved against
  builtins, its own functions and its set. One that isn't there fails the build, pointing at the
  line and at the declaration. A call whose name is only known at run time, `$tool`, `eval`, or
  the program run by `xargs`, `find -exec` or `sudo`, is an error too, unless a comment says what
  it runs: `# wrath: calls jq`. A package in the set that nothing calls is a warning.
- wrath's Rust runs programs as `Command::new("literal")`. Its recipe lists them, the build puts
  their paths in the binary, and a test checks that every literal is in the list.
- Other languages get the stubs and their tests. wrath doesn't promise to understand them.

Shell is where nearly all glue is written, so it gets the most help without being the only
choice.

### Modules add commands

```python
ctx.command(
    "unlock enroll",
    run = "unlock/enroll.sh",
    uses = ["systemd", "cryptsetup", "jq"],
    reads = ["storage.unlock"],
    root = True,
    doc = "Let the TPM unlock the disks again, as the machine boots now.",
)
```

- `run` is a file beside the module in its source, such as `grapes/modules/unlock/enroll.sh`, so
  editors, linters and tests treat it as one. A program too big for a script is a package with a
  recipe, and `run = "@wrath-unlock/bin/wrath-unlock"` names a program in one.
- `uses` is its closed set. wrath builds the command into its own store object: the file and a
  launcher that sets PATH and runs it.
- `reads` names options. Their values are rendered as JSON
  when the system is built and the command gets the file's path in `WRATH_INPUT`. A command acts
  on the generation that's running, and never parses configuration files or has values pasted
  into its text.
- `root` says the command needs root. Run without it, wrath says to use `sudo` and runs nothing.
- `doc` is the one line `wrath --help` shows, under the module's name. The program handles its own
  arguments and `--help`; wrath passes everything after the command's name through.
- The system lists its commands in `/run/system/etc/wrath/commands.json`: name, module, doc, root
  and the launcher's path. `wrath unlock enroll` looks there when no built-in command matches.
- A command can't take a built-in command's name, or another module's: both are errors when the
  configuration is evaluated, pointing at the `ctx.command` call.
- A failing command's error names the module it came from, with its exit code.

Units take `uses` too, with the same launcher around their `Exec` lines.

### Fewer scripts

A script in a module is a sign of a missing primitive. Before a module adds one, it uses what
systemd has (`StateDirectory`, `LoadCredential`, tmpfiles, sysusers) or wrath grows the primitive.
Syncthing's script is the first case: pushing a desired state into a running program's API and
removing what isn't listed is a pattern more modules will need.

## Consequences

- New machine commands go in grapes, in any language a recipe builds, next to the module whose
  options they read. Contributing one means writing a script and a scenario.
- Turning a module off removes its commands with it.
- A forgotten dependency is a build error for shell and Rust, and a message saying what to add
  for everything else. It never depends on what other modules happen to install.
- wrath's Rust shrinks to what has to be careful. `unlock enroll` moves to grapes first: small,
  already covered by `native-tpm-enroll`, and easy to compare with the Rust version. It loses no
  guarantee by moving; it has none today.
- Services stop finding programs on the shared PATH, so every unit in grapes needs its `uses`.
  The stubs make the misses easy to find in scenarios.
- The stubs need an index of which package provides which program, which the source's recipes
  already give.
- Commands are as good as their scripts at errors. wrath can't point at the user's TOML from
  inside one, only say which module failed.
- Scenarios could record which programs each thing ran and compare that with what it declares.
  Untested branches would be missing, so that can only add to the checks above. Left for later.
- Running a module's command on another machine, `--host`, comes later.

## Work

1. The launcher: a closed PATH, the stubs, `WRATH_INPUT`. Tests that it sees only what it lists
   and that a stub's error names the package.
2. `ctx.command` in the module API, its checks (names, files, packages), and `commands.json` in
   the system. The CLI: unknown subcommands looked up there, `--help` listing them, `root`.
3. Move `unlock enroll` to a grapes module with its script; `native-tpm-enroll` passes unchanged.
4. The shell check at build time.
5. wrath's own programs: listed in its recipe, their paths in the binary, the literal check.
6. `uses` for units, module by module, and syncthing's script replaced by a primitive.
