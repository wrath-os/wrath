# 0026: What a system holds is decided in Starlark

Status: accepted, 2026-11-11.

## Context

Modules decide most of what a system holds: they write files, add packages and enable units.
The builder's Rust still decides about 550 lines' worth on its own:

- the initrd: which kernel modules it loads, its modprobe settings, which drivers wait for the
  root (the deny-list of drivers that read firmware), and its crypttab and fstab;
- the system's fstab, crypttab and swap;
- `passwd`, `group` and `shadow` from the accounts;
- the kernel command line;
- which firmware is linked into the system;
- how a unit is rendered, its bare program names resolved, and which units are enabled.

Each is something a person could reasonably want done differently, and each takes a change to
Rust today. The initrd deny-list is an example: it was first written to hold back every
module nothing named, which kept disk drivers from loading, and a fix needed a new build of the
tool.

Some of these choices are known at plan time, from the configuration alone. Others depend on
what was built: the deny-list reads the built kernel's modinfo, and resolving a unit's program
needs the files a package has. Modules run at plan time, before anything is built, so they
can't make the second kind of choice now.

## Decision

Rust keeps the mechanism, and Starlark makes the choices.

Rust keeps:

- the store, keys, sandbox, fetching, ELF handling and output splitting;
- writing archives, such as the initrd's cpio, and compressing them;
- parsing: modules.dep, modinfo and a package's file list are read in Rust and given to Starlark
  as plain values, as locks are for build styles (0025);
- activation, which runs as root on the machine and has to work whatever the configuration;
- checks that hold for every system, such as a unit that runs a program no package has.

Starlark gets, as modules in this repository:

- the initrd's contents, given the kernel's modules and their firmware as data;
- the fstab, crypttab and swap, from `disks`;
- the account files, from `users`;
- the kernel command line and the firmware a system links;
- rendering units and choosing which are enabled.

Choices that need what was built run as actions in the graph: a Starlark function with declared
inputs, such as the built kernel, evaluated in the hermetic evaluator styles use. Its result is
a record kept under the hashes of the function, its Starlark files and its inputs, so it runs
once per change and a plan with nothing changed only reads records. Choices known at plan time
stay in the module, as `disks` writing grow-root does.

Discipline:

- Each move produces the same bytes as before, so nothing rebuilds. That's the test of the move.
- The 0025 discipline holds: modules live in this repository, with no helpers of helpers and no
  override layer. A configuration changes them through their options. Whether it may replace one
  of these modules with its own is left open.
- A piece moves only when a module would own it. Rust that only writes a format, such as the
  cpio writer, stays.

Plan stays fast and says what changes. Nix's import-from-derivation lets evaluation wait on a
build, so its evaluation gets slow and a plan can't be made without building. Here:

- Plan never builds and never runs an action. The graph's shape, meaning which actions exist and
  what each reads, comes from the model and the recipes alone. An action's result can't add
  packages, units or other actions.
- An action that reads build output shows in the plan by its key: `initrd` rebuilt, because
  `linux` changed, with the tier that brings. What's inside it shows once it's built, in
  `distro diff`, as it does for packages now.
- Everything a module decides at plan time, from the configuration, shows in the plan as a file
  diff, as it does now. A choice moves into an action only when it needs build output.
- plan-budget holds: the Framework's warm plan stays under 20 ms, and each move is measured
  before and after.

## Consequences

- One evaluator serves build styles and these actions. This waits for the 0025 evaluator.
- The plan-time pieces moved first: account files and the kernel command line, with the same
  bytes as before. Firmware links list each firmware package's files, so they need build output
  and follow with the initrd and units, once actions can run Starlark.
- The deny-list, kernel parameters and unit rendering become readable and changeable without a
  new build of the tool, and their tests become module golden tests.
- Each kind of action evaluated in Starlark costs a lookup when cached and an evaluation when
  not. plan-budget gains a case for the initrd, cold and warm.
- What a module can read grows: built outputs, through declared inputs only. A module that reads
  something it didn't declare is an error, as for styles.
