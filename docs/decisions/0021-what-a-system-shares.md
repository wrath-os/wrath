# 0021: What a system shares

Status: accepted, 2026-11-11.

## Context

A generation's `share`, which `XDG_DATA_DIRS` names, merges the `share` of the packages a system
names (0016, 0020). Two kinds of data miss it. A library's data, such as GTK's schemas, is on the
machine because a program refers to the library, but isn't merged unless something names GTK.
Data nothing refers to, such as the desktop schemas the GTK portal reads by name, isn't even on
the machine unless named, because a package's run-time needs are found by scanning its files for
store paths (0014). So modules name libraries for their data, which users would have to know.

NixOS merges only the packages a system lists, and wraps each GTK program to name its
dependencies' data; unwrapped programs fail. A package can ask for others to be installed beside
it (`propagatedUserEnvPkgs`), and modules add packages for their data. Guix profiles take the
listed packages and their propagated inputs, then make caches from them with profile hooks.
Distributions with one `/usr` get it from installing: a library's data lands in `/usr/share`, and
`Depends` brings data packages.

## Decision

**A system shares the data of everything it runs.** `share` merges the packages the system names
and every package their run-time closure reaches: one whose programs or libraries something in
the closure refers to. Only `share` widens. Programs on the path are still the named packages',
and units and services still come from service packages. What you name gives you commands; what
they need comes along as data. Indexes (0020) read the same packages.

**A recipe declares what the scan can't see.** `runtime_deps = ["gsettings-desktop-schemas"]`
names packages a package needs at run time without referring to them. The builder writes their
store paths into the package, in `lib/distro/runtime-deps`, so the scan finds them like any other
reference: they're copied to machines, kept by garbage collection and merged into `share`. They're
part of the package's key.

**References are recorded once.** A store path never changes, so the paths it refers to are kept
beside the store, in the work directory's `refs`, the first time they're found, and the builder
finds them as it places each package. A closure is then a union of short lists, cheap enough for
every plan. A cache will carry them with the paths.

**Two files in one place stop the build.** With more packages merged, more of them can ship the
same file. Identical files are fine. Different ones stop the generation's build and name both
packages; the recipe that shouldn't ship the file removes it. When a configuration needs to choose
instead, it gets a way to.

## Consequences

- Modules stop naming libraries for their data. niri's names programs only.
- `share` gets bigger: drirc files, icons and data of libraries nobody named. It's links.
- A clash that went unnoticed under first-one-wins becomes an error, found when a system builds.
- The first plan after this scans what it hasn't seen once; later ones read the records.

## Alternatives

- Wrappers that set `XDG_DATA_DIRS` per program. Every program needs one, and one missing fails at
  run time.
- Data packages as dependencies of the module. Works, and puts on users and module authors what
  the package knows.
- Merging everything the system's packages were built with. Too wide: compilers and headers.
